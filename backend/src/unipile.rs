//! Unipile sends due LinkedIn steps from the user's own LinkedIn account: the connect note as an invitation, each message
//! once they've accepted. A step it can't send keeps the reason in send_error and stays on the LinkedIn page for the user.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use chrono_tz::Tz;
use reqwest::{StatusCode, Url};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::hooks::{self, Alert};
use crate::models::{LinkedinMessage, Ts};
use crate::routes::linkedin::mark;
use crate::state::AppState;

/// Sends per LinkedIn account a day, well under the 80 to 100 invitations LinkedIn takes from a paid account.
const DAILY_SENDS: i64 = 25;
/// How long a message waits for the lead to accept the connect note before it's left for the user.
const ACCEPT_DAYS: i64 = 21;
/// LinkedIn's limits, which pause the account's sending for an hour rather than failing the lead.
const ACCOUNT_LIMITS: [&str; 2] = ["errors/limit_exceeded", "errors/cannot_resend_yet"];

pub struct Unipile {
    base: String,
    token: String,
    account: String,
    client: reqwest::Client,
}

#[derive(Debug)]
enum Failure {
    /// Something about this lead, like a profile LinkedIn doesn't find. The step is left for the user.
    Lead { kind: String, reason: String },
    /// The account or Unipile, like an expired LinkedIn login or a rate limit. The account's sending stops for an hour.
    Account(anyhow::Error),
}

impl Failure {
    fn lead(reason: &str) -> Self {
        Failure::Lead { kind: String::new(), reason: reason.into() }
    }

    fn into_error(self) -> anyhow::Error {
        match self {
            Failure::Lead { reason, .. } => anyhow::anyhow!(reason),
            Failure::Account(e) => e,
        }
    }
}

/// What happened to a due step.
#[derive(Debug, PartialEq)]
enum Step {
    Sent,
    /// The invitation was already out, sent by hand.
    AlreadyInvited,
    /// A connect note for someone who's already a connection.
    Connected,
    /// A message for someone who hasn't accepted the connect note yet.
    Waiting,
}

/// The API base URL from the DSN on the Unipile dashboard, like api8.unipile.com:13851, with or without https:// or a
/// path. Only Unipile's own hosts are taken, so the token never goes anywhere else.
pub fn base_url(dsn: &str) -> Option<String> {
    let dsn = dsn.trim();
    let url = Url::parse(&if dsn.contains("://") { dsn.to_string() } else { format!("https://{dsn}") }).ok()?;
    let host = url.host_str()?;
    if url.scheme() != "https" || !(host == "unipile.com" || host.ends_with(".unipile.com")) {
        return None;
    }
    Some(match url.port() {
        Some(port) => format!("https://{host}:{port}/api/v1"),
        None => format!("https://{host}/api/v1"),
    })
}

/// The public id in a profile URL, like sam-lee in https://www.linkedin.com/in/sam-lee/.
fn profile_id(url: &str) -> Option<&str> {
    let id = url.split_once("linkedin.com/in/")?.1.split(['/', '?', '#']).next()?;
    (!id.is_empty()).then_some(id)
}

/// The LinkedIn account behind a Unipile token, checked before it's saved: the first one that's running, else the first.
pub async fn find_account(base: &str, token: &str) -> anyhow::Result<(String, String)> {
    let u = Unipile::new(base, token, "")?;
    let list = u.call(u.client.get(format!("{base}/accounts?limit=250"))).await.map_err(Failure::into_error)?;
    let linkedin: Vec<&Value> = list["items"].as_array().into_iter().flatten().filter(|a| a["type"] == "LINKEDIN").collect();
    let running = |a: &&&Value| a["sources"].as_array().is_some_and(|s| s.iter().all(|s| s["status"] == "OK"));
    let account = linkedin.iter().find(running).or(linkedin.first()).context("There's no LinkedIn account on it. Connect one in Unipile first")?;
    let id = account["id"].as_str().filter(|id| !id.is_empty()).context("Unipile didn't return the account's id")?;
    Ok((id.to_string(), account["name"].as_str().unwrap_or_default().to_string()))
}

impl Unipile {
    pub fn new(base: &str, token: &str, account: &str) -> reqwest::Result<Self> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
        Ok(Self { base: base.into(), token: token.into(), account: account.into(), client })
    }

    /// Unipile explains a failure in `detail` or `title`, and names it in `type`, like errors/resource_not_found.
    async fn call(&self, request: reqwest::RequestBuilder) -> Result<Value, Failure> {
        let res = request
            .header("X-API-KEY", &self.token)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| Failure::Account(e.without_url().into()))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        if status.is_success() {
            return Ok(body);
        }
        let reason = ["detail", "title"].iter().find_map(|k| body[k].as_str().filter(|r| !r.is_empty())).unwrap_or(status.as_str()).to_string();
        let kind = body["type"].as_str().unwrap_or_default().to_string();
        let about_lead = status == StatusCode::NOT_FOUND || (status == StatusCode::UNPROCESSABLE_ENTITY && !ACCOUNT_LIMITS.contains(&kind.as_str()));
        Err(if about_lead { Failure::Lead { kind, reason } } else { Failure::Account(anyhow::anyhow!("{reason}")) })
    }

    /// `invited_at` is when the lead's connect note went out, for a message step.
    async fn send(&self, step: &LinkedinMessage, url: &str, invited_at: Option<Ts>) -> Result<Step, Failure> {
        let id = profile_id(url).ok_or_else(|| Failure::lead("The LinkedIn URL isn't a profile link"))?;
        let profile = self.call(self.client.get(format!("{}/users/{id}?account_id={}", self.base, self.account))).await?;
        let provider_id = profile["provider_id"].as_str().ok_or_else(|| Failure::Account(anyhow::anyhow!("Unipile didn't return the profile's id")))?;
        let connected = profile["network_distance"] == "FIRST_DEGREE" || profile["is_relationship"] == true;

        if step.kind == "connect" {
            if connected {
                return Ok(Step::Connected);
            }
            if profile["invitation"]["type"] == "SENT" && profile["invitation"]["status"] == "PENDING" {
                return Ok(Step::AlreadyInvited);
            }
            let invite = json!({ "account_id": self.account, "provider_id": provider_id, "message": step.body.chars().take(300).collect::<String>() });
            return match self.call(self.client.post(format!("{}/users/invite", self.base)).json(&invite)).await {
                Err(Failure::Lead { kind, .. }) if kind == "errors/already_invited_recently" => Ok(Step::AlreadyInvited),
                result => result.map(|_| Step::Sent),
            };
        }

        if !connected {
            return match invited_at {
                Some(at) if at > Utc::now() - chrono::Duration::days(ACCEPT_DAYS) => Ok(Step::Waiting),
                Some(_) => Err(Failure::lead("They haven't accepted the connect note in 3 weeks")),
                None => Err(Failure::lead("You aren't connected on LinkedIn, and the connect note wasn't sent")),
            };
        }
        let form = reqwest::multipart::Form::new()
            .text("account_id", self.account.clone())
            .text("attendees_ids", provider_id.to_string())
            .text("text", step.body.clone());
        self.call(self.client.post(format!("{}/chats", self.base)).multipart(form)).await.map(|_| Step::Sent)
    }
}

#[derive(sqlx::FromRow)]
struct Sender {
    project_id: Uuid,
    unipile_dsn: String,
    unipile_token_enc: String,
    unipile_account_id: String,
    timezone: String,
    send_days: Vec<i32>,
    send_start_hour: i32,
    send_end_hour: i32,
}

/// Connected, unpaused projects whose LinkedIn account hasn't had a Unipile failure in the last hour.
async fn senders(db: &PgPool) -> sqlx::Result<Vec<Sender>> {
    sqlx::query_as::<_, Sender>(
        "select s.project_id, s.unipile_dsn, s.unipile_token_enc, s.unipile_account_id, s.timezone, s.send_days, s.send_start_hour,
                s.send_end_hour
         from project_settings s join projects p on p.id = s.project_id
         where s.unipile_token_enc <> '' and not s.paused and p.org_id is not null
           and not exists (select 1 from project_settings o where o.unipile_account_id = s.unipile_account_id
                           and o.unipile_failed_at > now() - interval '1 hour')",
    )
    .fetch_all(db)
    .await
}

/// One due step per connected project each tick, inside its send window, so a day's sends spread out instead of going
/// in a burst. A Unipile failure stops every project on that LinkedIn account for an hour.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    // Projects can share a LinkedIn account, and a failure on one holds them all for the hour.
    let mut failed = HashSet::new();
    for s in senders(&state.db).await? {
        if failed.contains(&s.unipile_account_id) {
            continue;
        }
        if !crate::mail::in_window(s.timezone.parse().unwrap_or(Tz::UTC), &s.send_days, s.send_start_hour, s.send_end_hour, Utc::now()) {
            continue;
        }
        let token = match crate::crypto::decrypt(&state.cfg.app_secret, &s.unipile_token_enc) {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!(project_id = %s.project_id, error = %e, "couldn't decrypt the Unipile token");
                continue;
            }
        };
        let unipile = Unipile::new(&s.unipile_dsn, &token, &s.unipile_account_id)?;
        let result = match send_next(&state.db, &unipile, s.project_id).await {
            Ok(None) => continue,
            Ok(Some(result)) => result,
            Err(e) => {
                tracing::error!(project_id = %s.project_id, error = ?e, "linkedin sending failed");
                continue;
            }
        };
        if result.is_err() {
            failed.insert(s.unipile_account_id.clone());
        }
        hooks::record(&state.db, s.project_id, Alert::Unipile, &s.unipile_token_enc, &result).await;
    }
    Ok(())
}

/// Sends the project's longest-due step. None when there's nothing to send, else whether Unipile worked.
async fn send_next(db: &PgPool, unipile: &Unipile, project: Uuid) -> anyhow::Result<Option<anyhow::Result<()>>> {
    let sent_today: i64 = sqlx::query_scalar(
        "select count(*) from linkedin_messages m join project_settings s on s.project_id = m.project_id
         where s.unipile_account_id = $1 and m.sent_via = 'unipile' and m.done_at > now() - interval '1 day'",
    )
    .bind(&unipile.account)
    .fetch_one(db)
    .await?;
    if sent_today >= DAILY_SENDS {
        return Ok(None);
    }
    let Some(step) = sqlx::query_as::<_, LinkedinMessage>(
        "select m.* from linkedin_messages m join leads l on l.id = m.lead_id
         where m.project_id = $1 and m.status = 'todo' and m.due_at <= now() and m.send_error is null
           and l.linkedin_url <> '' and l.status in ('new', 'queued', 'contacted')
         order by m.due_at limit 1",
    )
    .bind(project)
    .fetch_optional(db)
    .await?
    else {
        return Ok(None);
    };
    let url: String = sqlx::query_scalar("select linkedin_url from leads where id = $1").bind(step.lead_id).fetch_one(db).await?;
    let invited_at: Option<Ts> =
        sqlx::query_scalar("select max(done_at) from linkedin_messages where lead_id = $1 and kind = 'connect' and status = 'done'")
            .bind(step.lead_id)
            .fetch_one(db)
            .await?;

    match unipile.send(&step, &url, invited_at).await {
        Ok(Step::Sent) => {
            mark(db, &step, "done", None).await?;
            sqlx::query("update linkedin_messages set sent_via = 'unipile' where id = $1").bind(step.id).execute(db).await?;
        }
        Ok(Step::AlreadyInvited) => {
            mark(db, &step, "done", None).await?;
        }
        // Already connected, so the first message comes due now.
        Ok(Step::Connected) => {
            mark(db, &step, "skipped", None).await?;
        }
        Ok(Step::Waiting) => {
            sqlx::query("update linkedin_messages set due_at = now() + interval '1 day' where id = $1").bind(step.id).execute(db).await?;
        }
        Err(Failure::Lead { reason, .. }) => {
            sqlx::query("update linkedin_messages set send_error = $2 where id = $1").bind(step.id).bind(reason).execute(db).await?;
        }
        Err(Failure::Account(e)) => return Ok(Some(Err(e))),
    }
    Ok(Some(Ok(())))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::{Multipart, Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::{Json, Router};

    use super::*;

    #[test]
    fn only_unipile_hosts_are_taken() {
        let base = Some("https://api8.unipile.com:13851/api/v1".to_string());
        assert_eq!(base_url("api8.unipile.com:13851"), base);
        assert_eq!(base_url(" https://API8.unipile.com:13851/api/v1/accounts "), base);
        for dsn in ["http://api8.unipile.com:13851", "127.0.0.1:8080", "unipile.com.example.com", "https://example.com/unipile.com", ""] {
            assert_eq!(base_url(dsn), None, "{dsn}");
        }
    }

    #[test]
    fn profile_ids_come_from_the_url() {
        assert_eq!(profile_id("https://www.linkedin.com/in/sam-lee/"), Some("sam-lee"));
        assert_eq!(profile_id("linkedin.com/in/sam-lee?trk=x"), Some("sam-lee"));
        assert_eq!(profile_id("https://www.linkedin.com/company/acme"), None);
        assert_eq!(profile_id("https://www.linkedin.com/in/"), None);
    }

    type Calls = Arc<Mutex<Vec<String>>>;

    /// Unipile as the docs describe it: sam hasn't accepted, ana is a connection, anyone else isn't found.
    async fn fake_unipile() -> (String, Calls) {
        async fn user(Path(id): Path<String>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
            assert_eq!(headers["x-api-key"], "TOKEN");
            match id.as_str() {
                "sam" => (StatusCode::OK, Json(json!({ "provider_id": "P-SAM", "network_distance": "SECOND_DEGREE" }))),
                "ana" => (StatusCode::OK, Json(json!({ "provider_id": "P-ANA", "network_distance": "FIRST_DEGREE" }))),
                _ => (StatusCode::NOT_FOUND, Json(json!({ "status": 404, "type": "errors/resource_not_found", "title": "User not found" }))),
            }
        }
        async fn invite(State(calls): State<Calls>, Json(body): Json<Value>) -> Json<Value> {
            calls.lock().unwrap().push(format!("invite {} {}", body["provider_id"], body["message"]));
            Json(json!({ "object": "UserInvitationSent" }))
        }
        async fn chat(State(calls): State<Calls>, mut form: Multipart) -> Json<Value> {
            let mut fields = Vec::new();
            while let Some(field) = form.next_field().await.unwrap() {
                let name = field.name().unwrap().to_owned();
                fields.push(format!("{name}={}", field.text().await.unwrap()));
            }
            calls.lock().unwrap().push(format!("chat {}", fields.join(" ")));
            Json(json!({ "object": "ChatStarted", "chat_id": "c1" }))
        }
        let calls = Calls::default();
        let app = Router::new()
            .route("/api/v1/users/{id}", get(user))
            .route("/api/v1/users/invite", post(invite))
            .route("/api/v1/chats", post(chat))
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, calls)
    }

    async fn lead_with_sequence(db: &PgPool, project: Uuid, profile: &str) -> Vec<Uuid> {
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, linkedin_url, status) values ($1, $2, 'contacted') returning id")
            .bind(project)
            .bind(format!("https://www.linkedin.com/in/{profile}/"))
            .fetch_one(db)
            .await
            .unwrap();
        sqlx::query_scalar(
            "insert into linkedin_messages (lead_id, project_id, step, kind, body, due_at)
             select $1, $2, s, case when s = 1 then 'connect' else 'message' end, 'Hi ' || s, case when s = 1 then now() end
             from generate_series(1, 3) s returning id",
        )
        .bind(lead)
        .bind(project)
        .fetch_all(db)
        .await
        .unwrap()
    }

    async fn step(db: &PgPool, id: Uuid) -> LinkedinMessage {
        sqlx::query_as("select * from linkedin_messages where id = $1").bind(id).fetch_one(db).await.unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn due_steps_go_out_through_unipile_and_messages_wait_for_the_connection(db: PgPool) {
        let (base, calls) = fake_unipile().await;
        let unipile = Unipile::new(&base, "TOKEN", "ACC").unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let sent = || async { send_next(&db, &unipile, project).await.unwrap().map(|r| r.is_ok()) };

        // Sam's connect note goes out as an invitation, and his first message comes due 3 days later.
        let sam = lead_with_sequence(&db, project, "sam").await;
        assert_eq!(sent().await, Some(true));
        let (connect, message) = (step(&db, sam[0]).await, step(&db, sam[1]).await);
        assert_eq!((connect.status.as_str(), connect.sent_via.as_str()), ("done", "unipile"));
        assert!(message.due_at.unwrap() > Utc::now() + chrono::Duration::days(2));
        assert_eq!(sent().await, None);

        // He hasn't accepted, so the message waits a day instead of going out.
        sqlx::query("update linkedin_messages set due_at = now() where id = $1").bind(sam[1]).execute(&db).await.unwrap();
        assert_eq!(sent().await, Some(true));
        let message = step(&db, sam[1]).await;
        assert_eq!(message.status, "todo");
        assert!(message.due_at.unwrap() > Utc::now() + chrono::Duration::hours(23));

        // Ana is already a connection: her connect note is skipped and her message goes out as a chat.
        let ana = lead_with_sequence(&db, project, "ana").await;
        assert_eq!(sent().await, Some(true));
        assert_eq!(step(&db, ana[0]).await.status, "skipped");
        assert_eq!(sent().await, Some(true));
        assert_eq!(step(&db, ana[1]).await.sent_via, "unipile");

        // A profile LinkedIn doesn't find is left for the user, with the reason.
        let gone = lead_with_sequence(&db, project, "gone").await;
        assert_eq!(sent().await, Some(true));
        let failed = step(&db, gone[0]).await;
        assert_eq!((failed.status.as_str(), failed.send_error.as_deref()), ("todo", Some("User not found")));
        assert_eq!(sent().await, None);

        assert_eq!(*calls.lock().unwrap(), ["invite \"P-SAM\" \"Hi 1\"", "chat account_id=ACC attendees_ids=P-ANA text=Hi 2"]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_failure_holds_every_project_on_the_same_linkedin_account(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let mut projects = Vec::new();
        for account in ["ACC", "ACC", "OTHER"] {
            let project: Uuid =
                sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
            sqlx::query("insert into project_settings (project_id, paused, unipile_token_enc, unipile_account_id) values ($1, false, 'x', $2)")
                .bind(project)
                .bind(account)
                .execute(&db)
                .await
                .unwrap();
            projects.push(project);
        }
        let sending = || async { senders(&db).await.unwrap().into_iter().map(|s| s.project_id).collect::<Vec<_>>() };
        assert_eq!(sending().await.len(), 3);

        sqlx::query("update project_settings set unipile_failed_at = now() - interval '10 minutes' where project_id = $1")
            .bind(projects[0])
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(sending().await, [projects[2]]);
    }
}
