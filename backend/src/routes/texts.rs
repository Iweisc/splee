//! Texts: the organization's Twilio connection on the Integrations page, the Texts page's drafts and sends, replying by
//! text from the inbox, and the webhooks Twilio posts for answers (/api/twilio/inbound) and delivery (/api/twilio/status).
//! The sending and the webhooks' effects are in crate::twilio.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, patch, post, put};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

use super::leads::{LEAD_ROW_SELECT, LeadRow};
use super::util_scope;
use crate::auth::Auth;
use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::models::{Lead, TextMessage, Ts};
use crate::phone;
use crate::state::AppState;
use crate::twilio::{self, Account, Client, Inbound, TEMPLATE_FIELDS};

/// Twilio's longest message. A text that long goes out as 10 segments and costs 10 texts.
const MAX_TEXT_CHARS: usize = 1600;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/organization/twilio", get(status).put(connect).delete(disconnect))
        .route("/organization/twilio/template", put(set_template))
        .route("/projects/{id}/texts", get(list))
        .route("/texts/{id}", patch(update))
        .route("/inbox/threads/{lead_id}/text", post(reply))
        .route("/twilio/inbound", post(inbound))
        .route("/twilio/status", post(delivery))
}

#[derive(Serialize, FromRow)]
struct Connection {
    account_sid: String,
    account_name: String,
    api_key_sid: String,
    sms_from: String,
    whatsapp_from: String,
    whatsapp_template_sid: String,
    whatsapp_template_body: String,
    whatsapp_template_vars: Value,
    error: Option<String>,
    failed_at: Option<Ts>,
    created_at: Ts,
}

#[derive(Serialize)]
struct Status {
    connection: Option<Connection>,
    /// Where the Twilio number's or Messaging Service's incoming messages webhook should point.
    inbound_url: String,
}

async fn status(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Status>> {
    let connection = sqlx::query_as::<_, Connection>("select * from twilio_accounts where org_id = $1").bind(auth.org_id).fetch_optional(&state.db).await?;
    Ok(Json(Status { connection, inbound_url: twilio::inbound_url(&state.cfg.public_url) }))
}

/// Twilio's ids: two letters and 32 hex digits.
fn sid(raw: &str, prefix: &str) -> Option<String> {
    let raw = raw.trim();
    let hex = raw.get(2..)?;
    (raw.len() == 34 && raw[..2].eq_ignore_ascii_case(prefix) && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("{prefix}{hex}"))
}

/// A number written with its country code, since a sender has no country to read a local one by.
fn sender_number(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.starts_with('+') { phone::normalize(raw, "") } else { None }
}

#[derive(Deserialize)]
struct ConnectBody {
    account_sid: String,
    api_key_sid: String,
    api_secret: String,
    auth_token: String,
    #[serde(default)]
    sms_from: String,
    #[serde(default)]
    whatsapp_from: String,
}

/// Checks the API key and the auth token with Twilio before saving them. Reconnecting the same account keeps its template.
async fn connect(State(state): State<AppState>, auth: Auth, Json(body): Json<ConnectBody>) -> AppResult<Json<Status>> {
    auth.require_admin()?;
    let bad = |m: &str| AppError::BadRequest(m.into());
    let account_sid = sid(&body.account_sid, "AC").ok_or_else(|| bad("The Account SID starts with AC, then 32 letters and digits"))?;
    let key_sid = sid(&body.api_key_sid, "SK").ok_or_else(|| bad("The API key SID starts with SK, then 32 letters and digits"))?;
    let (secret, token) = (body.api_secret.trim(), body.auth_token.trim());
    if secret.is_empty() || token.is_empty() {
        return Err(bad("Paste the API key's secret and the account's auth token"));
    }
    let sms_from = match body.sms_from.trim() {
        "" => String::new(),
        raw if raw.to_uppercase().starts_with("MG") => sid(raw, "MG").ok_or_else(|| bad("A Messaging Service SID starts with MG, then 32 letters and digits"))?,
        raw => sender_number(raw).ok_or_else(|| bad("Enter the SMS number with its country code, like +14155550100, or a Messaging Service SID"))?,
    };
    let whatsapp_from = match body.whatsapp_from.trim() {
        "" => String::new(),
        raw => sender_number(raw).ok_or_else(|| bad("Enter the WhatsApp number with its country code, like +14155550100"))?,
    };
    if sms_from.is_empty() && whatsapp_from.is_empty() {
        return Err(bad("Add an SMS sender, a WhatsApp number, or both"));
    }
    let taken: bool = sqlx::query_scalar("select exists (select 1 from twilio_accounts where account_sid = $1 and org_id <> $2)")
        .bind(&account_sid)
        .bind(auth.org_id)
        .fetch_one(&state.db)
        .await?;
    if taken {
        return Err(AppError::Conflict("This Twilio account is connected to another Splee organization".into()));
    }
    let base = &state.cfg.twilio_api_url;
    let name = Client::new(base, &account_sid, &key_sid, secret).check().await.map_err(|e| bad(&format!("Twilio didn't take the API key. {}", e.reason())))?;
    // Webhooks are signed with the auth token, so a wrong one would turn every reply away later.
    Client::new(base, &account_sid, &account_sid, token).check().await.map_err(|e| bad(&format!("Twilio didn't take the auth token. {}", e.reason())))?;
    let secret_key = &state.cfg.app_secret;
    sqlx::query(
        "insert into twilio_accounts (org_id, account_sid, account_name, api_key_sid, api_secret_enc, auth_token_enc, sms_from, whatsapp_from, created_by)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         on conflict (org_id) do update set
           whatsapp_template_sid = case when twilio_accounts.account_sid = excluded.account_sid then twilio_accounts.whatsapp_template_sid else '' end,
           whatsapp_template_body = case when twilio_accounts.account_sid = excluded.account_sid then twilio_accounts.whatsapp_template_body else '' end,
           whatsapp_template_vars = case when twilio_accounts.account_sid = excluded.account_sid then twilio_accounts.whatsapp_template_vars else '{}' end,
           account_sid = excluded.account_sid, account_name = excluded.account_name, api_key_sid = excluded.api_key_sid,
           api_secret_enc = excluded.api_secret_enc, auth_token_enc = excluded.auth_token_enc, sms_from = excluded.sms_from,
           whatsapp_from = excluded.whatsapp_from, error = null, failed_at = null",
    )
    .bind(auth.org_id)
    .bind(&account_sid)
    .bind(name)
    .bind(&key_sid)
    .bind(crypto::encrypt(secret_key, secret))
    .bind(crypto::encrypt(secret_key, token))
    .bind(sms_from)
    .bind(whatsapp_from)
    .bind(auth.user_id)
    .execute(&state.db)
    .await?;
    status(State(state), auth).await
}

/// Approved texts wait until Twilio is connected again.
async fn disconnect(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Status>> {
    auth.require_admin()?;
    sqlx::query("delete from twilio_accounts where org_id = $1").bind(auth.org_id).execute(&state.db).await?;
    status(State(state), auth).await
}

#[derive(Deserialize)]
struct TemplateBody {
    template_sid: String,
    /// Each of the template's variables to a lead field, like {"1": "first_name"}. Left out, {{1}} is their first name, {{2}}
    /// their company and {{3}} the sender's name, the order most templates use them in.
    #[serde(default)]
    variables: HashMap<String, String>,
}

/// Picks the WhatsApp template sequence steps send, read from Twilio so its variables can be checked. An empty SID clears it,
/// and WhatsApp steps wait until there's one again.
async fn set_template(State(state): State<AppState>, auth: Auth, Json(body): Json<TemplateBody>) -> AppResult<Json<Status>> {
    auth.require_admin()?;
    let account = Account::of_org(&state.db, auth.org_id).await?.ok_or_else(|| AppError::BadRequest("Connect Twilio first".into()))?;
    let (sid_value, text, vars) = if body.template_sid.trim().is_empty() {
        (String::new(), String::new(), json!({}))
    } else {
        let sid_value = sid(&body.template_sid, "HX").ok_or_else(|| AppError::BadRequest("A Content template SID starts with HX, then 32 letters and digits".into()))?;
        let client = Client::for_account(&state, &account)?;
        let (text, names) = client.template(&sid_value).await.map_err(|e| AppError::BadRequest(format!("Twilio couldn't find that template. {}", e.reason())))?;
        let mut vars = serde_json::Map::new();
        for name in names {
            let usual = match name.as_str() {
                "1" => Some("first_name"),
                "2" => Some("company_name"),
                "3" => Some("sender_name"),
                _ => None,
            };
            let field = match body.variables.get(&name).map(|f| f.trim()) {
                Some(f) => Some(f).filter(|f| TEMPLATE_FIELDS.contains(f)),
                None => usual,
            };
            let field = field.ok_or_else(|| AppError::BadRequest(format!("Pick what fills {{{{{name}}}}}")))?;
            vars.insert(name, json!(field));
        }
        (sid_value, text, Value::Object(vars))
    };
    sqlx::query("update twilio_accounts set whatsapp_template_sid = $2, whatsapp_template_body = $3, whatsapp_template_vars = $4 where org_id = $1")
        .bind(auth.org_id)
        .bind(sid_value)
        .bind(text)
        .bind(vars)
        .execute(&state.db)
        .await?;
    status(State(state), auth).await
}

#[derive(Deserialize)]
struct ListQuery {
    segment_id: Option<Uuid>,
}

#[derive(Serialize)]
struct Thread {
    lead: LeadRow,
    texts: Vec<TextMessage>,
}

/// Leads with a text written or sent, newest first.
async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(q): Query<ListQuery>) -> AppResult<Json<Vec<Thread>>> {
    auth.project(&state.db, id).await?;
    let leads = sqlx::query_as::<_, LeadRow>(&format!(
        "{LEAD_ROW_SELECT}
         where l.project_id = $1 and ($2::uuid is null or l.segment_id = $2) and exists (select 1 from text_messages t where t.lead_id = l.id)
         order by (select max(t.created_at) from text_messages t where t.lead_id = l.id) desc limit 500"
    ))
    .bind(id)
    .bind(q.segment_id)
    .fetch_all(&state.db)
    .await?;
    let ids: Vec<Uuid> = leads.iter().map(|l| l.lead.id).collect();
    let texts = sqlx::query_as::<_, TextMessage>("select * from text_messages where lead_id = any($1) order by coalesce(sent_at, due_at, created_at)")
        .bind(&ids)
        .fetch_all(&state.db)
        .await?;
    let mut by_lead: HashMap<Uuid, Vec<TextMessage>> = HashMap::new();
    for text in texts {
        by_lead.entry(text.lead_id).or_default().push(text);
    }
    Ok(Json(leads.into_iter().map(|lead| Thread { texts: by_lead.remove(&lead.lead.id).unwrap_or_default(), lead }).collect()))
}

#[derive(Deserialize)]
struct UpdateBody {
    /// "approved" or "cancelled".
    status: Option<String>,
    body: Option<String>,
}

/// Approves, edits or cancels a text that hasn't gone out. A template's text is Twilio's, so only SMS and free WhatsApp text
/// can be edited.
async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<UpdateBody>) -> AppResult<Json<TextMessage>> {
    let text: TextMessage = util_scope::load(&state.db, "text_messages", id, auth.org_id).await?;
    if !["draft", "approved"].contains(&text.status.as_str()) {
        return Err(AppError::Conflict("This text already went out or was cancelled".into()));
    }
    if let Some(status) = &body.status {
        super::util_input::one_of("status", status, &["approved", "cancelled"])?;
    }
    let new_body = body.body.as_deref().map(str::trim);
    if let Some(b) = new_body {
        if !text.template_sid.is_empty() {
            return Err(AppError::BadRequest("This is a WhatsApp template, so its text can't be changed here".into()));
        }
        if b.is_empty() || b.chars().count() > MAX_TEXT_CHARS {
            return Err(AppError::BadRequest(format!("A text needs 1 to {MAX_TEXT_CHARS} characters")));
        }
    }
    let updated = sqlx::query_as::<_, TextMessage>(
        "update text_messages set status = coalesce($2, status), body = coalesce($3, body),
           error = case when $2 = 'cancelled' then 'Cancelled by hand' else error end
         where id = $1 and status in ('draft', 'approved') returning *",
    )
    .bind(text.id)
    .bind(body.status)
    .bind(new_body)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::Conflict("This text already went out".into()))?;
    Ok(Json(updated))
}

#[derive(Deserialize)]
struct ReplyBody {
    body: String,
}

/// Answers the lead on the channel they last texted on. It goes out within a minute, any time of day, like an email reply.
/// WhatsApp only takes free text in the 24 hours after they last wrote there.
async fn reply(State(state): State<AppState>, auth: Auth, Path(lead_id): Path<Uuid>, Json(body): Json<ReplyBody>) -> AppResult<Json<TextMessage>> {
    let lead: Lead = util_scope::load(&state.db, "leads", lead_id, auth.org_id).await?;
    let text = body.body.trim();
    if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS {
        return Err(AppError::BadRequest(format!("A text needs 1 to {MAX_TEXT_CHARS} characters")));
    }
    if Account::of_org(&state.db, auth.org_id).await?.is_none() {
        return Err(AppError::BadRequest("Connect Twilio on the Integrations page to text leads".into()));
    }
    let last: Option<(String, Ts)> = sqlx::query_as(
        "select channel, created_at from email_messages where lead_id = $1 and direction = 'inbound' and channel <> 'email' order by created_at desc limit 1",
    )
    .bind(lead.id)
    .fetch_optional(&state.db)
    .await?;
    let Some((channel, at)) = last else { return Err(AppError::BadRequest("They haven't texted you, so answer by email".into())) };
    if channel == "whatsapp" && at < chrono::Utc::now() - chrono::Duration::hours(24) {
        return Err(AppError::BadRequest("It's been more than 24 hours since they wrote on WhatsApp, so WhatsApp only takes an approved template now".into()));
    }
    let blocked: bool = sqlx::query_scalar("select exists (select 1 from suppressions where org_id = $1 and kind = 'phone' and value = $2)")
        .bind(auth.org_id)
        .bind(&lead.phone)
        .fetch_one(&state.db)
        .await?;
    if blocked || lead.phone.is_empty() || ["unsubscribed", "do_not_contact"].contains(&lead.status.as_str()) {
        return Err(AppError::BadRequest("They opted out of texts".into()));
    }
    let mut tx = state.db.begin().await?;
    let message = sqlx::query_as::<_, TextMessage>(
        "insert into text_messages (project_id, lead_id, segment_id, channel, kind, to_number, body, status, due_at, created_by)
         values ($1, $2, $3, $4, 'reply', $5, $6, 'approved', now(), $7) returning *",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(lead.segment_id)
    .bind(&channel)
    .bind(&lead.phone)
    .bind(text)
    .bind(auth.user_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("update leads set remind_at = null where id = $1 and remind_at <= now()").bind(lead.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(message))
}

/// The org whose account the post names, when its signature checks out against the URL Twilio was given.
async fn verified(state: &AppState, headers: &HeaderMap, params: &[(String, String)], url: &str) -> AppResult<Uuid> {
    let refused = || AppError::Forbidden("That didn't come from a connected Twilio account".into());
    let account_sid = params.iter().find(|(k, _)| k == "AccountSid").map(|(_, v)| v.as_str()).ok_or_else(refused)?;
    let (org_id, token_enc): (Uuid, String) = sqlx::query_as("select org_id, auth_token_enc from twilio_accounts where account_sid = $1")
        .bind(account_sid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(refused)?;
    let token = crypto::decrypt(&state.cfg.app_secret, &token_enc)?;
    let signature = headers.get("x-twilio-signature").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !twilio::verify(&token, url, params, signature) {
        return Err(refused());
    }
    Ok(org_id)
}

fn param<'a>(params: &'a [(String, String)], name: &str) -> &'a str {
    params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or_default()
}

/// An empty TwiML answer, so Twilio sends nothing back to the lead itself.
async fn inbound(State(state): State<AppState>, headers: HeaderMap, Form(params): Form<Vec<(String, String)>>) -> AppResult<impl IntoResponse> {
    let org_id = verified(&state, &headers, &params, &twilio::inbound_url(&state.cfg.public_url)).await?;
    let msg = Inbound {
        sid: param(&params, "MessageSid"),
        from: param(&params, "From"),
        body: param(&params, "Body"),
        media: param(&params, "NumMedia").parse::<u32>().is_ok_and(|n| n > 0),
    };
    if msg.sid.is_empty() {
        return Err(AppError::BadRequest("MessageSid is missing".into()));
    }
    twilio::on_inbound(&state, org_id, &msg).await?;
    Ok(([(header::CONTENT_TYPE, "text/xml")], "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response></Response>"))
}

async fn delivery(State(state): State<AppState>, headers: HeaderMap, Form(params): Form<Vec<(String, String)>>) -> AppResult<StatusCode> {
    let org_id = verified(&state, &headers, &params, &twilio::status_url(&state.cfg.public_url)).await?;
    twilio::on_status(&state.db, org_id, param(&params, "MessageSid"), param(&params, "MessageStatus"), param(&params, "ErrorCode")).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{Timelike, Utc};
    use sqlx::PgPool;

    use super::*;
    use crate::config::Config;
    use crate::crypto::sha256_hex;
    use crate::twilio::fake;

    const PUBLIC: &str = "https://splee.test";
    const SAM: &str = "+447700900123";

    fn config(fake_url: &str) -> Config {
        Config {
            database_url: String::new(),
            app_secret: "test-secret".into(),
            public_url: PUBLIC.into(),
            unsubscribe_url: String::new(),
            bind_addr: String::new(),
            agent_model: String::new(),
            agent_concurrency: 0,
            sending_enabled: false,
            bird_bin: None,
            open_signup: true,
            public_agent: false,
            porkbun_api_url: String::new(),
            twilio_api_url: fake_url.into(),
            operator_emails: Vec::new(),
            google: None,
            microsoft: None,
            tumblr: None,
            ipinfo_token: None,
            slack: None,
        }
    }

    struct Seeded {
        state: AppState,
        base: String,
        client: reqwest::Client,
        sent: fake::Sent,
        project: Uuid,
        /// Sam Lee in London, at Smile Dental, in a working campaign, emailed 3 days ago.
        sam: Uuid,
        /// A lead of the other organization, Beta, with Sam's number.
        beta_lead: Uuid,
    }

    impl Seeded {
        /// Acme has Twilio connected with the fake's account and template HXintro. Beta has no Twilio. Each has an owner
        /// signed in as session "acme" or "beta".
        async fn start(db: PgPool) -> Self {
            let (fake_url, sent) = fake::start().await;
            let state = AppState { db: db.clone(), cfg: Arc::new(config(&fake_url)), events: tokio::sync::broadcast::channel(1).0 };
            let mut projects = vec![];
            for name in ["acme", "beta"] {
                let org: Uuid = sqlx::query_scalar("insert into organizations (name, referral_code) values ($1, $1) returning id").bind(name).fetch_one(&db).await.unwrap();
                let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ($1, 'x') returning id")
                    .bind(format!("{name}@example.test"))
                    .fetch_one(&db)
                    .await
                    .unwrap();
                sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(user).execute(&db).await.unwrap();
                sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                    .bind(sha256_hex(name))
                    .bind(user)
                    .bind(org)
                    .execute(&db)
                    .await
                    .unwrap();
                let project: Uuid =
                    sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Fieldnote', '{\"country_code\": \"GB\"}') returning id")
                        .bind(org)
                        .fetch_one(&db)
                        .await
                        .unwrap();
                sqlx::query(
                    "insert into project_settings (project_id, paused, sender_name, send_days, send_start_hour, send_end_hour, text_steps)
                     values ($1, false, 'Alex', '{1,2,3,4,5,6,7}', 0, 24,
                             '[{\"channel\": \"sms\", \"delay_days\": 2}, {\"channel\": \"whatsapp\", \"delay_days\": 3}]')",
                )
                .bind(project)
                .execute(&db)
                .await
                .unwrap();
                projects.push((org, project));
            }
            let (acme, project) = projects[0];
            sqlx::query(
                "insert into twilio_accounts (org_id, account_sid, api_key_sid, api_secret_enc, auth_token_enc, sms_from, whatsapp_from,
                                              whatsapp_template_sid, whatsapp_template_body, whatsapp_template_vars)
                 values ($1, $2, $3, $4, $5, '+14155550100', '+14155550101', $6, 'Hi {{1}}, can I send {{2}} a video?', '{\"1\": \"first_name\", \"2\": \"company_name\"}')",
            )
            .bind(acme)
            .bind(fake::ACCOUNT)
            .bind(fake::KEY)
            .bind(crypto::encrypt("test-secret", "secret"))
            .bind(crypto::encrypt("test-secret", "token"))
            .bind(fake::TEMPLATE)
            .execute(&db)
            .await
            .unwrap();
            let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name, status) values ($1, 'Dentists', 'working') returning id")
                .bind(project)
                .fetch_one(&db)
                .await
                .unwrap();
            let sam = emailed_lead(&db, project, Some(segment), "Sam", SAM).await;
            let beta_lead = emailed_lead(&db, projects[1].1, None, "Bo", SAM).await;

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", crate::routes::router()).with_state(state.clone());
            tokio::spawn(async move { axum::serve(listener, app).await });
            Self { state, base, client: reqwest::Client::new(), sent, project, sam, beta_lead }
        }

        fn db(&self) -> &PgPool {
            &self.state.db
        }

        async fn call(&self, session: &str, method: reqwest::Method, path: &str, body: Value) -> (u16, Value) {
            let res = self.client.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session={session}")).json(&body).send().await.unwrap();
            (res.status().as_u16(), res.json().await.unwrap_or_default())
        }

        /// Posts form `params` to a Twilio webhook, signed with `token` for `signed_url`.
        async fn webhook(&self, path: &str, params: &[(&str, &str)], token: &str, signed_url: &str) -> u16 {
            let params: Vec<(String, String)> = params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            let res = self
                .client
                .post(format!("{}/twilio/{path}", self.base))
                .header("x-twilio-signature", twilio::signature(token, signed_url, &params))
                .form(&params)
                .send()
                .await
                .unwrap();
            res.status().as_u16()
        }

        async fn text_in(&self, sid: &str, from: &str, body: &str) -> u16 {
            let params = [("AccountSid", fake::ACCOUNT), ("MessageSid", sid), ("From", from), ("To", "+14155550100"), ("Body", body), ("NumMedia", "0")];
            self.webhook("inbound", &params, "token", &twilio::inbound_url(PUBLIC)).await
        }

        async fn texts(&self, lead: Uuid) -> Vec<TextMessage> {
            sqlx::query_as("select * from text_messages where lead_id = $1 order by created_at").bind(lead).fetch_all(self.db()).await.unwrap()
        }

        async fn lead_status(&self, lead: Uuid) -> String {
            sqlx::query_scalar("select status from leads where id = $1").bind(lead).fetch_one(self.db()).await.unwrap()
        }
    }

    /// A contacted lead at Smile Dental whose first email went out 3 days ago.
    async fn emailed_lead(db: &PgPool, project: Uuid, segment: Option<Uuid>, name: &str, phone: &str) -> Uuid {
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, segment_id, full_name, first_name, email, phone, company_name, status)
             values ($1, $2, $3, $3, $4, $5, 'Smile Dental', 'contacted') returning id",
        )
        .bind(project)
        .bind(segment)
        .bind(name)
        .bind(format!("{}@smile.test", name.to_lowercase()))
        .bind(phone)
        .fetch_one(db)
        .await
        .unwrap();
        sqlx::query(
            "insert into email_messages (project_id, lead_id, segment_id, direction, kind, status, subject, body, sent_at)
             values ($1, $2, $3, 'outbound', 'initial', 'sent', 'Hi', 'Hello', now() - interval '3 days')",
        )
        .bind(project)
        .bind(lead)
        .bind(segment)
        .execute(db)
        .await
        .unwrap();
        lead
    }

    /// An approved text to the lead that's due now.
    async fn approved(db: &PgPool, lead: Uuid, kind: &str, channel: &str, step: i32) -> Uuid {
        sqlx::query_scalar(
            "insert into text_messages (project_id, lead_id, channel, kind, step, to_number, body, status, due_at)
             select project_id, id, $2, $3, $4, phone, 'Hi Sam', 'approved', now() - interval '1 minute' from leads where id = $1 returning id",
        )
        .bind(lead)
        .bind(channel)
        .bind(kind)
        .bind(step)
        .fetch_one(db)
        .await
        .unwrap()
    }

    async fn set_window(db: &PgPool, project: Uuid, timezone: &str, start: u32) {
        sqlx::query("update project_settings set timezone = $2, send_start_hour = $3, send_end_hour = $3 + 1 where project_id = $1")
            .bind(project)
            .bind(timezone)
            .bind(start as i32)
            .execute(db)
            .await
            .unwrap();
    }

    fn hour_in(tz: Tz) -> u32 {
        Utc::now().with_timezone(&tz).hour()
    }

    use chrono_tz::Tz;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn connecting_checks_the_api_key_and_the_auth_token_with_twilio(db: PgPool) {
        let s = Seeded::start(db).await;
        use reqwest::Method;
        let good = json!({ "account_sid": fake::ACCOUNT, "api_key_sid": fake::KEY, "api_secret": "secret", "auth_token": "token", "sms_from": "+1 415 555 0100" });
        let (code, body) = s.call("acme", Method::PUT, "/organization/twilio", good.clone()).await;
        assert_eq!(code, 200, "{body}");
        assert_eq!((body["connection"]["account_name"].as_str(), body["connection"]["sms_from"].as_str()), (Some("Fieldnote"), Some("+14155550100")));
        // Reconnecting the same account keeps the template it had.
        assert_eq!(body["connection"]["whatsapp_template_sid"], fake::TEMPLATE);
        assert_eq!(body["inbound_url"], "https://splee.test/api/twilio/inbound");
        assert!(body["connection"].get("api_secret_enc").is_none() && body["connection"].get("auth_token_enc").is_none());

        let (code, body) = s.call("acme", Method::PUT, "/organization/twilio", with(&good, &[("api_secret", "nope")])).await;
        assert_eq!(code, 400);
        assert!(body["error"].as_str().unwrap().starts_with("Twilio didn't take the API key. Twilio turned the credentials down"), "{body}");
        let (code, body) = s.call("acme", Method::PUT, "/organization/twilio", with(&good, &[("auth_token", "nope")])).await;
        assert_eq!((code, body["error"].as_str().unwrap().starts_with("Twilio didn't take the auth token.")), (400, true), "{body}");
        let (code, _) = s.call("acme", Method::PUT, "/organization/twilio", with(&good, &[("sms_from", ""), ("whatsapp_from", "")])).await;
        assert_eq!(code, 400);
        let (code, _) = s.call("acme", Method::PUT, "/organization/twilio", with(&good, &[("account_sid", "AC123")])).await;
        assert_eq!(code, 400);
        // One Twilio account belongs to one organization, so its webhooks name one.
        let (code, _) = s.call("beta", Method::PUT, "/organization/twilio", good.clone()).await;
        assert_eq!(code, 409);

        let pick = |vars: Value| json!({ "template_sid": fake::TEMPLATE, "variables": vars });
        let (code, body) = s.call("acme", Method::PUT, "/organization/twilio/template", pick(json!({ "1": "first_name", "2": "password" }))).await;
        assert_eq!((code, body["error"].as_str()), (400, Some("Pick what fills {{2}}")));
        // Left out, the variables get the usual fields.
        let (code, body) = s.call("acme", Method::PUT, "/organization/twilio/template", json!({ "template_sid": fake::TEMPLATE })).await;
        assert_eq!((code, &body["connection"]["whatsapp_template_vars"]), (200, &json!({ "1": "first_name", "2": "company_name" })), "{body}");
        let (code, body) = s.call("acme", Method::PUT, "/organization/twilio/template", pick(json!({ "1": "full_name", "2": "city" }))).await;
        assert_eq!((code, &body["connection"]["whatsapp_template_vars"]), (200, &json!({ "1": "full_name", "2": "city" })), "{body}");
        let (code, body) = s.call("acme", Method::PUT, "/organization/twilio/template", pick(json!({ "1": "first_name", "2": "company_name", "3": "x" }))).await;
        assert_eq!(code, 200, "{body}");
        assert_eq!(body["connection"]["whatsapp_template_vars"], json!({ "1": "first_name", "2": "company_name" }));
        assert_eq!(body["connection"]["whatsapp_template_body"], "Hi {{1}}, it's Alex from Fieldnote. Can I send {{2}} a two-minute video?");
        let (code, _) = s.call("acme", Method::PUT, "/organization/twilio/template", json!({ "template_sid": "HX0123456789abcdef0123456789abcdee" })).await;
        assert_eq!(code, 400);

        let (code, body) = s.call("acme", Method::DELETE, "/organization/twilio", json!({})).await;
        assert_eq!((code, body["connection"].is_null()), (200, true));
    }

    /// `good` with some fields changed.
    fn with(good: &Value, changes: &[(&str, &str)]) -> Value {
        let mut body = good.clone();
        for (k, v) in changes {
            body[*k] = json!(v);
        }
        body
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn text_steps_follow_the_first_email_in_the_leads_own_send_window(db: PgPool) {
        let s = Seeded::start(db).await;
        // The SMS step is due 2 days after the email, so a write run is queued for Sam, and only once.
        twilio::tick(&s.state).await.unwrap();
        twilio::tick(&s.state).await.unwrap();
        let runs: Vec<Value> = sqlx::query_scalar("select input from agent_runs where kind = 'write_texts'").fetch_all(s.db()).await.unwrap();
        assert_eq!(runs, [json!({ "lead_ids": [s.sam] })]);
        // Beta's lead has no Twilio to text from.
        assert!(twilio::due_steps(s.db(), Some(&[s.beta_lead])).await.unwrap().is_empty());

        // What the writer saves. The campaign is working, so it's approved.
        let step = twilio::due_steps(s.db(), Some(&[s.sam])).await.unwrap().remove(0);
        assert_eq!((step.step, step.channel.as_str(), step.sending, step.first), (1, "sms", true, true));
        let lead: Lead = sqlx::query_as("select * from leads where id = $1").bind(s.sam).fetch_one(s.db()).await.unwrap();
        assert!(twilio::insert_step(s.db(), &step, &lead, "Hi Sam, Alex from Fieldnote here.", None).await.unwrap());
        assert!(!twilio::insert_step(s.db(), &step, &lead, "Again", None).await.unwrap());

        // Sam's number is in London. The project's own zone, Tokyo, is in its hours, but London isn't, so it waits.
        let tokyo = hour_in(Tz::Asia__Tokyo);
        set_window(s.db(), s.project, "Asia/Tokyo", tokyo.min(22)).await;
        if hour_in(Tz::Europe__London) != tokyo.min(22) {
            twilio::tick(&s.state).await.unwrap();
            assert_eq!(s.texts(s.sam).await[0].status, "approved");
            assert!(s.sent.lock().unwrap().is_empty());
        }
        set_window(s.db(), s.project, "Asia/Tokyo", hour_in(Tz::Europe__London).min(22)).await;
        if hour_in(Tz::Europe__London) <= 22 {
            twilio::tick(&s.state).await.unwrap();
            let text = &s.texts(s.sam).await[0];
            assert_eq!((text.status.as_str(), text.twilio_sid.as_deref()), ("sent", Some("SM00000000000000000000000000000001")));
            let sent = s.sent.lock().unwrap();
            assert_eq!((sent[0]["To"].as_str(), sent[0]["From"].as_str(), sent[0]["Body"].as_str()), (SAM, "+14155550100", "Hi Sam, Alex from Fieldnote here."));
        }

        // The WhatsApp step is 3 days after the text and outside any 24-hour window, so it's the template, filled in.
        sqlx::query("update text_messages set status = 'sent', sent_at = now() - interval '4 days' where lead_id = $1").bind(s.sam).execute(s.db()).await.unwrap();
        sqlx::query("update project_settings set send_start_hour = 0, send_end_hour = 24 where project_id = $1").bind(s.project).execute(s.db()).await.unwrap();
        twilio::tick(&s.state).await.unwrap();
        let texts = s.texts(s.sam).await;
        assert_eq!((texts[1].channel.as_str(), texts[1].step, texts[1].body.as_str()), ("whatsapp", 2, "Hi Sam, can I send Smile Dental a video?"));
        assert_eq!((texts[1].status.as_str(), texts[1].template_vars.clone()), ("sent", json!({ "1": "Sam", "2": "Smile Dental" })));
        let last = s.sent.lock().unwrap().last().unwrap().clone();
        assert_eq!((last["To"].as_str(), last["ContentSid"].as_str(), last.get("Body")), ("whatsapp:+447700900123", fake::TEMPLATE, None));
        // That was the last step.
        assert!(twilio::due_steps(s.db(), None).await.unwrap().iter().all(|d| d.lead_id != s.sam));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_daily_cap_holds_sequence_texts_but_not_replies(db: PgPool) {
        let s = Seeded::start(db).await;
        sqlx::query("update project_settings set daily_text_limit = 1 where project_id = $1").bind(s.project).execute(s.db()).await.unwrap();
        let ana = emailed_lead(s.db(), s.project, None, "Ana", "+447700900456").await;
        approved(s.db(), s.sam, "sequence", "sms", 1).await;
        approved(s.db(), ana, "sequence", "sms", 1).await;
        // Two passes at once still share the one text the cap allows.
        let (a, b) = tokio::join!(twilio::tick(&s.state), twilio::tick(&s.state));
        a.unwrap();
        b.unwrap();
        assert_eq!(s.sent.lock().unwrap().len(), 1);
        // A reply to someone who wrote back still goes out at the cap.
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, status, channel, body) values ($1, $2, 'inbound', 'inbound', 'received', 'sms', 'Hi')",
        )
        .bind(s.project)
        .bind(ana)
        .execute(s.db())
        .await
        .unwrap();
        approved(s.db(), ana, "reply", "sms", 0).await;
        twilio::tick(&s.state).await.unwrap();
        let statuses: Vec<String> = s.texts(ana).await.into_iter().map(|t| t.status).collect();
        // Ana wrote back, so her sequence text is off and the reply went.
        assert_eq!(statuses, ["cancelled", "sent"]);
        assert_eq!(s.sent.lock().unwrap().len(), 2);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn suppressed_numbers_and_leads_are_never_texted(db: PgPool) {
        let s = Seeded::start(db).await;
        let ana = emailed_lead(s.db(), s.project, None, "Ana", "+447700900456").await;
        let bo = emailed_lead(s.db(), s.project, None, "Bo", "+447700900789").await;
        sqlx::query("insert into suppressions (org_id, value, kind, reason) select org_id, $2, 'phone', 'manual' from projects where id = $1")
            .bind(s.project)
            .bind(SAM)
            .execute(s.db())
            .await
            .unwrap();
        sqlx::query("update leads set status = 'unsubscribed' where id = $1").bind(bo).execute(s.db()).await.unwrap();
        sqlx::query("insert into suppressions (org_id, value, kind, reason) select org_id, 'ana@smile.test', 'email', 'unsubscribe' from projects where id = $1")
            .bind(s.project)
            .execute(s.db())
            .await
            .unwrap();
        assert!(twilio::due_steps(s.db(), Some(&[s.sam])).await.unwrap().is_empty());
        for lead in [s.sam, ana, bo] {
            approved(s.db(), lead, "sequence", "sms", 1).await;
        }
        twilio::tick(&s.state).await.unwrap();
        assert!(s.sent.lock().unwrap().is_empty());
        for (lead, why) in [(s.sam, "On the do-not-contact list"), (ana, "On the do-not-contact list"), (bo, "Lead unsubscribed")] {
            let text = &s.texts(lead).await[0];
            assert_eq!((text.status.as_str(), text.error.as_str()), ("cancelled", why));
        }
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn whatsapp_takes_free_text_only_in_the_24_hours_after_they_wrote(db: PgPool) {
        let s = Seeded::start(db).await;
        let wrote = |ago: &'static str| {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, direction, kind, status, channel, body, created_at)
                 values ($1, $2, 'inbound', 'inbound', 'received', 'whatsapp', 'Tell me more', now() - $3::interval)",
            )
            .bind(s.project)
            .bind(s.sam)
            .bind(ago)
        };
        wrote("30 hours").execute(s.db()).await.unwrap();
        let (code, body) = s.call("acme", reqwest::Method::POST, &format!("/inbox/threads/{}/text", s.sam), json!({ "body": "Sure, here's how" })).await;
        assert_eq!(code, 400, "{body}");
        assert!(body["error"].as_str().unwrap().contains("more than 24 hours"));

        wrote("2 hours").execute(s.db()).await.unwrap();
        let (code, body) = s.call("acme", reqwest::Method::POST, &format!("/inbox/threads/{}/text", s.sam), json!({ "body": "Sure, here's how" })).await;
        assert_eq!((code, body["channel"].as_str(), body["kind"].as_str()), (200, Some("whatsapp"), Some("reply")), "{body}");
        // Replies skip the send window.
        sqlx::query("update project_settings set send_days = '{}' where project_id = $1").bind(s.project).execute(s.db()).await.unwrap();
        twilio::tick(&s.state).await.unwrap();
        {
            let sent = s.sent.lock().unwrap();
            assert_eq!((sent[0]["To"].as_str(), sent[0]["Body"].as_str(), sent[0].get("ContentSid")), ("whatsapp:+447700900123", "Sure, here's how", None));
        }

        // Free text queued while the window was open fails once it has closed, rather than going out against WhatsApp's rules.
        sqlx::query("delete from email_messages where lead_id = $1 and direction = 'inbound' and created_at > now() - interval '3 hours'").bind(s.sam).execute(s.db()).await.unwrap();
        let late = approved(s.db(), s.sam, "reply", "whatsapp", 0).await;
        twilio::tick(&s.state).await.unwrap();
        let text = s.texts(s.sam).await.into_iter().find(|t| t.id == late).unwrap();
        assert_eq!(text.status, "failed");
        assert!(text.error.starts_with("More than 24 hours passed"), "{}", text.error);
        assert_eq!(s.sent.lock().unwrap().len(), 1);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn twilio_webhooks_need_a_signature_for_the_public_url(db: PgPool) {
        let s = Seeded::start(db).await;
        let params = [("AccountSid", fake::ACCOUNT), ("MessageSid", "SMin1"), ("From", SAM), ("Body", "Hi")];
        let url = twilio::inbound_url(PUBLIC);
        assert_eq!(s.webhook("inbound", &params, "wrong", &url).await, 403);
        // Signed for the address the backend itself listens on, not the one Twilio was given.
        assert_eq!(s.webhook("inbound", &params, "token", &format!("{}/twilio/inbound", s.base)).await, 403);
        assert_eq!(s.webhook("inbound", &params, "token", &twilio::status_url(PUBLIC)).await, 403);
        let unknown = [("AccountSid", concat!("AC", "0000000000000000", "000000000000000f")), ("MessageSid", "SMin1"), ("From", SAM), ("Body", "Hi")];
        assert_eq!(s.webhook("inbound", &unknown, "token", &url).await, 403);
        let stored: i64 = sqlx::query_scalar("select count(*) from email_messages where direction = 'inbound'").fetch_one(s.db()).await.unwrap();
        assert_eq!(stored, 0);

        assert_eq!(s.webhook("inbound", &params, "token", &url).await, 200);
        let stored: i64 = sqlx::query_scalar("select count(*) from email_messages where direction = 'inbound'").fetch_one(s.db()).await.unwrap();
        assert_eq!(stored, 1);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn stop_puts_the_number_on_the_list_and_unsubscribes_the_lead(db: PgPool) {
        let s = Seeded::start(db).await;
        approved(s.db(), s.sam, "sequence", "sms", 1).await;
        assert_eq!(s.text_in("SMstop", SAM, " Stop ").await, 200);
        let suppressed: Vec<(String, String)> = sqlx::query_as("select kind, value::text from suppressions order by kind").fetch_all(s.db()).await.unwrap();
        assert_eq!(suppressed, [("email".to_string(), "sam@smile.test".to_string()), ("phone".to_string(), SAM.to_string())]);
        assert_eq!(s.lead_status(s.sam).await, "unsubscribed");
        assert_eq!(s.texts(s.sam).await[0].status, "cancelled");
        let (classification, handled): (Option<String>, bool) =
            sqlx::query_as("select classification, handled_at is not null from email_messages where twilio_sid = 'SMstop'").fetch_one(s.db()).await.unwrap();
        assert_eq!((classification.as_deref(), handled), (Some("unsubscribe"), true));
        let runs: i64 = sqlx::query_scalar("select count(*) from agent_runs").fetch_one(s.db()).await.unwrap();
        assert_eq!(runs, 0);
        // A number no lead has still goes on the list.
        assert_eq!(s.text_in("SMstop2", "+447700900999", "UNSUBSCRIBE").await, 200);
        let listed: bool = sqlx::query_scalar("select exists (select 1 from suppressions where value = '+447700900999' and kind = 'phone')").fetch_one(s.db()).await.unwrap();
        assert!(listed);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_text_reply_stops_every_sequence_and_lands_once(db: PgPool) {
        let s = Seeded::start(db).await;
        approved(s.db(), s.sam, "sequence", "sms", 1).await;
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, step, status) values ($1, $2, 'outbound', 'follow_up', 1, 'approved')")
            .bind(s.project)
            .bind(s.sam)
            .execute(s.db())
            .await
            .unwrap();
        sqlx::query("insert into linkedin_messages (lead_id, project_id, step, kind, body) values ($1, $2, 1, 'connect', 'Hi')")
            .bind(s.sam)
            .bind(s.project)
            .execute(s.db())
            .await
            .unwrap();
        for _ in 0..2 {
            assert_eq!(s.text_in("SMreply", "+44 7700 900123", "Sounds good, call me Tuesday").await, 200);
        }
        assert_eq!(s.lead_status(s.sam).await, "replied");
        let text = &s.texts(s.sam).await[0];
        assert_eq!((text.status.as_str(), text.error.as_str()), ("cancelled", "They replied by text"));
        let follow_up: String = sqlx::query_scalar("select status from email_messages where kind = 'follow_up'").fetch_one(s.db()).await.unwrap();
        let linkedin: String = sqlx::query_scalar("select status from linkedin_messages").fetch_one(s.db()).await.unwrap();
        assert_eq!((follow_up.as_str(), linkedin.as_str()), ("cancelled", "skipped"));
        // Twilio posted it twice. It's stored and classified once.
        let inbound: Vec<(Uuid, String, String)> =
            sqlx::query_as("select lead_id, channel, body from email_messages where direction = 'inbound'").fetch_all(s.db()).await.unwrap();
        assert_eq!(inbound, [(s.sam, "sms".to_string(), "Sounds good, call me Tuesday".to_string())]);
        let runs: i64 = sqlx::query_scalar("select count(*) from agent_runs where kind = 'classify_reply'").fetch_one(s.db()).await.unwrap();
        assert_eq!(runs, 1);
        // The inbox thread shows it, and the reply box answers by text.
        let (code, thread) = s.call("acme", reqwest::Method::GET, &format!("/inbox/threads/{}", s.sam), json!(null)).await;
        assert_eq!((code, thread["messages"].as_array().unwrap().iter().any(|m| m["channel"] == "sms")), (200, true));
        let (code, _) = s.call("acme", reqwest::Method::POST, &format!("/inbox/threads/{}/draft", s.sam), json!({})).await;
        assert_eq!(code, 400);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_status_callback_records_delivery_and_failures(db: PgPool) {
        let s = Seeded::start(db).await;
        let ana = emailed_lead(s.db(), s.project, None, "Ana", "+447700900456").await;
        for (lead, sid) in [(s.sam, "SMa"), (ana, "SMb")] {
            let id = approved(s.db(), lead, "sequence", "sms", 1).await;
            sqlx::query("update text_messages set status = 'sent', sent_at = now(), twilio_sid = $2 where id = $1").bind(id).bind(sid).execute(s.db()).await.unwrap();
        }
        let status = |sid: &'static str, status: &'static str, code: &'static str| {
            [("AccountSid", fake::ACCOUNT), ("MessageSid", sid), ("MessageStatus", status), ("ErrorCode", code)]
        };
        let url = twilio::status_url(PUBLIC);
        assert_eq!(s.webhook("status", &status("SMa", "undelivered", "30003"), "token", &url).await, 204);
        assert_eq!(s.webhook("status", &status("SMb", "delivered", ""), "token", &url).await, 204);
        // A late "sent" doesn't undo either, and a forged one is turned away.
        assert_eq!(s.webhook("status", &status("SMb", "sent", ""), "token", &url).await, 204);
        assert_eq!(s.webhook("status", &status("SMb", "failed", "30005"), "nope", &url).await, 403);
        let sam = &s.texts(s.sam).await[0];
        assert_eq!((sam.status.as_str(), sam.error.as_str()), ("failed", "The phone is off or out of reach (Twilio error 30003)"));
        assert_eq!(s.texts(ana).await[0].status, "delivered");
        // A failed text ends the sequence.
        assert!(twilio::due_steps(s.db(), Some(&[s.sam])).await.unwrap().is_empty());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn another_organizations_lead_is_refused(db: PgPool) {
        let s = Seeded::start(db).await;
        let text = approved(s.db(), s.sam, "sequence", "sms", 1).await;
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status, channel, body) values ($1, $2, 'inbound', 'inbound', 'received', 'sms', 'Hi')")
            .bind(s.project)
            .bind(s.sam)
            .execute(s.db())
            .await
            .unwrap();
        let (code, _) = s.call("beta", reqwest::Method::POST, &format!("/inbox/threads/{}/text", s.sam), json!({ "body": "Hi" })).await;
        assert_eq!(code, 404);
        let (code, _) = s.call("beta", reqwest::Method::PATCH, &format!("/texts/{text}"), json!({ "status": "cancelled" })).await;
        assert_eq!(code, 404);
        let (code, _) = s.call("beta", reqwest::Method::GET, &format!("/projects/{}/texts", s.project), json!(null)).await;
        assert_eq!(code, 404);
        let (code, body) = s.call("acme", reqwest::Method::PATCH, &format!("/texts/{text}"), json!({ "body": "Hi Sam, Alex here" })).await;
        assert_eq!((code, body["status"].as_str(), body["body"].as_str()), (200, Some("approved"), Some("Hi Sam, Alex here")), "{body}");
        // Beta's lead shares Sam's number, but a text to Acme's Twilio only reaches Acme's leads.
        assert_eq!(s.text_in("SMx", SAM, "Who is this?").await, 200);
        let beta_inbound: i64 = sqlx::query_scalar("select count(*) from email_messages where lead_id = $1 and direction = 'inbound'")
            .bind(s.beta_lead)
            .fetch_one(s.db())
            .await
            .unwrap();
        assert_eq!(beta_inbound, 0);
        assert_eq!(s.texts(s.sam).await[0].error, "They replied by text");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn imports_and_edits_keep_numbers_in_e164(db: PgPool) {
        let s = Seeded::start(db).await;
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(s.project).fetch_one(s.db()).await.unwrap();
        let row = |email: &str, phone: &str, country: &str| super::super::util_lead_import::NewLead {
            email: email.into(),
            phone: phone.into(),
            country_code: country.into(),
            ..Default::default()
        };
        let rows = vec![(2, row("a@x.test", "07700 900111", "")), (3, row("b@x.test", "(415) 555-0134", "US")), (4, row("c@x.test", "call me", ""))];
        super::super::util_lead_import::insert(s.db(), org, s.project, "import", rows).await.unwrap();
        let phones: Vec<String> = sqlx::query_scalar("select phone from leads where email like '%@x.test' order by email").fetch_all(s.db()).await.unwrap();
        assert_eq!(phones, ["+447700900111", "+14155550134", ""]);

        let (code, body) = s.call("acme", reqwest::Method::PATCH, &format!("/leads/{}", s.sam), json!({ "phone": "07700 900222" })).await;
        assert_eq!((code, body["phone"].as_str()), (200, Some("+447700900222")), "{body}");
        let (code, _) = s.call("acme", reqwest::Method::PATCH, &format!("/leads/{}", s.sam), json!({ "phone": "12" })).await;
        assert_eq!(code, 400);
    }
}
