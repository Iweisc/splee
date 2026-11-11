//! Client report links on the Analytics page, and the public routes behind /report/{token} and the unsubscribe links in
//! the report emails. See crate::reports.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, patch};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::demo::client_ip;
use crate::auth::Auth;
use crate::crypto::{random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::reports::email::MAX_RECIPIENTS;
use crate::reports::{self, RANGES, ReportLink, SharedReport, UNSUBSCRIBE, recipient_from};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/report-links", get(list).post(create))
        .route("/report-links/{id}", patch(update).delete(revoke))
        .route("/shared-reports/{token}", get(show))
        .route("/report-unsubscribe/{token}", get(unsubscribe_status).post(unsubscribe))
}

#[derive(Serialize)]
pub(crate) struct Recipient {
    email: String,
    unsubscribed: bool,
}

#[derive(Serialize)]
pub(crate) struct LinkView {
    #[serde(flatten)]
    link: ReportLink,
    recipients: Vec<Recipient>,
}

/// The project's live links, newest first. `only` picks one of them.
async fn views(state: &AppState, org_id: Uuid, project_id: Option<Uuid>, only: Option<Uuid>) -> AppResult<Vec<LinkView>> {
    let links: Vec<ReportLink> = sqlx::query_as(
        "select * from report_links where org_id = $1 and ($2::uuid is null or project_id = $2) and ($3::uuid is null or id = $3)
           and revoked_at is null and (expires_at is null or expires_at > now())
         order by created_at desc",
    )
    .bind(org_id)
    .bind(project_id)
    .bind(only)
    .fetch_all(&state.db)
    .await?;
    let ids: Vec<Uuid> = links.iter().map(|l| l.id).collect();
    let recipients: Vec<(Uuid, String, bool)> =
        sqlx::query_as("select link_id, email, unsubscribed_at is not null from report_recipients where link_id = any($1) order by created_at, email")
            .bind(&ids)
            .fetch_all(&state.db)
            .await?;
    Ok(links
        .into_iter()
        .map(|link| LinkView {
            recipients: recipients.iter().filter(|r| r.0 == link.id).map(|(_, email, unsubscribed)| Recipient { email: email.clone(), unsubscribed: *unsubscribed }).collect(),
            link,
        })
        .collect())
}

pub(crate) async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<LinkView>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(views(&state, auth.org_id, Some(id), None).await?))
}

#[derive(Deserialize)]
#[serde(default)]
pub(crate) struct LinkBody {
    title: String,
    intro: String,
    brand: String,
    logo_url: String,
    range_preset: String,
    show_spend: bool,
    show_mailboxes: bool,
    show_leads: bool,
    email_schedule: String,
    recipients: Vec<String>,
    expires_at: Option<Ts>,
}

impl Default for LinkBody {
    fn default() -> Self {
        Self {
            title: String::new(),
            intro: String::new(),
            brand: "project".into(),
            logo_url: String::new(),
            range_preset: "last_30".into(),
            show_spend: false,
            show_mailboxes: false,
            show_leads: false,
            email_schedule: "none".into(),
            recipients: vec![],
            expires_at: None,
        }
    }
}

impl LinkBody {
    fn validate(&mut self) -> AppResult<()> {
        let bad = |message: &str| Err(AppError::BadRequest(message.into()));
        (self.title, self.intro, self.logo_url) = (self.title.trim().into(), self.intro.trim().into(), self.logo_url.trim().into());
        if self.title.chars().count() > 100 {
            return bad("Keep the title under 100 characters");
        }
        if self.intro.chars().count() > 1000 {
            return bad("Keep the note under 1,000 characters");
        }
        if !["project", "org"].contains(&self.brand.as_str()) {
            return bad("Show the project's name or the organization's");
        }
        if !self.logo_url.is_empty() && (!self.logo_url.starts_with("https://") || self.logo_url.len() > 500 || self.logo_url.contains(char::is_whitespace)) {
            return bad("Use a logo address that starts with https://");
        }
        if !RANGES.contains(&self.range_preset.as_str()) {
            return bad("Pick the last 7, 30 or 90 days, or the month so far");
        }
        if !["none", "weekly", "monthly"].contains(&self.email_schedule.as_str()) {
            return bad("Email the report weekly, monthly or not at all");
        }
        let mut emails = vec![];
        for raw in self.recipients.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
            let email = super::util_input::email(raw).ok_or_else(|| AppError::BadRequest(format!("{raw} isn't an email address")))?;
            if !emails.contains(&email) {
                emails.push(email);
            }
        }
        if emails.len() > MAX_RECIPIENTS {
            return bad("Email the report to 5 people at most");
        }
        if self.email_schedule != "none" && emails.is_empty() {
            return bad("Add who should get the email");
        }
        self.recipients = emails;
        if self.expires_at.is_some_and(|at| at <= Utc::now()) {
            return bad("Pick an expiry date in the future");
        }
        Ok(())
    }
}

/// Keeps the addresses still listed, with their unsubscribes, and adds the new ones.
async fn set_recipients(tx: &mut sqlx::PgConnection, link_id: Uuid, emails: &[String]) -> sqlx::Result<()> {
    sqlx::query("delete from report_recipients where link_id = $1 and email <> all($2)").bind(link_id).bind(emails).execute(&mut *tx).await?;
    sqlx::query("insert into report_recipients (link_id, email) select $1, unnest($2::text[]) on conflict (link_id, email) do nothing")
        .bind(link_id)
        .bind(emails)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// The token is in `url` and nowhere else: Splee keeps its hash, so the link can't be shown again.
pub(crate) async fn create(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(mut body): Json<LinkBody>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    body.validate()?;
    let token = random_token();
    let mut tx = state.db.begin().await?;
    let link_id: Uuid = sqlx::query_scalar(
        "insert into report_links (org_id, project_id, token_hash, title, intro, brand, logo_url, range_preset, show_spend, show_mailboxes, show_leads,
           email_schedule, expires_at, created_by)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) returning id",
    )
    .bind(auth.org_id)
    .bind(id)
    .bind(sha256_hex(&token))
    .bind(&body.title)
    .bind(&body.intro)
    .bind(&body.brand)
    .bind(&body.logo_url)
    .bind(&body.range_preset)
    .bind(body.show_spend)
    .bind(body.show_mailboxes)
    .bind(body.show_leads)
    .bind(&body.email_schedule)
    .bind(body.expires_at)
    .bind(auth.user_id)
    .fetch_one(&mut *tx)
    .await?;
    set_recipients(&mut tx, link_id, &body.recipients).await?;
    tx.commit().await?;
    let link = views(&state, auth.org_id, None, Some(link_id)).await?.pop().ok_or(AppError::NotFound)?;
    Ok(Json(json!({ "link": link, "url": format!("{}/report/{token}", state.cfg.public_url) })))
}

pub(crate) async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(mut body): Json<LinkBody>) -> AppResult<Json<LinkView>> {
    body.validate()?;
    let mut tx = state.db.begin().await?;
    let updated = sqlx::query(
        "update report_links set title = $3, intro = $4, brand = $5, logo_url = $6, range_preset = $7, show_spend = $8, show_mailboxes = $9,
           show_leads = $10, email_schedule = $11, expires_at = $12
         where id = $1 and org_id = $2 and revoked_at is null",
    )
    .bind(id)
    .bind(auth.org_id)
    .bind(&body.title)
    .bind(&body.intro)
    .bind(&body.brand)
    .bind(&body.logo_url)
    .bind(&body.range_preset)
    .bind(body.show_spend)
    .bind(body.show_mailboxes)
    .bind(body.show_leads)
    .bind(&body.email_schedule)
    .bind(body.expires_at)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    set_recipients(&mut tx, id, &body.recipients).await?;
    tx.commit().await?;
    Ok(Json(views(&state, auth.org_id, None, Some(id)).await?.pop().ok_or(AppError::NotFound)?))
}

/// Stops the link and its emails at once. The row stays, so its token can never open anything again.
pub(crate) async fn revoke(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let revoked = sqlx::query("update report_links set revoked_at = now() where id = $1 and org_id = $2 and revoked_at is null")
        .bind(id)
        .bind(auth.org_id)
        .execute(&state.db)
        .await?;
    if revoked.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}

const PUBLIC_PER_MINUTE: u32 = 60;

/// Plenty for a client opening the page and printing it, too few for a script trying tokens.
fn limit(headers: &HeaderMap, peer: SocketAddr) -> AppResult<()> {
    if crate::visitors::allow(&format!("report:{}", client_ip(headers, peer)), PUBLIC_PER_MINUTE) {
        Ok(())
    } else {
        Err(AppError::TooManyRequests("Too many requests. Try again in a minute.".into()))
    }
}

pub(crate) async fn show(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> AppResult<Json<SharedReport>> {
    limit(&headers, peer)?;
    let link = reports::by_token(&state, &token).await?;
    sqlx::query("update report_links set last_viewed_at = now() where id = $1").bind(link.id).execute(&state.db).await?;
    Ok(Json(reports::shared(&state.db, &link, Utc::now()).await?))
}

/// The recipient behind an unsubscribe token, and the title of the report it stops. A revoked link's still works.
async fn recipient(state: &AppState, token: &str) -> AppResult<(Uuid, String, String, bool)> {
    let id = recipient_from(&state.cfg.app_secret, UNSUBSCRIBE, token).ok_or(AppError::NotFound)?;
    sqlx::query_as(
        "select r.id, r.email, coalesce(nullif(l.title, ''), p.name || ' outreach report'), r.unsubscribed_at is not null
         from report_recipients r join report_links l on l.id = r.link_id join projects p on p.id = l.project_id where r.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)
}

// GET only looks the recipient up, since mail scanners open links. POST unsubscribes: the page's button, and the RFC 8058
// one-click POST mail clients send to List-Unsubscribe.
async fn unsubscribe_status(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> AppResult<Json<Value>> {
    limit(&headers, peer)?;
    let (_, email, title, unsubscribed) = recipient(&state, &token).await?;
    Ok(Json(json!({ "email": email, "title": title, "unsubscribed": unsubscribed })))
}

pub(crate) async fn unsubscribe(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> AppResult<Json<Value>> {
    limit(&headers, peer)?;
    let (id, email, title, _) = recipient(&state, &token).await?;
    sqlx::query("update report_recipients set unsubscribed_at = coalesce(unsubscribed_at, now()) where id = $1").bind(id).execute(&state.db).await?;
    Ok(Json(json!({ "email": email, "title": title, "unsubscribed": true })))
}
