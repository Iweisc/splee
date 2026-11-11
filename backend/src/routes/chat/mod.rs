//! The chat widget's public routes under /c/{key}, which a project's site calls, and the app's Chats tab and settings. The
//! key and the allowed domains are the visitors feature's, so one domain list covers both scripts. Every public request
//! needs an Origin on one of those domains, and only that origin gets CORS headers back.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State};
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_MAX_AGE, CACHE_CONTROL, CONTENT_TYPE, VARY,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use super::demo::client_ip;
use super::util_input;
use super::util_lead_import::{self, NewLead};
use super::util_pagination::{PageQuery, Paginated};
use super::visitors::{VisitorSite, intent_path, load_site, project_by_key};
use crate::auth::Auth;
use crate::chat::{self, Model, clean_text, cut};
use crate::crypto::{random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::hooks::{self, ChatAlert};
use crate::models::{Lead, Ts};
use crate::state::AppState;
use crate::visitors;

pub fn router() -> Router<AppState> {
    router_with(Arc::new(chat::Claude))
}

pub fn router_with(model: Arc<dyn Model>) -> Router<AppState> {
    let body = || DefaultBodyLimit::max(MAX_BODY);
    Router::new()
        // One parameter name for both, like the visitors snippet: the first one's is "{key}.js".
        .route("/c/{key}", get(script))
        .route("/c/{key}/config", get(config).options(preflight))
        .route("/c/{key}/messages", post(send).options(preflight).layer(body()))
        .route("/c/{key}/poll", post(poll).options(preflight).layer(body()))
        .route("/c/{key}/lead", post(lead).options(preflight).layer(body()))
        .route("/projects/{id}/chat", get(settings).put(update_settings))
        .route("/projects/{id}/chats", get(list))
        .route("/projects/{id}/chats/events", get(events))
        .route("/chats/{id}", get(thread))
        .route("/chats/{id}/reply", post(reply))
        .route("/chats/{id}/takeover", post(takeover))
        .layer(Extension(model))
}

const MAX_BODY: usize = 8 * 1024;
/// Messages and emails a minute from one IP, which someone typing stays well under.
const PER_IP: u32 = 10;
/// Messages a minute in one conversation.
const PER_CONVERSATION: u32 = 5;
/// Settings loads and polls a minute from one IP: a few open tabs polling every 4 seconds.
const POLLS_PER_IP: u32 = 120;
/// New conversations an hour from one IP, so clearing localStorage doesn't reset the per-conversation cap for long.
const CONVERSATIONS_PER_HOUR: i64 = 10;
/// Visitor messages in one conversation.
const MAX_VISITOR_MESSAGES: i64 = 40;
const MAX_TEAM_REPLY: usize = 2000;
const MAX_PATHS: usize = 20;
const MAX_GREETING: usize = 300;

const CAPPED: &str = "I can't answer more questions right now. Leave your email and someone from the team will get back to you.";
const NO_ANSWER: &str = "Sorry, I couldn't answer that just now. Leave your email and someone from the team will get back to you.";

/// The same script for every site, so browsers can keep it for an hour.
async fn script(State(state): State<AppState>, Path(file): Path<String>) -> AppResult<Response> {
    let key = file.strip_suffix(".js").ok_or(AppError::NotFound)?;
    project_by_key(&state.db, key).await?.ok_or(AppError::NotFound)?;
    let headers = [(CONTENT_TYPE, "text/javascript; charset=utf-8"), (CACHE_CONTROL, "public, max-age=3600")];
    Ok((headers, chat::WIDGET).into_response())
}

#[derive(Serialize, Deserialize, FromRow, Debug, Clone)]
pub struct ChatSettings {
    enabled: bool,
    accent: String,
    greeting: String,
    position: String,
    paths: Vec<String>,
    faq: String,
    daily_messages: i32,
    monthly_budget_micros: i64,
}

const SETTINGS: &str = "select enabled, accent, greeting, position, paths, faq, daily_messages, monthly_budget_micros from chat_settings where project_id = $1";

/// The name in the widget's header.
const TITLE: &str = "coalesce(nullif(company ->> 'name', ''), nullif(name, ''), domain, '')";

/// The project behind a public request's key, when the request comes from one of its domains.
struct Widget {
    project_id: Uuid,
    org_id: Uuid,
    project_name: String,
    /// None until the project saves its chat settings, which is off.
    settings: Option<ChatSettings>,
    origin: HeaderValue,
}

impl Widget {
    fn live(&self) -> AppResult<&ChatSettings> {
        self.settings.as_ref().filter(|s| s.enabled).ok_or_else(|| AppError::Forbidden("Chat is turned off on this site".into()))
    }
}

/// The Origin header and its host. Browsers always send one on the widget's requests, which are all cross-origin.
fn origin(headers: &HeaderMap) -> Option<(HeaderValue, String)> {
    let value = headers.get("origin").filter(|v| !v.is_empty() && v.as_bytes() != b"null")?;
    let url = reqwest::Url::parse(value.to_str().ok()?).ok().filter(|u| matches!(u.scheme(), "http" | "https"))?;
    Some((value.clone(), url.host_str()?.to_lowercase()))
}

async fn widget(db: &PgPool, key: &str, headers: &HeaderMap) -> AppResult<Widget> {
    let site = project_by_key(db, key).await?.ok_or(AppError::NotFound)?;
    let (origin, host) = origin(headers).ok_or_else(|| AppError::Forbidden("Open the chat from your website".into()))?;
    if !visitors::allowed_host(&site.domains, &host) {
        return Err(AppError::Forbidden("This site isn't one of the project's domains".into()));
    }
    let (org_id, project_name): (Uuid, String) =
        sqlx::query_as(&format!("select org_id, {TITLE} from projects where id = $1")).bind(site.project_id).fetch_one(db).await?;
    let settings = sqlx::query_as(SETTINGS).bind(site.project_id).fetch_optional(db).await?;
    Ok(Widget { project_id: site.project_id, org_id, project_name, settings, origin })
}

fn with_cors(origin: &HeaderValue, mut res: Response) -> Response {
    let headers = res.headers_mut();
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    headers.insert(VARY, HeaderValue::from_static("Origin"));
    res
}

/// Runs a public route for an allowed origin and adds its CORS headers, errors included, so the widget can show them.
async fn public<F, Fut>(state: &AppState, key: &str, headers: &HeaderMap, handle: F) -> Response
where
    F: FnOnce(Widget) -> Fut,
    Fut: Future<Output = AppResult<Json<Value>>>,
{
    match widget(&state.db, key, headers).await {
        Ok(w) => {
            let origin = w.origin.clone();
            with_cors(&origin, handle(w).await.into_response())
        }
        Err(e) => e.into_response(),
    }
}

/// The widget's requests never need one, but a site that wraps fetch with its own headers would.
async fn preflight(State(state): State<AppState>, Path(key): Path<String>, headers: HeaderMap) -> Response {
    match widget(&state.db, &key, &headers).await {
        Ok(w) => {
            let allow = [
                (ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST")),
                (ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("content-type")),
                (ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400")),
            ];
            with_cors(&w.origin, (StatusCode::NO_CONTENT, allow).into_response())
        }
        Err(e) => e.into_response(),
    }
}

fn visitor_ip(headers: &HeaderMap, peer: SocketAddr, state: &AppState) -> AppResult<String> {
    let ip: IpAddr = client_ip(headers, peer).parse().map_err(|_| AppError::BadRequest("Couldn't read your address".into()))?;
    Ok(visitors::ip_hash(&state.cfg.app_secret, ip))
}

fn limit(key: &str, per_minute: u32) -> AppResult<()> {
    if visitors::allow(key, per_minute) {
        return Ok(());
    }
    Err(AppError::TooManyRequests("You're sending messages quickly. Wait a minute and try again.".into()))
}

/// The widget posts text/plain so browsers skip the preflight, so the JSON is read by hand.
fn parse<T: DeserializeOwned>(body: &Bytes) -> AppResult<T> {
    serde_json::from_slice(body).map_err(|_| AppError::BadRequest("Send the message as JSON".into()))
}

async fn config(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, Path(key): Path<String>, headers: HeaderMap) -> Response {
    let ip_hash = visitor_ip(&headers, peer, &state);
    public(&state, &key, &headers, |w| async move {
        limit(&format!("chat-poll:{}", ip_hash?), POLLS_PER_IP)?;
        let Some(s) = w.settings.filter(|s| s.enabled) else { return Ok(Json(json!({"enabled": false}))) };
        Ok(Json(json!({
            "enabled": true, "title": w.project_name, "accent": s.accent, "greeting": s.greeting, "position": s.position,
            "paths": s.paths, "max_length": chat::MAX_LENGTH,
        })))
    })
    .await
}

#[derive(FromRow)]
struct Conversation {
    id: Uuid,
    domain: Option<String>,
    visitor_id: String,
    ip_hash: String,
    taken_over: bool,
}

const CONVERSATION: &str = "select id, domain::text, visitor_id, ip_hash, taken_over_at is not null as taken_over from chat_conversations";

/// The visitor's conversation, which only the token the widget got when it started opens.
async fn conversation(db: &PgPool, project_id: Uuid, id: Uuid, token: &str) -> AppResult<Conversation> {
    sqlx::query_as(&format!("{CONVERSATION} where id = $1 and project_id = $2 and token_hash = $3"))
        .bind(id)
        .bind(project_id)
        .bind(sha256_hex(token))
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// The company the visitors feature put behind this browser or network lately, if any.
async fn visiting_domain(db: &PgPool, project_id: Uuid, visitor_id: &str, ip_hash: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "select domain::text from visitor_hits
         where project_id = $1 and domain is not null and ((visitor_id <> '' and visitor_id = $2) or ip_hash = $3)
           and created_at > now() - interval '30 days'
         order by created_at desc limit 1",
    )
    .bind(project_id)
    .bind(visitor_id)
    .bind(ip_hash)
    .fetch_optional(db)
    .await
}

/// A message as the widget shows it. `who` is the first name of a teammate who answered.
#[derive(Serialize, FromRow, Debug)]
struct WidgetMessage {
    id: i64,
    role: String,
    body: String,
    who: String,
}

async fn widget_messages(db: &PgPool, conversation_id: Uuid, after: i64) -> sqlx::Result<Vec<WidgetMessage>> {
    sqlx::query_as(
        "select m.id, m.role, m.body, case when m.role = 'team' then coalesce(nullif(split_part(u.name, ' ', 1), ''), 'Team') else '' end as who
         from chat_messages m left join users u on u.id = m.user_id
         where m.conversation_id = $1 and m.id > $2 order by m.id limit 100",
    )
    .bind(conversation_id)
    .bind(after)
    .fetch_all(db)
    .await
}

async fn add_message(
    db: &PgPool,
    project_id: Uuid,
    conversation_id: Uuid,
    role: &str,
    body: &str,
    cost_micros: i64,
    user_id: Option<Uuid>,
) -> sqlx::Result<i64> {
    let id = sqlx::query_scalar(
        "insert into chat_messages (conversation_id, project_id, role, body, cost_micros, user_id) values ($1, $2, $3, $4, $5, $6) returning id",
    )
    .bind(conversation_id)
    .bind(project_id)
    .bind(role)
    .bind(body)
    .bind(cost_micros)
    .bind(user_id)
    .fetch_one(db)
    .await?;
    sqlx::query("update chat_conversations set last_message_at = now() where id = $1").bind(conversation_id).execute(db).await?;
    Ok(id)
}

/// Today's model answers and this month's spend, in UTC.
#[derive(Serialize, FromRow, Debug)]
struct Usage {
    today_messages: i64,
    month_spend_micros: i64,
}

/// Holds a daily slot and the most one answer may cost, so a burst of messages can't pass either cap. None when a cap is
/// reached. The caller deletes the hold once the answer's real cost is saved. Holds older than ANSWER_HOLD are a crashed call's.
async fn reserve(db: &PgPool, project_id: Uuid, settings: &ChatSettings) -> sqlx::Result<Option<Uuid>> {
    let mut tx = db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('chat-caps:' || $1::text, 0))").bind(project_id).execute(&mut *tx).await?;
    let used = usage(&mut *tx, project_id).await?;
    let hold = chat::ANSWER_HOLD_MICROS;
    if used.today_messages >= i64::from(settings.daily_messages) || used.month_spend_micros + hold > settings.monthly_budget_micros {
        return Ok(None);
    }
    let id = sqlx::query_scalar("insert into chat_reservations (project_id, micros) values ($1, $2) returning id")
        .bind(project_id)
        .bind(hold)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(id))
}

async fn usage<'e, E: sqlx::PgExecutor<'e>>(db: E, project_id: Uuid) -> sqlx::Result<Usage> {
    sqlx::query_as(
        "with held as (select count(*) as n, coalesce(sum(micros), 0)::bigint as micros from chat_reservations
                       where project_id = $1 and created_at > now() - interval '10 minutes')
         select (select count(*) from chat_messages where project_id = $1 and role = 'bot'
                   and created_at >= date_trunc('day', now() at time zone 'utc') at time zone 'utc') + held.n as today_messages,
                (select coalesce(sum(cost_micros), 0)::bigint from chat_messages where project_id = $1
                   and created_at >= date_trunc('month', now() at time zone 'utc') at time zone 'utc') + held.micros as month_spend_micros
         from held",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
}

#[derive(Deserialize)]
struct SendBody {
    conversation: Option<Uuid>,
    token: Option<String>,
    text: String,
    #[serde(default)]
    page: String,
    #[serde(default)]
    visitor: String,
}

async fn send(
    State(state): State<AppState>,
    Extension(model): Extension<Arc<dyn Model>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (ip_hash, st) = (visitor_ip(&headers, peer, &state), &state);
    public(st, &key, &headers, |w| async move { answer(st, model.as_ref(), &w, &ip_hash?, parse(&body)?).await }).await
}

/// Stores the visitor's message and answers it, unless someone on the team took the conversation over. One model call, when
/// the project's daily message cap and monthly spend cap both have room.
async fn answer(state: &AppState, model: &dyn Model, w: &Widget, ip_hash: &str, body: SendBody) -> AppResult<Json<Value>> {
    let settings = w.live()?;
    let db = &state.db;
    let text = clean_text(&body.text);
    if text.is_empty() {
        return Err(AppError::BadRequest("Type a message first".into()));
    }
    if text.chars().count() > chat::MAX_LENGTH {
        return Err(AppError::BadRequest(format!("Keep your message under {} characters", chat::MAX_LENGTH)));
    }
    limit(&format!("chat-ip:{ip_hash}"), PER_IP)?;
    let page = visitors::clean_path(&body.page);
    let (conv, token) = match (body.conversation, body.token.as_deref()) {
        (Some(id), Some(token)) => (conversation(db, w.project_id, id, token).await?, None),
        _ => {
            let recent: i64 = sqlx::query_scalar("select count(*) from chat_conversations where ip_hash = $1 and created_at > now() - interval '1 hour'")
                .bind(ip_hash)
                .fetch_one(db)
                .await?;
            if recent >= CONVERSATIONS_PER_HOUR {
                return Err(AppError::TooManyRequests("You've started a lot of chats. Try again in an hour.".into()));
            }
            let visitor = cut(&body.visitor, 100);
            let domain = visiting_domain(db, w.project_id, &visitor, ip_hash).await?;
            let token = random_token();
            let id: Uuid = sqlx::query_scalar(
                "insert into chat_conversations (project_id, token_hash, ip_hash, visitor_id, domain, page) values ($1, $2, $3, $4, $5, $6) returning id",
            )
            .bind(w.project_id)
            .bind(sha256_hex(&token))
            .bind(ip_hash)
            .bind(&visitor)
            .bind(&domain)
            .bind(&page)
            .fetch_one(db)
            .await?;
            (Conversation { id, domain, visitor_id: visitor, ip_hash: ip_hash.to_string(), taken_over: false }, Some(token))
        }
    };
    limit(&format!("chat-conversation:{}", conv.id), PER_CONVERSATION)?;
    let sent: i64 = sqlx::query_scalar("select count(*) from chat_messages where conversation_id = $1 and role = 'visitor'")
        .bind(conv.id)
        .fetch_one(db)
        .await?;
    if sent >= MAX_VISITOR_MESSAGES {
        return Err(AppError::TooManyRequests("This chat is full. Leave your email and someone from the team will get back to you.".into()));
    }
    sqlx::query("update chat_conversations set page = $2 where id = $1").bind(conv.id).bind(&page).execute(db).await?;
    let first = add_message(db, w.project_id, conv.id, "visitor", &text, 0, None).await?;
    chat::changed(state, w.project_id, conv.id);

    let mut ask_email = false;
    if !conv.taken_over {
        let reserved = reserve(db, w.project_id, settings).await?;
        let reply = match reserved {
            Some(_) => Some(ask_model(state, model, w, settings, conv.id).await),
            None => None,
        };
        let (role, body, cost) = match reply {
            Some(Ok((Some(r), cost))) if !r.reply.trim().is_empty() => {
                ask_email = !r.answered || r.interested;
                ("bot", cut(&clean_text(&r.reply), chat::MAX_LENGTH), micros(cost))
            }
            Some(Ok((_, cost))) => ("notice", NO_ANSWER.to_string(), micros(cost)),
            Some(Err(e)) => {
                tracing::warn!(error = ?e, project = %w.project_id, "couldn't answer a chat message");
                ("notice", NO_ANSWER.to_string(), 0)
            }
            None => ("notice", CAPPED.to_string(), 0),
        };
        // One transaction saves the cost and drops the hold, so reserve() always sees one of them. The row lock waits out a
        // takeover committing now. If a teammate took over while the model was answering, their reply stands and the answer
        // only leaves its cost, on the visitor's message, for the caps.
        let mut tx = db.begin().await?;
        let taken_over: bool = sqlx::query_scalar("select taken_over_at is not null from chat_conversations where id = $1 for update")
            .bind(conv.id)
            .fetch_one(&mut *tx)
            .await?;
        if taken_over {
            sqlx::query("update chat_messages set cost_micros = $2 where id = $1").bind(first).bind(cost).execute(&mut *tx).await?;
        } else {
            sqlx::query("insert into chat_messages (conversation_id, project_id, role, body, cost_micros) values ($1, $2, $3, $4, $5)")
                .bind(conv.id)
                .bind(w.project_id)
                .bind(role)
                .bind(&body)
                .bind(cost)
                .execute(&mut *tx)
                .await?;
            sqlx::query("update chat_conversations set last_message_at = now() where id = $1").bind(conv.id).execute(&mut *tx).await?;
        }
        if let Some(id) = reserved {
            sqlx::query("delete from chat_reservations where id = $1").bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        if taken_over {
            let messages = widget_messages(db, conv.id, first - 1).await?;
            return Ok(Json(json!({"conversation": conv.id, "token": token, "messages": messages, "ask_email": false})));
        }
        ask_email |= role == "notice";
        chat::changed(state, w.project_id, conv.id);
    }
    let messages = widget_messages(db, conv.id, first - 1).await?;
    Ok(Json(json!({"conversation": conv.id, "token": token, "messages": messages, "ask_email": ask_email})))
}

fn micros(usd: f64) -> i64 {
    (usd * 1_000_000.0).round() as i64
}

async fn ask_model(
    state: &AppState,
    model: &dyn Model,
    w: &Widget,
    settings: &ChatSettings,
    conversation_id: Uuid,
) -> anyhow::Result<(Option<chat::Reply>, f64)> {
    let facts = chat::facts(&state.db, w.project_id, &settings.faq).await?;
    let mut history: Vec<(String, String)> = sqlx::query_as(
        "select role, body from chat_messages where conversation_id = $1 and role in ('visitor', 'bot', 'team') order by id desc limit $2",
    )
    .bind(conversation_id)
    .bind(chat::HISTORY)
    .fetch_all(&state.db)
    .await?;
    history.reverse();
    let system = chat::system_prompt(&w.project_name, &facts);
    model.answer(state, &system, &chat::transcript(&history)).await
}

#[derive(Deserialize)]
struct PollBody {
    conversation: Uuid,
    token: String,
    #[serde(default)]
    after: i64,
}

/// The messages after `after`, which is how a teammate's replies reach the widget.
async fn poll(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, Path(key): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    let (ip_hash, db) = (visitor_ip(&headers, peer, &state), &state.db);
    public(&state, &key, &headers, |w| async move {
        limit(&format!("chat-poll:{}", ip_hash?), POLLS_PER_IP)?;
        let body: PollBody = parse(&body)?;
        let conv = conversation(db, w.project_id, body.conversation, &body.token).await?;
        Ok(Json(json!({"messages": widget_messages(db, conv.id, body.after).await?})))
    })
    .await
}

#[derive(Deserialize)]
struct LeadBody {
    conversation: Uuid,
    token: String,
    email: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    company: String,
}

async fn lead(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, Path(key): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    let (ip_hash, st) = (visitor_ip(&headers, peer, &state), &state);
    public(st, &key, &headers, |w| async move { capture(st, &w, &ip_hash?, parse(&body)?).await }).await
}

/// The project's booking link if it set one, else the organization's first booking page.
async fn booking_url(state: &AppState, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Option<String>> {
    let link: Option<String> = sqlx::query_scalar("select nullif(trim(booking_link), '') from project_settings where project_id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
        .flatten();
    if link.is_some() {
        return Ok(link);
    }
    let slug: Option<String> =
        sqlx::query_scalar("select slug from booking_pages where org_id = $1 order by created_at limit 1").bind(org_id).fetch_optional(&state.db).await?;
    Ok(slug.map(|slug| format!("{}/book/{slug}", state.cfg.public_url)))
}

/// Makes the visitor a hot lead, or updates the lead they already are, with the conversation in its notes and their company
/// from the visitors feature when it's known. An address or domain on the do-not-contact list is kept on the conversation
/// only. Thanks them with the booking link when there is one and tells the team in chat and the hot_lead webhook.
async fn capture(state: &AppState, w: &Widget, ip_hash: &str, body: LeadBody) -> AppResult<Json<Value>> {
    w.live()?;
    let db = &state.db;
    let email = util_input::email(&body.email).ok_or_else(|| AppError::BadRequest("Enter a valid email address".into()))?;
    let name = cut(&clean_text(&body.name), 200);
    let company = cut(&clean_text(&body.company), 200);
    let conv = conversation(db, w.project_id, body.conversation, &body.token).await?;
    limit(&format!("chat-ip:{ip_hash}"), PER_IP)?;
    let domain = match conv.domain.clone() {
        Some(domain) => Some(domain),
        None => visiting_domain(db, w.project_id, &conv.visitor_id, &conv.ip_hash).await?,
    };
    let email_domain = email.rsplit('@').next().unwrap_or_default().to_string();
    let suppressed: bool = sqlx::query_scalar("select exists (select 1 from suppressions where org_id = $1 and value = any($2::citext[]))")
        .bind(w.org_id)
        .bind(vec![email.clone(), email_domain])
        .fetch_one(db)
        .await?;
    let lead: Option<Lead> = if suppressed {
        None
    } else {
        let known: String = sqlx::query_scalar(
            "select coalesce((select nullif(name, '') from companies where project_id = $1 and domain = $2::citext),
                             (select name from directory_companies where domain = lower($2)), '')",
        )
        .bind(w.project_id)
        .bind(&domain)
        .fetch_one(db)
        .await?;
        let new = NewLead {
            email: email.clone(),
            full_name: name.clone(),
            company_name: if company.is_empty() { known } else { company.clone() },
            company_domain: domain.clone().unwrap_or_default(),
            ..Default::default()
        };
        util_lead_import::insert(db, w.org_id, w.project_id, "chat", vec![(1, new)]).await?;
        let notes = format!("Website chat on {}:\n{}", chrono::Utc::now().format("%-d %b %Y"), transcript_note(db, conv.id).await?);
        sqlx::query_as(
            "update leads set
               full_name = case when full_name = '' then $3 else full_name end,
               company_name = case when company_name = '' then $4 else company_name end,
               company_domain = coalesce(company_domain, $5::citext),
               company_id = coalesce(company_id,
                 (select c.id from companies c where c.project_id = $1 and c.domain = coalesce(leads.company_domain, $5::citext))),
               status = case when status in ('new', 'queued', 'contacted', 'replied') then 'hot' else status end,
               notes = trim(notes || E'\\n\\n' || $6)
             where project_id = $1 and email = $2::citext returning *",
        )
        .bind(w.project_id)
        .bind(&email)
        .bind(&name)
        .bind(&company)
        .bind(&domain)
        .bind(&notes)
        .fetch_optional(db)
        .await?
    };
    sqlx::query(
        "update chat_conversations set email = $2, name = case when $3 = '' then name else $3 end, company = case when $4 = '' then company else $4 end,
           domain = coalesce(domain, $5::citext), lead_id = coalesce($6, lead_id) where id = $1",
    )
    .bind(conv.id)
    .bind(&email)
    .bind(&name)
    .bind(&company)
    .bind(&domain)
    .bind(lead.as_ref().map(|l| l.id))
    .execute(db)
    .await?;

    let greeting = if name.is_empty() { "Thanks!".to_string() } else { format!("Thanks, {}!", name.split_whitespace().next().unwrap_or(&name)) };
    let mut thanks = format!("{greeting} Someone from the team will email you at {email}.");
    if let Some(url) = booking_url(state, w.project_id, w.org_id).await? {
        thanks.push_str(&format!(" If you'd like to pick a time now, book a call here: {url}"));
    }
    let id = add_message(db, w.project_id, conv.id, "notice", &thanks, 0, None).await?;
    chat::changed(state, w.project_id, conv.id);
    if let Some(mut lead) = lead {
        lead.owner_id = lead.owner_id.or(crate::ownership::claim(db, lead.id).await);
        announce(state, w, conv.id, lead);
    }
    Ok(Json(json!({"messages": widget_messages(db, conv.id, id - 1).await?})))
}

/// The conversation without Splee's own notices, oldest first, as (role, body).
async fn exchange(db: &PgPool, conversation_id: Uuid) -> sqlx::Result<Vec<(String, String)>> {
    sqlx::query_as("select role, body from chat_messages where conversation_id = $1 and role <> 'notice' order by id")
        .bind(conversation_id)
        .fetch_all(db)
        .await
}

/// The conversation as lead notes.
async fn transcript_note(db: &PgPool, conversation_id: Uuid) -> sqlx::Result<String> {
    let rows = exchange(db, conversation_id).await?;
    Ok(rows.iter().map(|(role, body)| format!("{}: {body}", if role == "visitor" { "Visitor" } else { "Us" })).collect::<Vec<_>>().join("\n"))
}

pub fn chat_url(public_url: &str, project_id: Uuid, conversation_id: Uuid) -> String {
    format!("{public_url}/app/p/{project_id}/inbox?tab=chats&chat={conversation_id}")
}

/// Scores the lead and posts the hot-lead alerts in the background, so the visitor's thanks doesn't wait on them.
fn announce(state: &AppState, w: &Widget, conversation_id: Uuid, lead: Lead) {
    let (state, project_id, project) = (state.clone(), w.project_id, w.project_name.clone());
    tokio::spawn(async move {
        if let Err(e) = crate::score::refresh_lead(&state.db, lead.id).await {
            tracing::warn!(error = ?e, lead = %lead.id, "couldn't score a chat lead");
        }
        let messages = exchange(&state.db, conversation_id).await.unwrap_or_default();
        let who = match (lead.full_name.as_str(), lead.company_name.as_str()) {
            ("", "") => lead.email.clone().unwrap_or_default(),
            ("", company) => format!("{} ({company})", lead.email.clone().unwrap_or_default()),
            (name, "") => name.to_string(),
            (name, company) => format!("{name} ({company})"),
        };
        let summary = format!("{who} asked to hear from {project} in your website chat");
        let quote = messages.iter().rev().find(|(role, _)| role == "visitor").map(|(_, body)| cut(body, 300));
        let link = chat_url(&state.cfg.public_url, project_id, conversation_id);
        let alert = ChatAlert { label: "Hot lead", summary: &summary, quote: quote.as_deref(), link: &link, link_text: "Open the chat" };
        hooks::post_chat(&state.db, project_id, &alert).await;
        let chat = json!({"id": conversation_id, "messages": messages.iter().map(|(role, body)| json!({"role": role, "body": body})).collect::<Vec<_>>()});
        hooks::send_lead_event(&state, project_id, "hot_lead", &lead, Some(("chat", chat))).await;
    });
}

#[derive(Serialize)]
struct SettingsPage {
    title: String,
    settings: ChatSettings,
    usage: Usage,
    site: VisitorSite,
}

async fn load_settings(db: &PgPool, project_id: Uuid) -> sqlx::Result<ChatSettings> {
    sqlx::query("insert into chat_settings (project_id) values ($1) on conflict (project_id) do nothing").bind(project_id).execute(db).await?;
    sqlx::query_as(SETTINGS).bind(project_id).fetch_one(db).await
}

async fn settings(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<SettingsPage>> {
    let project = auth.project(&state.db, id).await?;
    let site = load_site(&state.db, id, project.domain.as_deref()).await?;
    let title = sqlx::query_scalar(&format!("select {TITLE} from projects where id = $1")).bind(id).fetch_one(&state.db).await?;
    Ok(Json(SettingsPage { title, settings: load_settings(&state.db, id).await?, usage: usage(&state.db, id).await?, site }))
}

#[derive(Deserialize)]
struct SettingsBody {
    enabled: Option<bool>,
    accent: Option<String>,
    greeting: Option<String>,
    position: Option<String>,
    paths: Option<Vec<String>>,
    faq: Option<String>,
    daily_messages: Option<i32>,
    monthly_budget_micros: Option<i64>,
}

/// The domains are the visitors feature's and saved through its route.
async fn update_settings(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<SettingsBody>) -> AppResult<Json<SettingsPage>> {
    auth.project(&state.db, id).await?;
    load_settings(&state.db, id).await?;
    let accent = body.accent.map(|a| a.trim().to_lowercase());
    if accent.as_deref().is_some_and(|a| a.len() != 7 || !a.starts_with('#') || !a[1..].chars().all(|c| c.is_ascii_hexdigit())) {
        return Err(AppError::BadRequest("Pick the colour as a hex code, like #2563eb".into()));
    }
    let greeting = body.greeting.map(|g| clean_text(&g));
    if greeting.as_deref().is_some_and(|g| g.is_empty() || g.chars().count() > MAX_GREETING) {
        return Err(AppError::BadRequest(format!("Write a greeting of up to {MAX_GREETING} characters")));
    }
    if let Some(position) = &body.position {
        util_input::one_of("position", position, &["right", "left"])?;
    }
    let mut paths: Option<Vec<String>> = None;
    if let Some(raw) = body.paths {
        let mut list: Vec<String> = vec![];
        for p in raw.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
            let path = intent_path(p).ok_or_else(|| AppError::BadRequest(format!("{p} isn't a page on your site, like /pricing")))?;
            if !list.contains(&path) {
                list.push(path);
            }
        }
        if list.len() > MAX_PATHS {
            return Err(AppError::BadRequest(format!("List at most {MAX_PATHS} pages")));
        }
        paths = Some(list);
    }
    let faq = body.faq.map(|f| f.trim().to_string());
    if faq.as_deref().is_some_and(|f| f.chars().count() > chat::FAQ_CHARS) {
        return Err(AppError::BadRequest(format!("Keep the FAQ under {} characters", chat::FAQ_CHARS)));
    }
    if body.daily_messages.is_some_and(|n| !(1..=5000).contains(&n)) {
        return Err(AppError::BadRequest("Allow between 1 and 5,000 answers a day".into()));
    }
    if body.monthly_budget_micros.is_some_and(|n| !(0..=1_000_000_000).contains(&n)) {
        return Err(AppError::BadRequest("Set a monthly limit between $0 and $1,000".into()));
    }
    sqlx::query(
        "update chat_settings set enabled = coalesce($2, enabled), accent = coalesce($3, accent), greeting = coalesce($4, greeting),
           position = coalesce($5, position), paths = coalesce($6, paths), faq = coalesce($7, faq),
           daily_messages = coalesce($8, daily_messages), monthly_budget_micros = coalesce($9, monthly_budget_micros)
         where project_id = $1",
    )
    .bind(id)
    .bind(body.enabled)
    .bind(accent)
    .bind(greeting)
    .bind(body.position)
    .bind(paths)
    .bind(faq)
    .bind(body.daily_messages)
    .bind(body.monthly_budget_micros)
    .execute(&state.db)
    .await?;
    settings(State(state), auth, Path(id)).await
}

/// A conversation in the Chats tab. `company` is what the visitor typed, else the visiting company's name. `live` means a
/// message in the last 10 minutes.
#[derive(Serialize, FromRow, Debug)]
pub struct ChatRow {
    id: Uuid,
    project_id: Uuid,
    email: Option<String>,
    name: String,
    company: String,
    domain: Option<String>,
    lead_id: Option<Uuid>,
    page: String,
    taken_over: bool,
    live: bool,
    messages: i64,
    last_message: String,
    last_role: String,
    last_message_at: Ts,
    created_at: Ts,
}

const ROWS: &str = "select c.id, c.project_id, c.email::text, c.name, coalesce(nullif(c.company, ''), nullif(co.name, ''), d.name, '') as company,
       c.domain::text, c.lead_id, c.page, c.taken_over_at is not null as taken_over, c.last_message_at > now() - interval '10 minutes' as live,
       (select count(*) from chat_messages m where m.conversation_id = c.id) as messages,
       coalesce(last.body, '') as last_message, coalesce(last.role, '') as last_role, c.last_message_at, c.created_at
     from chat_conversations c
     join projects p on p.id = c.project_id
     left join companies co on co.project_id = c.project_id and co.domain = c.domain
     left join directory_companies d on d.domain = lower(c.domain::text)
     left join lateral (select m.body, m.role from chat_messages m where m.conversation_id = c.id order by m.id desc limit 1) last on true";

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(page): Query<PageQuery>) -> AppResult<Json<Paginated<ChatRow>>> {
    auth.project(&state.db, id).await?;
    let total: i64 = sqlx::query_scalar("select count(*) from chat_conversations where project_id = $1").bind(id).fetch_one(&state.db).await?;
    let items = sqlx::query_as(&format!("{ROWS} where c.project_id = $1 order by c.last_message_at desc, c.id limit $2 offset $3"))
        .bind(id)
        .bind(page.limit())
        .bind(page.offset())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(Paginated { items, total }))
}

#[derive(Serialize, FromRow, Debug)]
struct ThreadMessage {
    id: i64,
    role: String,
    body: String,
    who: String,
    created_at: Ts,
}

async fn load_row(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<ChatRow> {
    sqlx::query_as(&format!("{ROWS} where c.id = $1 and p.org_id = $2")).bind(id).bind(org_id).fetch_optional(db).await?.ok_or(AppError::NotFound)
}

async fn thread(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let chat = load_row(&state.db, auth.org_id, id).await?;
    let messages: Vec<ThreadMessage> = sqlx::query_as(
        "select m.id, m.role, m.body, coalesce(nullif(u.name, ''), u.email::text, '') as who, m.created_at
         from chat_messages m left join users u on u.id = m.user_id where m.conversation_id = $1 order by m.id",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({"chat": chat, "messages": messages})))
}

#[derive(Deserialize)]
struct ReplyBody {
    body: String,
}

/// Answers as a teammate, which takes the conversation over so the model stops answering. The widget picks it up on its
/// next poll.
async fn reply(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<ReplyBody>) -> AppResult<Json<Value>> {
    let chat = load_row(&state.db, auth.org_id, id).await?;
    let text = clean_text(&body.body);
    if text.is_empty() {
        return Err(AppError::BadRequest("Type a reply first".into()));
    }
    if text.chars().count() > MAX_TEAM_REPLY {
        return Err(AppError::BadRequest(format!("Keep the reply under {MAX_TEAM_REPLY} characters")));
    }
    sqlx::query("update chat_conversations set taken_over_at = coalesce(taken_over_at, now()), taken_over_by = coalesce(taken_over_by, $2) where id = $1")
        .bind(id)
        .bind(auth.user_id)
        .execute(&state.db)
        .await?;
    add_message(&state.db, chat.project_id, id, "team", &text, 0, Some(auth.user_id)).await?;
    chat::changed(&state, chat.project_id, id);
    thread(State(state), auth, Path(id)).await
}

#[derive(Deserialize)]
struct TakeoverBody {
    on: bool,
}

/// Off hands the conversation back to the model.
async fn takeover(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<TakeoverBody>) -> AppResult<Json<Value>> {
    let chat = load_row(&state.db, auth.org_id, id).await?;
    sqlx::query(
        "update chat_conversations set taken_over_at = case when $2 then coalesce(taken_over_at, now()) end,
           taken_over_by = case when $2 then coalesce(taken_over_by, $3) end where id = $1",
    )
    .bind(id)
    .bind(body.on)
    .bind(auth.user_id)
    .execute(&state.db)
    .await?;
    chat::changed(&state, chat.project_id, id);
    thread(State(state), auth, Path(id)).await
}

/// SSE for the Chats tab: the id of each conversation that changed in this project, as it happens.
async fn events(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Response> {
    auth.project(&state.db, id).await?;
    let mut rx = state.events.subscribe();
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(e) if e.kind == chat::CHAT_EVENT && e.data["project_id"] == json!(id) => {
                    yield Ok::<_, Infallible>(Event::default().data(json!({"conversation": e.run_id}).to_string()));
                }
                Ok(_) => continue,
                // Missed some: the tab refetches everything on an event without a conversation.
                Err(RecvError::Lagged(_)) => yield Ok(Event::default().data("{}")),
                Err(RecvError::Closed) => return,
            }
        }
    };
    // no-transform stops gzip middleware (Next's dev proxy, nginx) from buffering the stream until it ends.
    let headers = [(CACHE_CONTROL, "no-cache, no-transform"), (HeaderName::from_static("x-accel-buffering"), "no")];
    Ok((headers, Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))).into_response())
}

#[cfg(test)]
mod tests;
