//! The Calls page: open call steps and calls made, a token for calling from the browser, logging what happened, and the
//! webhooks Twilio posts for calls: the TwiML request when the browser dials (/api/twilio/voice), and the status, recording
//! and transcription callbacks. The rules are in crate::calls.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::texts::{param, verified};
use super::util_input;
use super::util_scope;
use crate::auth::Auth;
use crate::calls;
use crate::error::{AppError, AppResult};
use crate::models::{Call, Lead, Ts};
use crate::ownership::{self, OwnerFilter};
use crate::state::AppState;
use crate::twilio::{Account, Client};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/calls", get(list))
        .route("/calls/{id}/dial", post(dial))
        .route("/calls/{id}/log", post(log))
        .route("/calls/{id}/skip", post(skip))
        .route("/calls/{id}/recording", get(recording))
        .route("/twilio/voice", post(voice))
        .route("/twilio/calls/status", post(status_callback))
        .route("/twilio/calls/recording", post(recording_callback))
        .route("/twilio/calls/transcription", post(transcription_callback))
}

#[derive(FromRow)]
struct Joined {
    #[sqlx(flatten)]
    call: Call,
    lead_name: String,
    lead_title: String,
    lead_company: String,
    lead_phone: String,
    lead_country: String,
    owner_name: Option<String>,
    called_by_name: Option<String>,
}

#[derive(Serialize)]
pub struct CallRow {
    #[serde(flatten)]
    pub call: Call,
    pub lead_name: String,
    pub lead_title: String,
    pub lead_company: String,
    pub lead_phone: String,
    pub owner_name: Option<String>,
    pub called_by_name: Option<String>,
    /// overdue (open and due before today), today (open) or done (logged, skipped or cancelled in the last 14 days).
    pub bucket: &'static str,
    /// The lead's time zone and the time there now, like "14:05".
    pub time_zone: String,
    pub local_time: String,
    /// Why now is outside their calling hours, or None.
    pub outside_hours: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    /// open, done, or blank for both.
    pub status: Option<String>,
    /// Mine, unassigned, mine_or_unassigned, everyone or a member's id. See ownership::OwnerFilter.
    pub owner: Option<String>,
}

/// Open calls, overdue first, then the ones made in the last 14 days, newest first. Open calls of leads who have since
/// replied, booked or gone on the do-not-contact list are cancelled first, so none of them shows as one to make.
pub async fn rows(db: &PgPool, project_id: Uuid, me: Option<Uuid>, q: &ListQuery) -> AppResult<Vec<CallRow>> {
    let owner = OwnerFilter::parse(q.owner.as_deref(), me)?;
    let status = q.status.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(s) = status {
        util_input::one_of("status", s, &["open", "done"])?;
    }
    calls::cancel_stale(db, Some(project_id)).await?;
    let found = sqlx::query_as::<_, Joined>(&format!(
        "select c.*, l.full_name as lead_name, l.title as lead_title, l.company_name as lead_company, l.phone as lead_phone,
                l.country_code as lead_country, coalesce(nullif(o.name, ''), o.email::text) as owner_name,
                coalesce(nullif(b.name, ''), b.email::text) as called_by_name
         from calls c join leads l on l.id = c.lead_id
         left join users o on o.id = c.owner_id left join users b on b.id = c.called_by
         where c.project_id = $1 and (c.status = 'open' or c.closed_at > now() - interval '14 days')
           and ($2::text is null or ($2 = 'open') = (c.status = 'open')) and {}
         order by c.status <> 'open', case when c.status = 'open' then c.due_at end, c.closed_at desc limit 300",
        ownership::sql("c", 3, 4)
    ))
    .bind(project_id)
    .bind(status)
    .bind(owner.mode())
    .bind(owner.user())
    .fetch_all(db)
    .await?;
    let project_tz: String = sqlx::query_scalar("select timezone from project_settings where project_id = $1")
        .bind(project_id)
        .fetch_optional(db)
        .await?
        .unwrap_or_else(|| "UTC".into());
    let now = Utc::now();
    let tz: Tz = project_tz.parse().unwrap_or(Tz::UTC);
    let midnight = now.with_timezone(&tz).date_naive().and_hms_opt(0, 0, 0).expect("midnight exists");
    let today = tz.from_local_datetime(&midnight).earliest().map_or(now, |d| d.with_timezone(&Utc));
    Ok(found
        .into_iter()
        .map(|j| {
            let zone = calls::zone(&j.lead_phone, &j.lead_country, &project_tz);
            let bucket = match j.call.status.as_str() {
                "open" if j.call.due_at < today => "overdue",
                "open" => "today",
                _ => "done",
            };
            CallRow {
                bucket,
                time_zone: zone.name().to_string(),
                local_time: now.with_timezone(&zone).format("%H:%M").to_string(),
                outside_hours: calls::outside_hours(zone, now),
                call: j.call,
                lead_name: j.lead_name,
                lead_title: j.lead_title,
                lead_company: j.lead_company,
                lead_phone: j.lead_phone,
                owner_name: j.owner_name,
                called_by_name: j.called_by_name,
            }
        })
        .collect())
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(q): Query<ListQuery>) -> AppResult<Json<Vec<CallRow>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(rows(&state.db, id, Some(auth.user_id), &q).await?))
}

#[derive(Serialize)]
struct DialToken {
    token: String,
    identity: String,
    to: String,
    expires_at: Ts,
}

/// A token the Voice SDK places this one call with, after the same checks the TwiML request makes. A refusal, like a call
/// outside the lead's calling hours, is a 409 with the reason to show.
async fn dial(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<DialToken>> {
    let call: Call = util_scope::load(&state.db, "calls", id, auth.org_id).await?;
    let now = Utc::now();
    let dial = calls::authorize(&state.db, auth.org_id, auth.user_id, call.id, None, now).await?.map_err(AppError::Conflict)?;
    let secret = crate::crypto::decrypt(&state.cfg.app_secret, &dial.api_secret_enc)?;
    let identity = calls::identity(auth.user_id);
    let token = calls::access_token(&dial.account_sid, &dial.api_key_sid, &secret, &dial.twiml_app_sid, &identity, now.timestamp());
    Ok(Json(DialToken { token, identity, to: dial.lead.phone, expires_at: now + chrono::Duration::seconds(calls::TOKEN_SECS) }))
}

#[derive(Deserialize)]
pub struct LogBody {
    pub outcome: String,
    #[serde(default)]
    pub notes: String,
}

/// Checks a logged outcome and notes, shared with the API.
pub fn check_log(body: &LogBody) -> AppResult<&str> {
    util_input::one_of("outcome", &body.outcome, calls::OUTCOMES)?;
    Ok(util_input::notes(Some(&body.notes))?.unwrap_or_default())
}

async fn log(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<LogBody>) -> AppResult<Json<Call>> {
    let call: Call = util_scope::load(&state.db, "calls", id, auth.org_id).await?;
    let notes = check_log(&body)?;
    let logged = calls::log(&state, call.id, &body.outcome, notes, Some(auth.user_id)).await?;
    Ok(Json(logged.ok_or_else(|| AppError::Conflict("This call was already logged, skipped or cancelled".into()))?))
}

/// Skips an open call, and the sequence goes on to its next step.
async fn skip(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Call>> {
    let call: Call = util_scope::load(&state.db, "calls", id, auth.org_id).await?;
    let skipped = sqlx::query_as::<_, Call>(
        "update calls set status = 'skipped', closed_at = now(), cancel_reason = 'Skipped by hand', called_by = coalesce(called_by, $2)
         where id = $1 and status = 'open' returning *",
    )
    .bind(call.id)
    .bind(auth.user_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(Json(skipped.ok_or_else(|| AppError::Conflict("This call was already logged, skipped or cancelled".into()))?))
}

/// The call's recording, fetched from Twilio with the organization's API key.
async fn recording(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<impl IntoResponse> {
    let call: Call = util_scope::load(&state.db, "calls", id, auth.org_id).await?;
    if call.recording_sid.is_empty() {
        return Err(AppError::NotFound);
    }
    let account = Account::of_org(&state.db, auth.org_id).await?.ok_or_else(|| AppError::BadRequest("Connect Twilio to play recordings".into()))?;
    let audio = Client::for_account(&state, &account)?.recording(&call.recording_sid).await.map_err(|e| AppError::BadRequest(e.reason().into()))?;
    Ok(([(header::CONTENT_TYPE, "audio/mpeg"), (header::CACHE_CONTROL, "private, max-age=3600")], audio))
}

fn twiml(body: String) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/xml")], body)
}

/// What the TwiML App asks when the browser places a call: the call to make, and the number in `To`. The caller is the team
/// member whose token placed it. Anything the checks turn down is said to the caller, since an error status would only get
/// Twilio's own "application error".
async fn voice(State(state): State<AppState>, headers: HeaderMap, Form(params): Form<Vec<(String, String)>>) -> AppResult<impl IntoResponse> {
    let org_id = verified(&state, &headers, &params, &calls::voice_url(&state.cfg.public_url)).await?;
    let (Some(user), Ok(call_id)) = (calls::caller(param(&params, "From")), param(&params, "call_id").parse::<Uuid>()) else {
        return Ok(twiml(calls::twiml_refuse("Start calls from the Calls page in Splee.")));
    };
    let dial = match calls::authorize(&state.db, org_id, user, call_id, Some(param(&params, "To")), Utc::now()).await? {
        Ok(dial) => dial,
        Err(why) => return Ok(twiml(calls::twiml_refuse(&why))),
    };
    // One Twilio call per task, since the callbacks find the task by its CallSid. The same CallSid again is Twilio retrying.
    let placed = sqlx::query(
        "update calls set twilio_sid = $2, called_by = $3, called_at = now(), to_number = $4, call_status = 'ringing'
         where id = $1 and (twilio_sid is null or twilio_sid = $2)",
    )
    .bind(dial.call.id)
    .bind(param(&params, "CallSid"))
    .bind(user)
    .bind(&dial.lead.phone)
    .execute(&state.db)
    .await?;
    if placed.rows_affected() == 0 {
        return Ok(twiml(calls::twiml_refuse("This call was already made. Log how it went on the Calls page first.")));
    }
    Ok(twiml(calls::twiml_dial(&state.cfg.public_url, &dial.from, &dial.lead.phone, dial.transcribe)))
}

async fn status_callback(State(state): State<AppState>, headers: HeaderMap, Form(params): Form<Vec<(String, String)>>) -> AppResult<StatusCode> {
    let org_id = verified(&state, &headers, &params, &calls::status_url(&state.cfg.public_url)).await?;
    let parent = param(&params, "ParentCallSid");
    let parent = if parent.is_empty() { param(&params, "CallSid") } else { parent };
    let duration = param(&params, "CallDuration").parse().ok();
    calls::on_status(&state.db, org_id, parent, param(&params, "CallStatus"), duration).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn recording_callback(State(state): State<AppState>, headers: HeaderMap, Form(params): Form<Vec<(String, String)>>) -> AppResult<StatusCode> {
    let org_id = verified(&state, &headers, &params, &calls::recording_url(&state.cfg.public_url)).await?;
    if param(&params, "RecordingStatus") == "completed" {
        let secs = param(&params, "RecordingDuration").parse().ok();
        let (call, sid, url) = (param(&params, "CallSid"), param(&params, "RecordingSid"), param(&params, "RecordingUrl"));
        calls::on_recording(&state.db, org_id, call, sid, url, secs).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn transcription_callback(State(state): State<AppState>, headers: HeaderMap, Form(params): Form<Vec<(String, String)>>) -> AppResult<StatusCode> {
    let org_id = verified(&state, &headers, &params, &calls::transcription_url(&state.cfg.public_url)).await?;
    calls::on_transcription(&state.db, org_id, param(&params, "CallSid"), param(&params, "TranscriptionEvent"), &params).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// For the API and MCP: logs a call with the lead, closing their open call if they have one.
pub async fn log_for_lead(state: &AppState, org_id: Uuid, lead_id: Uuid, body: &LogBody) -> AppResult<Value> {
    let lead: Lead = util_scope::load(&state.db, "leads", lead_id, org_id).await?;
    let notes = check_log(body)?;
    let open: Option<Uuid> = sqlx::query_scalar("select id from calls where lead_id = $1 and status = 'open' order by due_at limit 1")
        .bind(lead.id)
        .fetch_optional(&state.db)
        .await?;
    let logged = match open {
        Some(id) => calls::log(state, id, &body.outcome, notes, None).await?,
        None => None,
    };
    let call = match logged {
        Some(call) => call,
        None => calls::log_manual(state, &lead, &body.outcome, notes, None).await?,
    };
    Ok(json!(call))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use chrono::Timelike;
    use reqwest::Method;

    use super::*;
    use crate::config::Config;
    use crate::crypto::{self, sha256_hex};
    use crate::twilio::{self, fake};

    const PUBLIC: &str = "https://splee.test";
    /// A US number, so Sam is called in the project's time zone.
    const SAM: &str = "+14155550134";
    const KEY: &str = "splee_test_calls";

    /// The fixed-offset zone where it's `hour` o'clock now, so calling hours don't depend on when the tests run.
    fn zone_at(hour: i64) -> String {
        let mut offset = hour - Utc::now().hour() as i64;
        if offset > 14 {
            offset -= 24;
        } else if offset < -12 {
            offset += 24;
        }
        // Etc zones have the sign backwards: Etc/GMT-3 is three hours ahead of UTC.
        if offset >= 0 { format!("Etc/GMT-{offset}") } else { format!("Etc/GMT+{}", -offset) }
    }

    struct Seeded {
        state: AppState,
        base: String,
        client: reqwest::Client,
        sent: fake::Sent,
        acme_org: Uuid,
        project: Uuid,
        /// Signed in as session "jo", on Acme's team. Sam's owner.
        jo: Uuid,
        beta_user: Uuid,
        beta_project: Uuid,
        /// A contacted lead whose first email went out 3 days ago, so their call step, 2 days after it, is due.
        sam: Uuid,
        beta_lead: Uuid,
    }

    impl Seeded {
        /// Acme has Twilio set up for calls with the fake's account, its TwiML App and transcription on, and a project whose
        /// first follow-up is a call 2 days after the first email, then an email 3 days after that. It's midday for Sam.
        async fn start(db: PgPool) -> Self {
            let (fake_url, sent) = fake::start().await;
            let base_cfg = (*crate::autopilot::tests::state(db.clone()).cfg).clone();
            let cfg = Config { app_secret: "test-secret".into(), public_url: PUBLIC.into(), twilio_api_url: fake_url, ..base_cfg };
            let state = AppState { db: db.clone(), cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(1).0 };
            let user = |email: &'static str| sqlx::query_scalar::<_, Uuid>("insert into users (email, password_hash) values ($1, 'x') returning id").bind(email).fetch_one(&db);
            let session = |user: Uuid, org: Uuid, name: &'static str| {
                sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                    .bind(sha256_hex(name))
                    .bind(user)
                    .bind(org)
                    .execute(&db)
            };
            let mut orgs = vec![];
            for name in ["acme", "beta"] {
                let org: Uuid = sqlx::query_scalar("insert into organizations (name, referral_code) values ($1, $1) returning id").bind(name).fetch_one(&db).await.unwrap();
                let owner = user(if name == "acme" { "acme@example.test" } else { "beta@example.test" }).await.unwrap();
                sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(owner).execute(&db).await.unwrap();
                session(owner, org, name).await.unwrap();
                let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Fieldnote') returning id").bind(org).fetch_one(&db).await.unwrap();
                sqlx::query(
                    "insert into project_settings (project_id, paused, timezone, send_days, send_start_hour, send_end_hour, follow_ups)
                     values ($1, false, $2, '{1,2,3,4,5,6,7}', 0, 24,
                             '[{\"kind\": \"call\", \"delay_days\": 2, \"enabled\": true, \"instructions\": \"Ask about the new clinic\"},
                               {\"delay_days\": 3, \"enabled\": true, \"instructions\": \"\"}]')",
                )
                .bind(project)
                .bind(zone_at(12))
                .execute(&db)
                .await
                .unwrap();
                orgs.push((org, owner, project));
            }
            let [(acme_org, _, project), (_, beta_user, beta_project)] = [orgs[0], orgs[1]];
            let jo = user("jo@example.test").await.unwrap();
            sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(acme_org).bind(jo).execute(&db).await.unwrap();
            session(jo, acme_org, "jo").await.unwrap();
            sqlx::query(
                "insert into twilio_accounts (org_id, account_sid, api_key_sid, api_secret_enc, auth_token_enc, sms_from, twiml_app_sid, call_transcription)
                 values ($1, $2, $3, $4, $5, '+14155550100', $6, true)",
            )
            .bind(acme_org)
            .bind(fake::ACCOUNT)
            .bind(fake::KEY)
            .bind(crypto::encrypt("test-secret", "secret"))
            .bind(crypto::encrypt("test-secret", "token"))
            .bind(fake::APP)
            .execute(&db)
            .await
            .unwrap();
            sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'dialer', 'splee_test_', $2)")
                .bind(acme_org)
                .bind(sha256_hex(KEY))
                .execute(&db)
                .await
                .unwrap();
            let sam = emailed_lead(&db, project, "Sam", SAM, Some(jo)).await;
            let beta_lead = emailed_lead(&db, beta_project, "Bo", SAM, None).await;

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", crate::routes::router()).with_state(state.clone());
            tokio::spawn(async move { axum::serve(listener, app).await });
            Self { state, base, client: reqwest::Client::new(), sent, acme_org, project, jo, beta_user, beta_project, sam, beta_lead }
        }

        fn db(&self) -> &PgPool {
            &self.state.db
        }

        async fn call(&self, session: &str, method: Method, path: &str, body: Value) -> (u16, Value) {
            let res = self.client.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session={session}")).json(&body).send().await.unwrap();
            (res.status().as_u16(), res.json().await.unwrap_or_default())
        }

        async fn api(&self, method: Method, path: &str, body: Value) -> (u16, Value) {
            let res = self.client.request(method, format!("{}{path}", self.base)).bearer_auth(KEY).json(&body).send().await.unwrap();
            (res.status().as_u16(), res.json().await.unwrap_or_default())
        }

        /// Posts form `params` to a Twilio webhook under /api/twilio, signed with `token` for the URL Twilio was given.
        async fn webhook(&self, path: &str, params: &[(&str, &str)], token: &str) -> (u16, String) {
            let params: Vec<(String, String)> = params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            let signed = format!("{PUBLIC}/api/twilio/{path}");
            let res = self
                .client
                .post(format!("{}/twilio/{path}", self.base))
                .header("x-twilio-signature", twilio::signature(token, &signed, &params))
                .form(&params)
                .send()
                .await
                .unwrap();
            (res.status().as_u16(), res.text().await.unwrap())
        }

        /// What Twilio asks when `user`'s browser dials `to` for `call`.
        async fn voice(&self, call: Uuid, user: Uuid, to: &str, sid: &str) -> (u16, String) {
            let from = format!("client:{}", calls::identity(user));
            let call = call.to_string();
            let params = [("AccountSid", fake::ACCOUNT), ("ApplicationSid", fake::APP), ("CallSid", sid), ("From", from.as_str()), ("To", to), ("call_id", call.as_str())];
            self.webhook("voice", &params, "token").await
        }

        async fn open_call(&self, lead: Uuid) -> Uuid {
            sqlx::query_scalar("insert into calls (project_id, lead_id, kind, step, status, to_number) select project_id, id, 'sequence', 1, 'open', phone from leads where id = $1 returning id")
                .bind(lead)
                .fetch_one(self.db())
                .await
                .unwrap()
        }

        async fn row(&self, call: Uuid) -> Call {
            sqlx::query_as("select * from calls where id = $1").bind(call).fetch_one(self.db()).await.unwrap()
        }

        async fn lead(&self, lead: Uuid) -> Lead {
            sqlx::query_as("select * from leads where id = $1").bind(lead).fetch_one(self.db()).await.unwrap()
        }

        async fn set_zone(&self, zone: &str) {
            sqlx::query("update project_settings set timezone = $2 where project_id = $1").bind(self.project).bind(zone).execute(self.db()).await.unwrap();
        }
    }

    async fn emailed_lead(db: &PgPool, project: Uuid, name: &str, phone: &str, owner: Option<Uuid>) -> Uuid {
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, full_name, first_name, email, phone, company_name, status, owner_id)
             values ($1, $2, $2, $3, $4, 'Smile Dental', 'contacted', $5) returning id",
        )
        .bind(project)
        .bind(name)
        .bind(format!("{}@smile.test", name.to_lowercase()))
        .bind(phone)
        .bind(owner)
        .fetch_one(db)
        .await
        .unwrap();
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, status, subject, body, sent_at, message_id)
             values ($1, $2, 'outbound', 'initial', 'sent', 'Hi', 'Hello', now() - interval '3 days', $3)",
        )
        .bind(project)
        .bind(lead)
        .bind(format!("<{lead}@splee.test>"))
        .execute(db)
        .await
        .unwrap();
        lead
    }

    async fn write_runs_for(db: &PgPool, lead: Uuid) -> i64 {
        sqlx::query_scalar("select count(*) from agent_runs where kind = 'write_emails' and input -> 'lead_ids' ? $1").bind(lead.to_string()).fetch_one(db).await.unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_call_step_opens_a_call_for_the_owner_and_the_sequence_waits_for_it(db: PgPool) {
        let s = Seeded::start(db).await;
        let nobodys = emailed_lead(s.db(), s.project, "Ana", "+14155550135", None).await;
        let no_phone = emailed_lead(s.db(), s.project, "Kai", "", None).await;
        let fresh = emailed_lead(s.db(), s.project, "Lu", "+14155550136", None).await;
        sqlx::query("update email_messages set sent_at = now() - interval '1 day' where lead_id = $1").bind(fresh).execute(s.db()).await.unwrap();

        // Sam, Ana and Kai here, and Beta's lead in their own project.
        assert_eq!(calls::schedule(s.db()).await.unwrap(), 4);
        assert_eq!(calls::schedule(s.db()).await.unwrap(), 0, "a step opens once");
        let calls_of = |lead: Uuid| sqlx::query_as::<_, Call>("select * from calls where lead_id = $1").bind(lead).fetch_all(s.db());
        let sams = calls_of(s.sam).await.unwrap();
        assert_eq!(sams.len(), 1);
        let call = &sams[0];
        assert_eq!((call.status.as_str(), call.step, call.owner_id, call.instructions.as_str()), ("open", 1, Some(s.jo), "Ask about the new clinic"));
        assert_eq!(calls_of(nobodys).await.unwrap()[0].owner_id, None, "anyone can take an unowned lead's call");
        let skipped = &calls_of(no_phone).await.unwrap()[0];
        assert_eq!((skipped.status.as_str(), skipped.cancel_reason.as_str()), ("skipped", "No phone number"));
        assert!(calls_of(fresh).await.unwrap().is_empty(), "not due for another day");

        // The email after the call waits while the call is open.
        crate::mail::sender::schedule_follow_ups(&s.state).await.unwrap();
        assert_eq!(write_runs_for(s.db(), s.sam).await, 0);

        // Jo sees it with the unassigned ones, and Acme's owner sees it under everyone.
        let (status, mine) = s.call("jo", Method::GET, &format!("/projects/{}/calls?owner=mine_or_unassigned", s.project), json!({})).await;
        assert_eq!(status, 200, "{mine}");
        let names: Vec<&str> = mine.as_array().unwrap().iter().map(|c| c["lead_name"].as_str().unwrap()).collect();
        assert!(names.contains(&"Sam") && names.contains(&"Ana"), "{names:?}");
        let (_, theirs) = s.call("acme", Method::GET, &format!("/projects/{}/calls?owner=mine&status=open", s.project), json!({})).await;
        assert_eq!(theirs, json!([]));
        let sam_row = mine.as_array().unwrap().iter().find(|c| c["lead_name"] == "Sam").unwrap().clone();
        assert_eq!((sam_row["bucket"].as_str(), sam_row["owner_name"].as_str(), sam_row["outside_hours"].clone()), (Some("overdue"), Some("jo@example.test"), Value::Null));
        assert_eq!(s.call("beta", Method::GET, &format!("/projects/{}/calls", s.project), json!({})).await.0, 404);

        // Logged three days ago, the email after it is due now.
        let (status, logged) = s.call("jo", Method::POST, &format!("/calls/{}/log", call.id), json!({ "outcome": "voicemail", "notes": "Left a message" })).await;
        assert_eq!((status, logged["status"].as_str(), logged["called_by"].as_str()), (200, Some("done"), Some(s.jo.to_string().as_str())));
        crate::mail::sender::schedule_follow_ups(&s.state).await.unwrap();
        assert_eq!(write_runs_for(s.db(), s.sam).await, 0, "3 days after the call, not after the email");
        sqlx::query("update calls set closed_at = now() - interval '3 days' where id = $1").bind(call.id).execute(s.db()).await.unwrap();
        crate::mail::sender::schedule_follow_ups(&s.state).await.unwrap();
        assert_eq!(write_runs_for(s.db(), s.sam).await, 1);
        // Skipping moves the sequence on too.
        let ana = &calls_of(nobodys).await.unwrap()[0];
        assert_eq!(s.call("acme", Method::POST, &format!("/calls/{}/skip", ana.id), json!({})).await.1["status"], "skipped");
        assert_eq!(s.call("acme", Method::POST, &format!("/calls/{}/skip", ana.id), json!({})).await.0, 409);
        assert_eq!(s.call("beta", Method::POST, &format!("/calls/{}/log", ana.id), json!({ "outcome": "connected" })).await.0, 404);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn open_calls_are_cancelled_when_the_lead_replies_books_or_opts_out(db: PgPool) {
        let s = Seeded::start(db).await;
        let replied = s.open_call(s.sam).await;
        let booker = emailed_lead(s.db(), s.project, "Ana", "+14155550135", None).await;
        let booked = s.open_call(booker).await;
        let opted = emailed_lead(s.db(), s.project, "Kai", "+14155550136", None).await;
        let suppressed = s.open_call(opted).await;
        let emailer = emailed_lead(s.db(), s.project, "Lu", "+14155550137", None).await;
        let emailed_back = s.open_call(emailer).await;
        let stopper = emailed_lead(s.db(), s.project, "Max", "+14155550138", None).await;
        let stopped = s.open_call(stopper).await;

        crate::routes::linkedin::stop_for_reply(s.db(), &s.lead(s.sam).await, "They replied by text").await.unwrap();
        assert_eq!(s.call("acme", Method::PATCH, &format!("/leads/{booker}"), json!({ "status": "meeting_booked" })).await.0, 200);
        crate::mail::suppress_lead(&s.state, &s.lead(stopper).await, Some(s.acme_org), "unsubscribed", "unsubscribe").await.unwrap();
        // Ones the paths above don't see go when the page loads or the next tick runs.
        sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, '+14155550136', 'phone', 'manual')").bind(s.acme_org).execute(s.db()).await.unwrap();
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status, body) values ($1, $2, 'inbound', 'inbound', 'received', 'Not now')")
            .bind(s.project)
            .bind(emailer)
            .execute(s.db())
            .await
            .unwrap();
        let (_, open) = s.call("acme", Method::GET, &format!("/projects/{}/calls?status=open", s.project), json!({})).await;
        assert_eq!(open, json!([]));

        for (call, why) in [
            (replied, "They replied by text"),
            (booked, "They booked a meeting"),
            (stopped, "Lead unsubscribed"),
            (suppressed, "On the do-not-contact list"),
            (emailed_back, "They replied"),
        ] {
            let row = s.row(call).await;
            assert_eq!((row.status.as_str(), row.cancel_reason.as_str()), ("cancelled", why));
        }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_browser_gets_a_token_only_for_a_call_it_may_make(db: PgPool) {
        let s = Seeded::start(db).await;
        let call = s.open_call(s.sam).await;
        let (status, dial) = s.call("jo", Method::POST, &format!("/calls/{call}/dial"), json!({})).await;
        assert_eq!(status, 200, "{dial}");
        assert_eq!((dial["to"].as_str(), dial["identity"].as_str()), (Some(SAM), Some(calls::identity(s.jo).as_str())));
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(dial["token"].as_str().unwrap().split('.').nth(1).unwrap()).unwrap()).unwrap();
        assert_eq!((claims["iss"].as_str(), claims["sub"].as_str()), (Some(fake::KEY), Some(fake::ACCOUNT)));
        assert_eq!(claims["grants"]["voice"]["outgoing"]["application_sid"], fake::APP);
        assert_eq!(claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(), calls::TOKEN_SECS);

        assert_eq!(s.call("beta", Method::POST, &format!("/calls/{call}/dial"), json!({})).await.0, 404);
        s.set_zone(&zone_at(22)).await;
        let (status, late) = s.call("jo", Method::POST, &format!("/calls/{call}/dial"), json!({})).await;
        assert_eq!(status, 409);
        assert!(late["error"].as_str().unwrap().contains("Call between 8:00 and 20:00 their time"), "{late}");
        let (_, listed) = s.call("jo", Method::GET, &format!("/projects/{}/calls", s.project), json!({})).await;
        assert!(listed[0]["outside_hours"].as_str().unwrap().starts_with("It's 22:"), "{listed}");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn twilio_dials_only_a_member_calling_the_leads_own_number_inside_their_hours(db: PgPool) {
        let s = Seeded::start(db).await;
        let call = s.open_call(s.sam).await;
        let beta_call = s.open_call(s.beta_lead).await;

        let (status, twiml) = s.voice(call, s.beta_user, SAM, "CA1").await;
        assert_eq!(status, 200);
        assert!(twiml.contains("<Say>Only someone on the lead's team can call them.</Say>") && !twiml.contains("<Dial"), "{twiml}");
        let (_, twiml) = s.voice(call, s.jo, "+14155550199", "CA1").await;
        assert!(twiml.contains("That number isn't this lead's.") && !twiml.contains("<Dial"), "{twiml}");
        let (_, twiml) = s.voice(beta_call, s.jo, SAM, "CA1").await;
        assert!(twiml.contains("This call isn't in your organization."), "{twiml}");
        let (_, twiml) = s.voice(call, Uuid::new_v4(), SAM, "CA1").await;
        assert!(twiml.contains("Only someone on the lead's team"), "{twiml}");

        sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, $2, 'phone', 'unsubscribe')").bind(s.acme_org).bind(SAM).execute(s.db()).await.unwrap();
        let (_, twiml) = s.voice(call, s.jo, SAM, "CA1").await;
        assert!(twiml.contains("They're on the do-not-contact list.") && !twiml.contains("<Dial"), "{twiml}");
        sqlx::query("delete from suppressions").execute(s.db()).await.unwrap();

        s.set_zone(&zone_at(6)).await;
        let (_, twiml) = s.voice(call, s.jo, SAM, "CA1").await;
        assert!(twiml.contains("where they are. Call between 8:00 and 20:00 their time.") && !twiml.contains("<Dial"), "{twiml}");
        s.set_zone(&zone_at(12)).await;
        assert!(s.row(call).await.called_at.is_none(), "nothing was dialed");

        // Signed with something other than the auth token, or for an account nobody connected.
        let params = [("AccountSid", fake::ACCOUNT), ("CallSid", "CA1"), ("From", "client:x"), ("To", SAM)];
        assert_eq!(s.webhook("voice", &params, "not-the-token").await.0, 403);
        let params = [("AccountSid", "AC00000000000000000000000000000000"), ("CallSid", "CA1"), ("From", "client:x"), ("To", SAM)];
        assert_eq!(s.webhook("voice", &params, "token").await.0, 403);

        let (status, twiml) = s.voice(call, s.jo, SAM, "CA1").await;
        assert_eq!(status, 200);
        assert!(twiml.contains("<Dial callerId=\"+14155550100\"") && twiml.contains(&format!(">{SAM}</Number>")), "{twiml}");
        assert!(twiml.contains("<Start><Transcription"), "transcription is on: {twiml}");
        let row = s.row(call).await;
        assert_eq!((row.called_by, row.to_number.as_str(), row.call_status.as_str()), (Some(s.jo), SAM, "ringing"));
        // Twilio retrying the same call still dials, but a second call on the task waits for the first to be logged.
        assert!(s.voice(call, s.jo, SAM, "CA1").await.1.contains("<Dial"));
        let (_, again) = s.voice(call, s.jo, SAM, "CA9").await;
        assert!(!again.contains("<Dial") && again.contains("already made"), "{again}");
        let sid: Option<String> = sqlx::query_scalar("select twilio_sid from calls where id = $1").bind(call).fetch_one(s.db()).await.unwrap();
        assert_eq!(sid.as_deref(), Some("CA1"));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn signed_callbacks_fill_in_the_length_recording_and_transcript(db: PgPool) {
        let s = Seeded::start(db).await;
        let call = s.open_call(s.sam).await;
        assert_eq!(s.voice(call, s.jo, SAM, "CA1").await.0, 200);

        let status = [("AccountSid", fake::ACCOUNT), ("CallSid", "CA2"), ("ParentCallSid", "CA1"), ("CallStatus", "completed"), ("CallDuration", "42")];
        assert_eq!(s.webhook("calls/status", &status, "nope").await.0, 403);
        assert_eq!(s.row(call).await.duration_secs, None);
        assert_eq!(s.webhook("calls/status", &status, "token").await.0, 204);
        let row = s.row(call).await;
        assert_eq!((row.call_status.as_str(), row.duration_secs), ("completed", Some(42)));

        let url = format!("https://api.twilio.com/2010-04-01/Accounts/{}/Recordings/{}", fake::ACCOUNT, fake::RECORDING);
        let recording = [("AccountSid", fake::ACCOUNT), ("CallSid", "CA1"), ("RecordingSid", fake::RECORDING), ("RecordingUrl", url.as_str()),
                         ("RecordingDuration", "40"), ("RecordingStatus", "completed")];
        assert_eq!(s.webhook("calls/recording", &recording, "nope").await.0, 403);
        assert_eq!(s.webhook("calls/recording", &recording, "token").await.0, 204);
        let row = s.row(call).await;
        assert_eq!((row.recording_url.as_str(), row.recording_secs), (url.as_str(), Some(40)));
        let res = s.client.get(format!("{}/calls/{call}/recording", s.base)).header("cookie", "splee_session=jo").send().await.unwrap();
        assert_eq!(res.headers()["content-type"], "audio/mpeg");
        assert_eq!(res.bytes().await.unwrap().as_ref(), b"ID3 fake mp3");
        let res = s.client.get(format!("{}/calls/{call}/recording", s.base)).header("cookie", "splee_session=beta").send().await.unwrap();
        assert_eq!(res.status().as_u16(), 404);

        let part = |seq: &'static str, track: &'static str, text: &str, last: &'static str| {
            let data = json!({ "transcript": text, "confidence": 0.9 }).to_string();
            vec![("AccountSid", fake::ACCOUNT.to_string()), ("CallSid", "CA1".into()), ("TranscriptionEvent", "transcription-content".into()),
                 ("SequenceId", seq.into()), ("Track", track.into()), ("TranscriptionData", data), ("Final", last.into())]
        };
        async fn post(s: &Seeded, params: Vec<(&'static str, String)>, token: &str) -> u16 {
            let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
            s.webhook("calls/transcription", &params, token).await.0
        }
        assert_eq!(post(&s, part("1", "inbound_track", "Is now a good time?", "true"), "nope").await, 403);
        // Out of order, and a partial result that's dropped.
        assert_eq!(post(&s, part("3", "outbound_track", "Send me the case study.", "true"), "token").await, 204);
        assert_eq!(post(&s, part("2", "outbound_track", "Send me", "false"), "token").await, 204);
        assert_eq!(post(&s, part("1", "inbound_track", "Is now a good time?", "true"), "token").await, 204);
        let stopped = || vec![("AccountSid", fake::ACCOUNT.to_string()), ("CallSid", "CA1".into()), ("TranscriptionEvent", "transcription-stopped".into())];
        assert_eq!(post(&s, stopped(), "token").await, 204);
        assert_eq!(post(&s, stopped(), "token").await, 204);
        assert_eq!(s.row(call).await.transcript, "Us: Is now a good time?\nLead: Send me the case study.");
        let runs: Vec<Value> = sqlx::query_scalar("select input from agent_runs where kind = 'summarize_call' and project_id = $1").bind(s.project).fetch_all(s.db()).await.unwrap();
        assert_eq!(runs, vec![json!({ "call_id": call })]);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn outcomes_update_the_lead(db: PgPool) {
        let s = Seeded::start(db).await;
        let unsent_follow_up = |lead: Uuid| {
            sqlx::query_scalar::<_, Uuid>(
                "insert into email_messages (project_id, lead_id, direction, kind, step, status, subject, body)
                 select project_id, id, 'outbound', 'follow_up', 2, 'approved', 'Re: Hi', 'Following on' from leads where id = $1 returning id",
            )
            .bind(lead)
            .fetch_one(s.db())
        };
        let approved_text = |lead: Uuid| {
            sqlx::query_scalar::<_, Uuid>(
                "insert into text_messages (project_id, lead_id, channel, kind, step, to_number, body, status, due_at)
                 select project_id, id, 'sms', 'sequence', 1, phone, 'Hi', 'approved', now() from leads where id = $1 returning id",
            )
            .bind(lead)
            .fetch_one(s.db())
        };
        let email_status = |id: Uuid| sqlx::query_as::<_, (String, String)>("select status, error from email_messages where id = $1").bind(id).fetch_one(s.db());
        let text_status = |id: Uuid| sqlx::query_as::<_, (String, String)>("select status, error from text_messages where id = $1").bind(id).fetch_one(s.db());
        let log = |call: Uuid, outcome: &'static str| {
            let s = &s;
            async move { s.call("jo", Method::POST, &format!("/calls/{call}/log"), json!({ "outcome": outcome, "notes": "Spoke briefly" })).await }
        };

        let booked = s.open_call(s.sam).await;
        let email = unsent_follow_up(s.sam).await.unwrap();
        assert_eq!(log(booked, "meeting_booked").await.0, 200);
        assert_eq!(s.lead(s.sam).await.status, "meeting_booked");
        assert_eq!(email_status(email).await.unwrap(), ("cancelled".into(), "They booked a meeting on a call".into()));
        assert!(crate::deals::for_lead(s.db(), s.sam).await.unwrap().is_some(), "booking opens the lead's deal like any booking");
        assert_eq!(log(booked, "connected").await.0, 409, "a call is logged once");

        let wrong = emailed_lead(s.db(), s.project, "Ana", "+14155550135", None).await;
        let call = s.open_call(wrong).await;
        let text = approved_text(wrong).await.unwrap();
        assert_eq!(log(call, "wrong_number").await.0, 200);
        let lead = s.lead(wrong).await;
        assert_eq!((lead.phone.as_str(), lead.status.as_str()), ("", "contacted"));
        assert_eq!(text_status(text).await.unwrap(), ("cancelled".into(), "Wrong number".into()));

        let cold = emailed_lead(s.db(), s.project, "Kai", "+14155550136", None).await;
        let call = s.open_call(cold).await;
        let email = unsent_follow_up(cold).await.unwrap();
        assert_eq!(log(call, "not_interested").await.0, 200);
        assert_eq!(s.lead(cold).await.status, "not_interested");
        assert_eq!(email_status(email).await.unwrap(), ("cancelled".into(), "Not interested on a call".into()));

        let warm = emailed_lead(s.db(), s.project, "Lu", "+14155550137", None).await;
        let call = s.open_call(warm).await;
        assert_eq!(s.call("jo", Method::POST, &format!("/calls/{call}/log"), json!({ "outcome": "maybe" })).await.0, 400);
        let (_, logged) = log(call, "connected").await;
        assert_eq!((logged["outcome"].as_str(), logged["notes"].as_str()), (Some("connected"), Some("Spoke briefly")));
        let lead = s.lead(warm).await;
        assert_eq!(lead.status, "contacted");
        assert!(lead.last_contacted_at.is_some());
        // The lead's timeline has it.
        let (_, detail) = s.call("jo", Method::GET, &format!("/leads/{warm}"), json!({})).await;
        assert_eq!(detail["calls"][0]["outcome"], "connected");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_api_and_mcp_list_and_log_calls_for_the_keys_organization(db: PgPool) {
        let s = Seeded::start(db).await;
        let call = s.open_call(s.sam).await;
        s.open_call(s.beta_lead).await;

        let (status, listed) = s.api(Method::GET, &format!("/v1/projects/{}/calls?owner=unassigned", s.project), json!({})).await;
        assert_eq!((status, listed[0]["id"].as_str()), (200, Some(call.to_string().as_str())), "{listed}");
        assert!(listed[0].get("transcript_parts").is_none() && listed[0].get("twilio_sid").is_none());
        assert_eq!(s.api(Method::GET, &format!("/v1/projects/{}/calls?owner=jo@example.test", s.project), json!({})).await.1, json!([]));
        assert_eq!(s.api(Method::GET, &format!("/v1/projects/{}/calls", s.beta_project), json!({})).await.0, 404);

        let (status, logged) = s.api(Method::POST, &format!("/v1/leads/{}/calls", s.sam), json!({ "outcome": "voicemail", "notes": "From the dialer" })).await;
        assert_eq!((status, logged["id"].as_str(), logged["status"].as_str()), (200, Some(call.to_string().as_str()), Some("done")), "{logged}");
        let (_, manual) = s.api(Method::POST, &format!("/v1/leads/{}/calls", s.sam), json!({ "outcome": "connected" })).await;
        assert_eq!((manual["kind"].as_str(), manual["status"].as_str()), (Some("manual"), Some("done")));
        assert_eq!(s.api(Method::POST, &format!("/v1/leads/{}/calls", s.beta_lead), json!({ "outcome": "connected" })).await.0, 404);
        assert_eq!(s.api(Method::POST, &format!("/v1/leads/{}/calls", s.sam), json!({ "outcome": "busy" })).await.0, 400);

        let mcp = |tool: &str, arguments: Value| {
            let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": tool, "arguments": arguments } });
            let req = s.client.post(format!("{}/mcp", s.base)).bearer_auth(KEY).header("accept", "application/json, text/event-stream").json(&body);
            async move {
                let res: Value = req.send().await.unwrap().json().await.unwrap();
                (res["result"]["isError"].as_bool().unwrap(), res["result"]["content"][0]["text"].as_str().unwrap().to_string())
            }
        };
        let (error, text) = mcp("list_call_tasks", json!({ "project_id": s.project, "status": "done" })).await;
        assert!(!error && text.contains("From the dialer") && !text.contains("\"lead_title\""), "{text}");
        let other = emailed_lead(s.db(), s.project, "Ana", "+14155550135", None).await;
        let (error, text) = mcp("log_call", json!({ "lead_id": other, "outcome": "meeting_booked" })).await;
        assert!(!error && text.contains("meeting_booked"), "{text}");
        assert_eq!(s.lead(other).await.status, "meeting_booked");
        let (error, _) = mcp("log_call", json!({ "lead_id": s.beta_lead, "outcome": "connected" })).await;
        assert!(error, "another organization's lead");
        let (error, _) = mcp("list_call_tasks", json!({ "project_id": s.beta_project })).await;
        assert!(error);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn turning_calling_on_points_the_twiml_app_at_splee(db: PgPool) {
        let s = Seeded::start(db).await;
        let body = |sid: &str| json!({ "twiml_app_sid": sid, "voice_from": "+1 415 555 0199", "transcribe": false });
        let (status, saved) = s.call("acme", Method::PUT, "/organization/twilio/calling", body(fake::APP)).await;
        assert_eq!(status, 200, "{saved}");
        assert_eq!((saved["connection"]["voice_from"].as_str(), saved["connection"]["call_transcription"].as_bool()), (Some("+14155550199"), Some(false)));
        assert_eq!(saved["voice_url"], "https://splee.test/api/twilio/voice");
        let update = s.sent.lock().unwrap().last().cloned().unwrap();
        assert_eq!((update["VoiceUrl"].as_str(), update["VoiceMethod"].as_str()), ("https://splee.test/api/twilio/voice", "POST"));
        let (status, err) = s.call("acme", Method::PUT, "/organization/twilio/calling", body("AP00000000000000000000000000000000")).await;
        assert!(status == 400 && err["error"].as_str().unwrap().starts_with("Twilio couldn't set up that TwiML App"), "{err}");
        assert_eq!(s.call("acme", Method::PUT, "/organization/twilio/calling", body("AP123")).await.0, 400);
        assert_eq!(s.call("jo", Method::PUT, "/organization/twilio/calling", body(fake::APP)).await.0, 403, "admins only");
        assert_eq!(s.call("beta", Method::PUT, "/organization/twilio/calling", body(fake::APP)).await.0, 400, "Beta has no Twilio");

        // Turned off, calls can't be placed.
        s.call("acme", Method::PUT, "/organization/twilio/calling", body("")).await;
        let call = s.open_call(s.sam).await;
        let (status, off) = s.call("jo", Method::POST, &format!("/calls/{call}/dial"), json!({})).await;
        assert_eq!((status, off["error"].as_str()), (409, Some("Turn on calling in the Twilio card on the Integrations page first.")));
    }
}
