//! Add to Slack on the Notifications page, the channel each project posts to, and what the buttons on Splee's Slack messages
//! do. Slack sends every button press and modal to /api/slack/interactions, signed with the app's signing secret. It wants an
//! answer within 3 seconds, so the request is answered first and the work happens after, with the outcome shown by updating
//! the message and, for anything that went wrong, a note only the person who pressed sees.
//! The messages themselves are built in crate::slack.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Redirect;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

use super::{emails, inbox, util_scope};
use crate::auth::{Auth, MaybeAuth};
use crate::crypto::{self, random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::mail::oauth::pkce_challenge;
use crate::models::{EmailMessage, Lead, Segment, Ts};
use crate::slack::{self, Client};
use crate::state::AppState;

const STATE_MINUTES: i64 = 10;
const STATE_PURPOSE: &str = "slack-oauth";
const PAGE: &str = "/app/notifications";
const START_AGAIN: &str = "This Slack sign-in expired or was already used. Add Splee to Slack again.";
const NOT_YOURS: &str = "This isn't in your Splee organization, so nothing was done.";
const SIGNED_OUT: &str = "Splee isn't connected to this Slack workspace anymore. An admin can add it again on the Notifications page.";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/organization/slack", get(status).delete(disconnect))
        .route("/organization/slack/install", post(install))
        .route("/organization/slack/channels", get(channels))
        .route("/projects/{id}/slack-channel", put(pick_channel))
        .route("/slack/oauth/callback", get(callback))
        .route("/slack/interactions", post(interactions))
}

#[derive(Serialize, FromRow)]
struct Workspace {
    team_id: String,
    team_name: String,
    installed_by: Option<String>,
    created_at: Ts,
}

#[derive(Serialize, FromRow)]
struct ProjectChannel {
    project_id: Uuid,
    project_name: String,
    channel_id: Option<String>,
    channel_name: Option<String>,
    error: Option<String>,
    failed_at: Option<Ts>,
}

#[derive(Serialize)]
struct Status {
    /// The server has a Slack app. Without one the page shows nothing about Slack.
    available: bool,
    workspace: Option<Workspace>,
    projects: Vec<ProjectChannel>,
}

async fn status(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Status>> {
    if state.cfg.slack.is_none() {
        return Ok(Json(Status { available: false, workspace: None, projects: vec![] }));
    }
    let workspace = sqlx::query_as::<_, Workspace>(
        "select i.team_id, i.team_name, u.name as installed_by, i.created_at from slack_installs i left join users u on u.id = i.installed_by
         where i.org_id = $1",
    )
    .bind(auth.org_id)
    .fetch_optional(&state.db)
    .await?;
    let projects = sqlx::query_as::<_, ProjectChannel>(
        "select p.id as project_id, p.name as project_name, c.channel_id, c.channel_name, c.error, c.failed_at
         from projects p left join slack_channels c on c.project_id = p.id where p.org_id = $1 order by p.created_at",
    )
    .bind(auth.org_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Status { available: true, workspace, projects }))
}

fn app(state: &AppState) -> AppResult<&crate::config::SlackApp> {
    state.cfg.slack.as_ref().ok_or_else(|| AppError::BadRequest("Slack isn't set up on this Splee".into()))
}

async fn install(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    let app = app(&state)?;
    let (nonce, verifier) = (random_token(), random_token());
    sqlx::query("delete from slack_oauth_states where expires_at < now()").execute(&state.db).await?;
    sqlx::query("insert into slack_oauth_states (nonce_hash, user_id, org_id, code_verifier_enc, expires_at) values ($1, $2, $3, $4, $5)")
        .bind(sha256_hex(&nonce))
        .bind(auth.user_id)
        .bind(auth.org_id)
        .bind(crypto::encrypt(&state.cfg.app_secret, &verifier))
        .bind(Utc::now() + Duration::minutes(STATE_MINUTES))
        .execute(&state.db)
        .await?;
    let mut url = Url::parse(slack::AUTHORIZE_URL).expect("a fixed URL");
    url.query_pairs_mut()
        .append_pair("client_id", &app.client_id)
        .append_pair("scope", slack::SCOPES)
        .append_pair("redirect_uri", &slack::redirect_uri(&state.cfg.public_url))
        .append_pair("state", &crypto::sign_state(&state.cfg.app_secret, STATE_PURPOSE, &nonce, auth.user_id, auth.org_id))
        .append_pair("code_challenge", &pkce_challenge(&verifier))
        .append_pair("code_challenge_method", "S256");
    Ok(Json(json!({ "url": String::from(url) })))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Always redirects to the Notifications page, with `slack=connected` or `slack_error`.
async fn callback(State(state): State<AppState>, MaybeAuth(auth): MaybeAuth, Query(q): Query<CallbackQuery>) -> Redirect {
    let params = match finish(&state, auth, q).await {
        Ok(()) => vec![("slack", "connected".to_string())],
        Err(error) => vec![("slack_error", error)],
    };
    let url = Url::parse_with_params(&format!("http://splee{PAGE}"), &params).expect("a fixed URL");
    Redirect::to(&format!("{PAGE}?{}", url.query().unwrap_or_default()))
}

#[derive(FromRow)]
struct Pending {
    user_id: Uuid,
    org_id: Uuid,
    code_verifier_enc: String,
    expires_at: Ts,
}

async fn finish(state: &AppState, auth: Option<Auth>, q: CallbackQuery) -> Result<(), String> {
    let internal = |e: &dyn std::fmt::Display| {
        tracing::error!(error = %e, "adding Splee to Slack failed");
        "Something went wrong connecting Slack. Try again.".to_string()
    };
    let app = state.cfg.slack.as_ref().ok_or("Slack isn't set up on this Splee.")?;
    let signed = q.state.as_deref().unwrap_or_default();
    let nonce = crypto::state_nonce(signed).ok_or(START_AGAIN)?;
    // Deleted before anything else is checked, so a state works once whatever happens.
    let pending = sqlx::query_as::<_, Pending>("delete from slack_oauth_states where nonce_hash = $1 returning user_id, org_id, code_verifier_enc, expires_at")
        .bind(sha256_hex(nonce))
        .fetch_optional(&state.db)
        .await
        .map_err(|e| internal(&e))?
        .ok_or(START_AGAIN)?;
    let secret = &state.cfg.app_secret;
    if !crypto::verify_state(secret, STATE_PURPOSE, signed, pending.user_id, pending.org_id) || pending.expires_at <= Utc::now() {
        return Err(START_AGAIN.into());
    }
    if let Some(error) = q.error {
        return Err(match error.as_str() {
            "access_denied" => "You didn't allow Splee into Slack, so nothing was connected.".into(),
            _ => format!("Slack couldn't connect: {error}"),
        });
    }
    let auth = auth.ok_or("Sign in to Splee, then add it to Slack again.")?;
    if (auth.user_id, auth.org_id) != (pending.user_id, pending.org_id) {
        return Err("You switched organizations while connecting Slack. Add it again from this one.".into());
    }
    auth.require_admin().map_err(|e| e.public_message())?;
    let code = q.code.filter(|c| !c.is_empty()).ok_or(START_AGAIN)?;
    let verifier = crypto::decrypt(secret, &pending.code_verifier_enc).map_err(|e| internal(&e))?;
    let access = slack::oauth_access(app, &code, &slack::redirect_uri(&state.cfg.public_url), &verifier).await.map_err(|e| e.to_string())?;
    let (Some(token), Some(team_id)) = (access["access_token"].as_str(), access["team"]["id"].as_str()) else {
        return Err("Slack didn't give Splee a bot token. Add it to Slack again.".into());
    };
    let team_name = access["team"]["name"].as_str().unwrap_or_default();

    let mut tx = state.db.begin().await.map_err(|e| internal(&e))?;
    // One organization per workspace, so a button pressed there names a single organization.
    let elsewhere: bool = sqlx::query_scalar("select exists (select 1 from slack_installs where team_id = $1 and org_id <> $2)")
        .bind(team_id)
        .bind(auth.org_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| internal(&e))?;
    if elsewhere {
        return Err(format!("{team_name} is already connected to another Splee organization. Disconnect it there first."));
    }
    // Channels belong to a workspace, so moving to another one clears the projects' picks.
    sqlx::query(
        "delete from slack_channels c using projects p where p.id = c.project_id and p.org_id = $1
           and exists (select 1 from slack_installs i where i.org_id = $1 and i.team_id <> $2)",
    )
    .bind(auth.org_id)
    .bind(team_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| internal(&e))?;
    sqlx::query(
        "insert into slack_installs (org_id, team_id, team_name, bot_user_id, bot_token_enc, installed_by) values ($1, $2, $3, $4, $5, $6)
         on conflict (org_id) do update set team_id = excluded.team_id, team_name = excluded.team_name, bot_user_id = excluded.bot_user_id,
           bot_token_enc = excluded.bot_token_enc, installed_by = excluded.installed_by, created_at = now()",
    )
    .bind(auth.org_id)
    .bind(team_id)
    .bind(team_name)
    .bind(access["bot_user_id"].as_str().unwrap_or_default())
    .bind(crypto::encrypt(secret, token))
    .bind(auth.user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| internal(&e))?;
    tx.commit().await.map_err(|e| internal(&e))
}

/// The org's bot, or a 400 saying Slack isn't connected.
async fn client(state: &AppState, org_id: Uuid) -> AppResult<Client> {
    app(state)?;
    let token: Option<String> = sqlx::query_scalar("select bot_token_enc from slack_installs where org_id = $1").bind(org_id).fetch_optional(&state.db).await?;
    let token = token.ok_or_else(|| AppError::BadRequest("Add Splee to Slack first".into()))?;
    Ok(Client::for_install(state, &token)?)
}

fn slack_error(e: anyhow::Error) -> AppError {
    match e.downcast::<slack::ApiError>() {
        Ok(e) => AppError::BadRequest(e.to_string()),
        Err(e) => AppError::BadRequest(format!("Couldn't reach Slack: {e:#}")),
    }
}

async fn channels(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<slack::Channel>>> {
    Ok(Json(client(&state, auth.org_id).await?.channels().await.map_err(slack_error)?))
}

#[derive(Deserialize)]
struct PickBody {
    /// None stops posting the project to Slack.
    channel_id: Option<String>,
}

async fn pick_channel(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PickBody>) -> AppResult<Json<Value>> {
    let project = auth.project(&state.db, id).await?;
    let Some(channel_id) = body.channel_id.filter(|c| !c.trim().is_empty()) else {
        sqlx::query("delete from slack_channels where project_id = $1").bind(project.id).execute(&state.db).await?;
        return Ok(Json(json!({ "ok": true })));
    };
    // Only a channel the bot can see now, which is also how its name is known.
    let channels = client(&state, auth.org_id).await?.channels().await.map_err(slack_error)?;
    let channel = channels
        .into_iter()
        .find(|c| c.id == channel_id)
        .ok_or_else(|| AppError::BadRequest("Slack doesn't show Splee that channel. Invite Splee to it first.".into()))?;
    sqlx::query(
        "insert into slack_channels (project_id, channel_id, channel_name) values ($1, $2, $3)
         on conflict (project_id) do update set channel_id = excluded.channel_id, channel_name = excluded.channel_name, error = null, failed_at = null",
    )
    .bind(project.id)
    .bind(&channel.id)
    .bind(&channel.name)
    .execute(&state.db)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn disconnect(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    // Revoke before deleting, so a failed revoke leaves the token here to try again. A token Slack already dropped counts as revoked.
    if state.cfg.slack.is_some() {
        let token: Option<String> =
            sqlx::query_scalar("select bot_token_enc from slack_installs where org_id = $1").bind(auth.org_id).fetch_optional(&state.db).await?;
        if let Some(token) = token
            && let Err(e) = Client::for_install(&state, &token)?.revoke().await
            && !slack::signed_out(&e)
        {
            return Err(slack_error(e));
        }
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("delete from slack_installs where org_id = $1").bind(auth.org_id).execute(&mut *tx).await?;
    sqlx::query("delete from slack_channels c using projects p where p.id = c.project_id and p.org_id = $1").bind(auth.org_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

/// Checks the signature, answers at once and acts after. A 401 for anything Slack didn't sign.
async fn interactions(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let Some(app) = state.cfg.slack.as_ref() else { return StatusCode::NOT_FOUND };
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let (timestamp, signature) = (header("x-slack-request-timestamp"), header("x-slack-signature"));
    if !slack::verify_signature(&app.signing_secret, timestamp, &body, signature, Utc::now().timestamp()) {
        return StatusCode::UNAUTHORIZED;
    }
    let Some(payload) = form_payload(&body) else { return StatusCode::BAD_REQUEST };
    tokio::spawn(async move { handle(&state, payload).await });
    // An empty 200 is also what closes a modal after its submission.
    StatusCode::OK
}

/// Slack posts interactions as a form with the JSON in `payload`.
fn form_payload(body: &[u8]) -> Option<Value> {
    let mut url = Url::parse("http://slack").expect("a fixed URL");
    url.set_query(Some(std::str::from_utf8(body).ok()?));
    let payload = url.query_pairs().find(|(k, _)| k == "payload")?.1;
    serde_json::from_str(&payload).ok()
}

/// Where the message that was acted on lives, to update it.
struct Message {
    channel: String,
    ts: String,
}

/// The Splee member who pressed a button, found by the email on their Slack profile, and the org's bot.
struct Actor {
    org_id: Uuid,
    user_id: Uuid,
    name: String,
    slack_user: String,
    client: Client,
}

/// What an action has to say to the person who pressed, if anything. The updated message says the rest.
type Said = Option<String>;

pub(crate) async fn handle(state: &AppState, payload: Value) {
    let metadata: Value = payload["view"]["private_metadata"].as_str().and_then(|m| serde_json::from_str(m).ok()).unwrap_or_default();
    let response_url = payload["response_url"].as_str().or(metadata["response_url"].as_str()).unwrap_or_default().to_string();
    let said = match payload["type"].as_str() {
        Some("block_actions") => block_action(state, &payload).await,
        Some("view_submission") if payload["view"]["callback_id"] == "edit_reply" => submission(state, &payload, &metadata).await,
        _ => Ok(None),
    };
    let text = match said {
        Ok(Some(text)) => text,
        Ok(None) => return,
        Err(e) => {
            tracing::error!(error = ?e, "a Slack action failed");
            "Something went wrong in Splee, so nothing was done. Try again, or open it in Splee.".into()
        }
    };
    slack::tell(&response_url, &text).await;
}

async fn actor(state: &AppState, payload: &Value) -> anyhow::Result<Result<Actor, String>> {
    let team = payload["team"]["id"].as_str().or(payload["user"]["team_id"].as_str()).unwrap_or_default();
    let slack_user = payload["user"]["id"].as_str().unwrap_or_default().to_string();
    let install: Option<(Uuid, String, String)> =
        sqlx::query_as("select i.org_id, i.bot_token_enc, o.name from slack_installs i join organizations o on o.id = i.org_id where i.team_id = $1")
            .bind(team)
            .fetch_optional(&state.db)
            .await?;
    let Some((org_id, token, org_name)) = install else { return Ok(Err(SIGNED_OUT.into())) };
    let client = Client::for_install(state, &token)?;
    let email = match client.user_email(&slack_user).await {
        Ok(email) => email,
        Err(e) if slack::signed_out(&e) => return Ok(Err(SIGNED_OUT.into())),
        Err(e) => return Err(e),
    };
    let Some(email) = email else {
        return Ok(Err("Your Slack profile doesn't show an email address, so Splee can't tell who you are. Nothing was done.".into()));
    };
    let member: Option<(Uuid, String)> = sqlx::query_as(
        "select u.id, u.name from users u join memberships m on m.user_id = u.id where m.org_id = $1 and lower(u.email::text) = $2",
    )
    .bind(org_id)
    .bind(&email)
    .fetch_optional(&state.db)
    .await?;
    let Some((user_id, name)) = member else {
        return Ok(Err(format!("Only members of {org_name} in Splee can do this, and {email} isn't one. Ask an admin to invite you.")));
    };
    let name = if name.trim().is_empty() { email } else { name };
    Ok(Ok(Actor { org_id, user_id, name, slack_user, client }))
}

fn uuid(value: &Value) -> Option<Uuid> {
    value.as_str()?.parse().ok()
}

async fn block_action(state: &AppState, payload: &Value) -> anyhow::Result<Said> {
    let action = &payload["actions"][0];
    let action_id = action["action_id"].as_str().unwrap_or_default();
    // Open in Splee and Review in Splee are links. Slack tells us about the press anyway.
    if matches!(action_id, "open" | "review") {
        return Ok(None);
    }
    let actor = match actor(state, payload).await? {
        Ok(actor) => actor,
        Err(said) => return Ok(Some(said)),
    };
    let message = Message {
        channel: payload["container"]["channel_id"].as_str().or(payload["channel"]["id"].as_str()).unwrap_or_default().into(),
        ts: payload["container"]["message_ts"].as_str().unwrap_or_default().into(),
    };
    let value = &action["value"];
    let blocks = &payload["message"]["blocks"];
    let text = payload["message"]["text"].as_str().unwrap_or_default();
    match (action_id, uuid(value)) {
        ("send_reply", Some(draft)) => send_draft(state, &actor, draft, None, &message).await,
        ("edit_reply", Some(lead)) => open_editor(state, &actor, lead, payload).await,
        ("snooze", Some(lead)) => snooze(state, &actor, lead, &message).await,
        ("approve_email", Some(email)) => decide_email(state, &actor, email, true, &message, blocks, text).await,
        ("skip_email", Some(email)) => decide_email(state, &actor, email, false, &message, blocks, text).await,
        ("approve_campaign", _) => approve_campaign(state, &actor, value, &message, blocks, text).await,
        _ => Ok(None),
    }
}

async fn submission(state: &AppState, payload: &Value, metadata: &Value) -> anyhow::Result<Said> {
    let actor = match actor(state, payload).await? {
        Ok(actor) => actor,
        Err(said) => return Ok(Some(said)),
    };
    let message = Message { channel: metadata["channel"].as_str().unwrap_or_default().into(), ts: metadata["ts"].as_str().unwrap_or_default().into() };
    let text = payload["view"]["state"]["values"]["reply"]["body"]["value"].as_str().unwrap_or_default().to_string();
    if text.trim().is_empty() {
        return Ok(Some("The reply was empty, so nothing was sent.".into()));
    }
    match (uuid(&metadata["draft"]), uuid(&metadata["lead"])) {
        (Some(draft), _) => send_draft(state, &actor, draft, Some(text), &message).await,
        (None, Some(lead)) => send_written(state, &actor, lead, &text, &message).await,
        _ => Ok(None),
    }
}

/// Records who acted on `target`. False when someone already did, which is what stops a second send.
async fn claim(db: &sqlx::PgPool, target: Uuid, actor: &Actor, action: &str) -> sqlx::Result<bool> {
    let claimed = sqlx::query(
        "insert into slack_actions (target_id, org_id, action, user_id, slack_user_id) values ($1, $2, $3, $4, $5) on conflict (target_id) do nothing",
    )
    .bind(target)
    .bind(actor.org_id)
    .bind(action)
    .bind(actor.user_id)
    .bind(&actor.slack_user)
    .execute(db)
    .await?;
    Ok(claimed.rows_affected() == 1)
}

/// For an action that failed after claiming, so it can be tried again.
async fn unclaim(db: &sqlx::PgPool, target: Uuid) -> sqlx::Result<()> {
    sqlx::query("delete from slack_actions where target_id = $1").bind(target).execute(db).await.map(drop)
}

/// Who claimed `target` from Slack, and for what.
async fn claimed_by(db: &sqlx::PgPool, target: Uuid) -> sqlx::Result<Option<(String, String)>> {
    let row: Option<(Option<String>, String)> =
        sqlx::query_as("select u.name, a.action from slack_actions a left join users u on u.id = a.user_id where a.target_id = $1")
            .bind(target)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(name, action)| (name.unwrap_or_else(|| "Someone".into()), action)))
}

async fn claimer(db: &sqlx::PgPool, target: Uuid) -> sqlx::Result<String> {
    Ok(claimed_by(db, target).await?.map(|(name, _)| name).unwrap_or_else(|| "Someone".into()))
}

/// Builds the hot reply message again from the database, so it shows what happened whoever did it and wherever.
async fn refresh_hot_reply(state: &AppState, actor: &Actor, lead_id: Uuid, message: &Message, note: Option<String>) -> anyhow::Result<()> {
    let Some((hot, _)) = slack::load_hot_reply(&state.db, &state.cfg.public_url, lead_id, note).await? else { return Ok(()) };
    actor.client.update_message(&message.channel, &message.ts, &hot.text(), &hot.blocks()).await
}

/// Send reply, or the edit modal's Send when there was a draft: approves the draft as the inbox does.
async fn send_draft(state: &AppState, actor: &Actor, draft_id: Uuid, body: Option<String>, message: &Message) -> anyhow::Result<Said> {
    let db = &state.db;
    let Ok(draft) = util_scope::load::<EmailMessage>(db, "email_messages", draft_id, actor.org_id).await else { return Ok(Some(NOT_YOURS.into())) };
    if draft.kind != "auto_reply" {
        return Ok(Some(NOT_YOURS.into()));
    }
    let already = if draft.status != "draft" {
        Some(match (draft.status.as_str(), claimed_by(db, draft.id).await?) {
            ("cancelled", _) => "This draft was dropped because someone answered the lead in Splee.".to_string(),
            (_, Some((name, _))) => format!("{name} already sent this reply."),
            _ => "This reply was already sent.".to_string(),
        })
    } else if !claim(db, draft.id, actor, "send_reply").await? {
        Some(format!("{} already sent this reply.", claimer(db, draft.id).await?))
    } else {
        match inbox::approve_reply(db, actor.org_id, draft.id, body).await {
            Ok(_) => None,
            Err(e) => {
                unclaim(db, draft.id).await?;
                Some(e.public_message())
            }
        }
    };
    refresh_hot_reply(state, actor, draft.lead_id, message, None).await?;
    Ok(already)
}

/// The edit modal's Send when nothing was drafted: a reply the person wrote, as from the inbox.
async fn send_written(state: &AppState, actor: &Actor, lead_id: Uuid, text: &str, message: &Message) -> anyhow::Result<Said> {
    let db = &state.db;
    if util_scope::load::<Lead>(db, "leads", lead_id, actor.org_id).await.is_err() {
        return Ok(Some(NOT_YOURS.into()));
    }
    let Some((hot, reply_id)) = slack::load_hot_reply(db, &state.cfg.public_url, lead_id, None).await? else { return Ok(Some(NOT_YOURS.into())) };
    let said = if hot.answered {
        Some("This lead was already answered, so your reply wasn't sent.".to_string())
    } else if !claim(db, reply_id, actor, "send_reply").await? {
        Some(format!("{} is already answering this lead, so your reply wasn't sent.", claimer(db, reply_id).await?))
    } else {
        match inbox::send_reply(db, actor.org_id, lead_id, text).await {
            Ok(_) => None,
            Err(e) => {
                unclaim(db, reply_id).await?;
                Some(e.public_message())
            }
        }
    };
    refresh_hot_reply(state, actor, lead_id, message, None).await?;
    Ok(said)
}

async fn open_editor(state: &AppState, actor: &Actor, lead_id: Uuid, payload: &Value) -> anyhow::Result<Said> {
    let db = &state.db;
    if util_scope::load::<Lead>(db, "leads", lead_id, actor.org_id).await.is_err() {
        return Ok(Some(NOT_YOURS.into()));
    }
    let Some((hot, _)) = slack::load_hot_reply(db, &state.cfg.public_url, lead_id, None).await? else { return Ok(Some(NOT_YOURS.into())) };
    if hot.answered {
        let message = Message {
            channel: payload["container"]["channel_id"].as_str().unwrap_or_default().into(),
            ts: payload["container"]["message_ts"].as_str().unwrap_or_default().into(),
        };
        refresh_hot_reply(state, actor, lead_id, &message, None).await?;
        return Ok(Some("This lead was already answered.".into()));
    }
    let metadata = json!({
        "lead": lead_id,
        "draft": hot.draft_id,
        "channel": payload["container"]["channel_id"],
        "ts": payload["container"]["message_ts"],
        "response_url": payload["response_url"],
    });
    let draft = hot.draft_id.and(hot.answer.as_deref());
    let view = slack::reply_modal(&hot.lead_name, &hot.reply, draft, &metadata);
    actor.client.open_view(payload["trigger_id"].as_str().unwrap_or_default(), &view).await?;
    Ok(None)
}

async fn snooze(state: &AppState, actor: &Actor, lead_id: Uuid, message: &Message) -> anyhow::Result<Said> {
    if util_scope::load::<Lead>(&state.db, "leads", lead_id, actor.org_id).await.is_err() {
        return Ok(Some(NOT_YOURS.into()));
    }
    inbox::set_reminder(&state.db, lead_id, Some(Utc::now() + Duration::days(1))).await?;
    let note = format!("{} snoozed this for a day. It comes back in the Splee inbox tomorrow.", actor.name);
    refresh_hot_reply(state, actor, lead_id, message, Some(note)).await?;
    Ok(None)
}

async fn decide_email(state: &AppState, actor: &Actor, id: Uuid, approve: bool, message: &Message, blocks: &Value, text: &str) -> anyhow::Result<Said> {
    let db = &state.db;
    let Ok(email) = util_scope::load::<EmailMessage>(db, "email_messages", id, actor.org_id).await else { return Ok(Some(NOT_YOURS.into())) };
    let block = format!("email:{id}");
    let (action, done) = if approve { ("approve", "approved") } else { ("skip", "skipped") };
    let already = |status: &str, by: Option<String>| {
        let what = match status {
            "approved" | "scheduled" | "sending" => "approved",
            "cancelled" => "skipped",
            "sent" => return "This email already went out.".to_string(),
            _ => return "This email can't be changed from Slack anymore. Open it in Splee.".to_string(),
        };
        match by {
            Some(name) => format!("{name} already {what} this email."),
            None => format!("This email was already {what}."),
        }
    };
    let said = if email.status != "draft" {
        Some(already(&email.status, claimed_by(db, id).await?.map(|(name, _)| name)))
    } else if !claim(db, id, actor, action).await? {
        // Someone pressed a moment ago and their change may not be saved yet, so their action says what it was.
        let (name, theirs) = claimed_by(db, id).await?.unwrap_or_else(|| ("Someone".into(), action.into()));
        Some(already(if theirs == "skip" { "cancelled" } else { "approved" }, Some(name)))
    } else {
        let body = emails::UpdateBody { status: Some(if approve { "approved" } else { "cancelled" }.into()), ..Default::default() };
        match emails::update_one(db, actor.org_id, id, body).await {
            Ok(_) => None,
            Err(e) => {
                unclaim(db, id).await?;
                return Ok(Some(e.public_message()));
            }
        }
    };
    let note = match &said {
        None => format!("{} {done} this email.", actor.name),
        Some(already) => already.clone(),
    };
    actor.client.update_message(&message.channel, &message.ts, text, &slack::mark_done(blocks, &block, &note)).await?;
    Ok(said)
}

#[derive(Deserialize)]
struct CampaignValue {
    segment: Uuid,
    /// When the digest counted, so emails written since aren't approved unseen.
    before: Ts,
}

async fn approve_campaign(state: &AppState, actor: &Actor, value: &Value, message: &Message, blocks: &Value, text: &str) -> anyhow::Result<Said> {
    let db = &state.db;
    let Some(value) = value.as_str().and_then(|v| serde_json::from_str::<CampaignValue>(v).ok()) else { return Ok(None) };
    let Ok(segment) = util_scope::load::<Segment>(db, "segments", value.segment, actor.org_id).await else { return Ok(Some(NOT_YOURS.into())) };
    let ids: Vec<Uuid> = sqlx::query_scalar(&format!("select e.id from email_messages e where {} and e.segment_id = $2 and e.created_at <= $3", slack::WAITING))
        .bind(segment.project_id)
        .bind(segment.id)
        .bind(value.before)
        .fetch_all(db)
        .await?;
    let (mut approved, mut failed) = (0, None);
    for id in ids {
        if !claim(db, id, actor, "approve").await? {
            continue;
        }
        let body = emails::UpdateBody { status: Some("approved".into()), ..Default::default() };
        match emails::update_one(db, actor.org_id, id, body).await {
            Ok(_) => approved += 1,
            Err(e) => {
                unclaim(db, id).await?;
                let (n, _) = failed.get_or_insert((0, e.public_message()));
                *n += 1;
            }
        }
    }
    // Keep the button while any are left, so pressing it again retries them.
    if let Some((n, why)) = failed {
        let s = if n == 1 { "" } else { "s" };
        return Ok(Some(format!("Approved {approved}, but {n} email{s} couldn't be approved ({why}). Press Approve all again or open them in Splee.")));
    }
    let (note, said) = match approved {
        0 => ("Nothing was left to approve here.".to_string(), Some("These emails were already approved or skipped.".to_string())),
        1 => (format!("{} approved 1 email.", actor.name), None),
        n => (format!("{} approved {n} emails.", actor.name), None),
    };
    actor.client.update_message(&message.channel, &message.ts, text, &slack::mark_done(blocks, &format!("campaign:{}", segment.id), &note)).await?;
    Ok(said)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    use sqlx::PgPool;

    use super::*;
    use crate::config::{Config, SlackApp};

    /// Slack's Web API and response URLs. It knows two people by Slack id and keeps every call.
    #[derive(Clone, Default)]
    struct FakeSlack {
        calls: Arc<Mutex<Vec<(String, Value)>>>,
        /// An error auth.revoke answers with, when set.
        revoke_error: Arc<Mutex<Option<&'static str>>>,
    }

    impl FakeSlack {
        async fn start() -> (Self, String) {
            let fake = Self::default();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let app = Router::new().route("/{method}", post(fake_method)).with_state(fake.clone());
            tokio::spawn(async move { axum::serve(listener, app).await });
            (fake, url)
        }

        fn calls(&self, method: &str) -> Vec<Value> {
            self.calls.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, body)| body.clone()).collect()
        }

        /// What the person who pressed was told, through the response URL.
        fn told(&self) -> Vec<String> {
            self.calls("respond").iter().map(|b| b["text"].as_str().unwrap().to_string()).collect()
        }

        fn last_update(&self) -> Value {
            self.calls("chat.update").pop().expect("the message was updated")
        }
    }

    async fn fake_method(State(fake): State<FakeSlack>, Path(method): Path<String>, headers: HeaderMap, body: Bytes) -> Json<Value> {
        let json = headers.get("content-type").is_some_and(|c| c.to_str().unwrap().starts_with("application/json"));
        let body: Value = if json {
            serde_json::from_slice(&body).unwrap()
        } else {
            let mut url = Url::parse("http://x").unwrap();
            url.set_query(Some(std::str::from_utf8(&body).unwrap()));
            Value::Object(url.query_pairs().map(|(k, v)| (k.into_owned(), json!(v))).collect())
        };
        fake.calls.lock().unwrap().push((method.clone(), body.clone()));
        Json(match method.as_str() {
            "users.lookupByEmail" => match body["email"].as_str() {
                Some("ana@acme.test") => json!({ "ok": true, "user": { "id": "UANA" } }),
                _ => json!({ "ok": false, "error": "users_not_found" }),
            },
            "users.info" => match body["user"].as_str() {
                Some("UANA") => json!({ "ok": true, "user": { "profile": { "email": "Ana@Acme.test" } } }),
                Some("UEVE") => json!({ "ok": true, "user": { "profile": { "email": "eve@beta.test" } } }),
                _ => json!({ "ok": false, "error": "user_not_found" }),
            },
            "conversations.list" => json!({
                "ok": true,
                "channels": [{ "id": "C1", "name": "sales", "is_private": false }, { "id": "G2", "name": "deals", "is_private": true }],
                "response_metadata": { "next_cursor": "" },
            }),
            "auth.revoke" => match *fake.revoke_error.lock().unwrap() {
                Some(error) => json!({ "ok": false, "error": error }),
                None => json!({ "ok": true, "revoked": true }),
            },
            "oauth.v2.access" => json!({ "ok": true, "access_token": "xoxb-new", "bot_user_id": "UBOT", "team": { "id": "TNEW", "name": "Fieldnote" } }),
            _ => json!({ "ok": true, "ts": "1700000000.000200" }),
        })
    }

    struct Seeded {
        state: AppState,
        base: String,
        client: reqwest::Client,
        fake: FakeSlack,
        fake_url: String,
        acme: Uuid,
        project: Uuid,
        segment: Uuid,
        lead: Uuid,
        draft: Uuid,
        /// A lead who replied with nothing drafted.
        quiet_lead: Uuid,
        beta_draft: Uuid,
    }

    fn config(fake_url: &str) -> Config {
        Config {
            database_url: String::new(),
            app_secret: "test-secret".into(),
            public_url: "http://localhost:3405".into(),
            unsubscribe_url: String::new(),
            bind_addr: String::new(),
            agent_model: String::new(),
            agent_concurrency: 0,
            sending_enabled: false,
            bird_bin: None,
            open_signup: true,
            public_agent: false,
            porkbun_api_url: String::new(),
            twilio_api_url: String::new(),
            bots_api_url: String::new(),
            outreach_api_url: String::new(),
            postmaster_api_url: String::new(),
            operator_emails: Vec::new(),
            google: None,
            microsoft: None,
            tumblr: None,
            salesforce: None,
            zoho: None,
            meta: None,
            graph_api_url: String::new(),
            crm_api_url: String::new(),
            payments_api_url: String::new(),
            webflow_api_url: String::new(),
            linkedin_ads: None,
            google_ads_developer_token: None,
            ads_api_url: String::new(),
            ipinfo_token: None,
            slack: Some(SlackApp { client_id: "123.456".into(), client_secret: "shh".into(), signing_secret: "signing".into(), api_url: fake_url.into() }),
        }
    }

    async fn scalar<T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin>(db: &PgPool, sql: &str) -> T {
        sqlx::query_scalar(sql).fetch_one(db).await.unwrap()
    }

    /// A hot lead who replied, with its question, and a lead they answered into a campaign.
    async fn lead_with_reply(db: &PgPool, project: Uuid, name: &str) -> Uuid {
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, status, full_name, email, company_name) values ($1, 'hot', $2, $3, 'Acme') returning id",
        )
        .bind(project)
        .bind(name)
        .bind(format!("{}@lead.test", name.to_lowercase().replace(' ', ".")))
        .fetch_one(db)
        .await
        .unwrap();
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, status, subject, body, classification, message_id, created_at)
             values ($1, $2, 'inbound', 'inbound', 'received', 'Quick idea', 'Sounds good. Tuesday?', 'hot', $3, now() - interval '1 hour')",
        )
        .bind(project)
        .bind(lead)
        .bind(format!("<{lead}@lead.test>"))
        .execute(db)
        .await
        .unwrap();
        lead
    }

    async fn draft_for(db: &PgPool, project: Uuid, lead: Uuid) -> Uuid {
        sqlx::query_scalar(
            "insert into email_messages (project_id, lead_id, direction, kind, status, subject, body)
             values ($1, $2, 'outbound', 'auto_reply', 'draft', 'Re: Quick idea', 'Hi, Tuesday works.') returning id",
        )
        .bind(project)
        .bind(lead)
        .fetch_one(db)
        .await
        .unwrap()
    }

    impl Seeded {
        /// Ana is an admin of Acme, which added Splee to Slack workspace T1. Eve is only in Beta, on workspace T2.
        async fn start(db: PgPool) -> Self {
            let (fake, fake_url) = FakeSlack::start().await;
            let state = AppState { db: db.clone(), cfg: Arc::new(config(&fake_url)), events: tokio::sync::broadcast::channel(1).0 };
            let mut orgs = vec![];
            for (org, user, email, team) in [("Acme", "Ana Silva", "ana@acme.test", "T1"), ("Beta", "Eve", "eve@beta.test", "T2")] {
                let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(org).fetch_one(&db).await.unwrap();
                let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, $2, 'x') returning id")
                    .bind(email)
                    .bind(user)
                    .fetch_one(&db)
                    .await
                    .unwrap();
                sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(user).execute(&db).await.unwrap();
                sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                    .bind(sha256_hex(&format!("session-{team}")))
                    .bind(user)
                    .bind(org)
                    .execute(&db)
                    .await
                    .unwrap();
                sqlx::query("insert into slack_installs (org_id, team_id, team_name, bot_token_enc) values ($1, $2, $2, $3)")
                    .bind(org)
                    .bind(team)
                    .bind(crypto::encrypt("test-secret", &format!("xoxb-{team}")))
                    .execute(&db)
                    .await
                    .unwrap();
                orgs.push(org);
            }
            let mut projects = vec![];
            for org in &orgs {
                let project: Uuid =
                    sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Fieldnote') returning id").bind(org).fetch_one(&db).await.unwrap();
                sqlx::query("insert into slack_channels (project_id, channel_id, channel_name) values ($1, 'C1', 'sales')")
                    .bind(project)
                    .execute(&db)
                    .await
                    .unwrap();
                projects.push(project);
            }
            let project = projects[0];
            let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name, status) values ($1, 'Founders', 'working') returning id")
                .bind(project)
                .fetch_one(&db)
                .await
                .unwrap();
            let lead = lead_with_reply(&db, project, "Sarah Lee").await;
            let draft = draft_for(&db, project, lead).await;
            let quiet_lead = lead_with_reply(&db, project, "Sam Quiet").await;
            let beta_lead = lead_with_reply(&db, projects[1], "Bo Beta").await;
            let beta_draft = draft_for(&db, projects[1], beta_lead).await;

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", crate::routes::router()).with_state(state.clone());
            tokio::spawn(async move { axum::serve(listener, app).await });
            let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
            Self { state, base, client, fake, fake_url, acme: orgs[0], project, segment, lead, draft, quiet_lead, beta_draft }
        }

        /// A button press by `user` in `team` on a message in C1.
        fn press(&self, team: &str, user: &str, action_id: &str, value: impl ToString) -> Value {
            json!({
                "type": "block_actions",
                "team": { "id": team },
                "user": { "id": user, "team_id": team },
                "trigger_id": "trigger-1",
                "response_url": format!("{}/respond", self.fake_url),
                "container": { "type": "message", "channel_id": "C1", "message_ts": "1700000000.000100" },
                "message": { "text": "fallback", "blocks": [] },
                "actions": [{ "action_id": action_id, "value": value.to_string() }],
            })
        }

        async fn status(&self, id: Uuid) -> (String, String) {
            sqlx::query_as("select status, body from email_messages where id = $1").bind(id).fetch_one(&self.state.db).await.unwrap()
        }

        /// Sends a payload through the route the way Slack does, signed `age` seconds ago.
        async fn post_signed(&self, payload: &Value, secret: &str, age: i64) -> u16 {
            let body = format!("payload={}", Url::parse_with_params("http://x", [("p", payload.to_string())]).unwrap().query().unwrap().trim_start_matches("p="));
            let ts = (Utc::now().timestamp() - age).to_string();
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
            mac.update(format!("v0:{ts}:{body}").as_bytes());
            let res = self
                .client
                .post(format!("{}/slack/interactions", self.base))
                .header("content-type", "application/x-www-form-urlencoded")
                .header("x-slack-request-timestamp", ts)
                .header("x-slack-signature", format!("v0={}", hex::encode(mac.finalize().into_bytes())))
                .body(body)
                .send()
                .await
                .unwrap();
            res.status().as_u16()
        }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_hot_reply_mentions_its_owner_looked_up_once(db: PgPool) {
        let s = Seeded::start(db).await;
        let ana: Uuid = scalar(&s.state.db, "select id from users where email = 'ana@acme.test'").await;
        let bo: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('bo@acme.test', 'Bo', 'x') returning id")
            .fetch_one(&s.state.db)
            .await
            .unwrap();
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(s.acme).bind(bo).execute(&s.state.db).await.unwrap();
        let owned_by = |lead: Uuid, owner: Option<Uuid>| {
            sqlx::query("update leads set owner_id = $2 where id = $1").bind(lead).bind(owner).execute(&s.state.db)
        };
        let posts = || s.fake.calls("chat.postMessage").iter().map(|c| c["text"].as_str().unwrap().to_string()).collect::<Vec<_>>();

        crate::slack::post_hot_reply(&s.state, s.lead).await;
        assert_eq!(posts(), ["Hot reply: Sarah Lee (Acme) replied to Fieldnote"], "nobody owns it yet");
        assert!(s.fake.calls("users.lookupByEmail").is_empty());

        owned_by(s.lead, Some(ana)).await.unwrap();
        crate::slack::post_hot_reply(&s.state, s.lead).await;
        crate::slack::post_hot_reply(&s.state, s.lead).await;
        assert_eq!(posts()[1..], ["Hot reply: Sarah Lee (Acme) replied to Fieldnote for <@UANA>"; 2]);
        let headline = &s.fake.calls("chat.postMessage")[1]["blocks"][0]["text"]["text"];
        assert_eq!(headline, "*Hot reply:* Sarah Lee (Acme) replied to Fieldnote for <@UANA>");
        assert_eq!(s.fake.calls("users.lookupByEmail").len(), 1, "the second post used the cache");
        assert_eq!(s.fake.calls("users.lookupByEmail")[0]["email"], "ana@acme.test");

        // Someone Slack doesn't know is looked up once too, and the post goes out without a mention.
        owned_by(s.quiet_lead, Some(bo)).await.unwrap();
        crate::slack::post_hot_reply(&s.state, s.quiet_lead).await;
        crate::slack::post_hot_reply(&s.state, s.quiet_lead).await;
        assert_eq!(posts()[3..], ["Hot reply: Sam Quiet (Acme) replied to Fieldnote"; 2]);
        assert_eq!(s.fake.calls("users.lookupByEmail").len(), 2);
        let cached: Vec<(String, Option<String>)> = sqlx::query_as("select email::text, slack_user_id from slack_user_ids order by email")
            .fetch_all(&s.state.db)
            .await
            .unwrap();
        assert_eq!(cached, [("ana@acme.test".into(), Some("UANA".into())), ("bo@acme.test".into(), None)]);

        // A changed address is looked up again.
        sqlx::query("update users set email = 'ana.silva@acme.test' where id = $1").bind(ana).execute(&s.state.db).await.unwrap();
        crate::slack::post_hot_reply(&s.state, s.lead).await;
        assert_eq!(s.fake.calls("users.lookupByEmail").len(), 3);
        assert_eq!(posts().last().unwrap(), "Hot reply: Sarah Lee (Acme) replied to Fieldnote");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn only_signed_fresh_requests_get_in_and_the_work_comes_after_the_answer(db: PgPool) {
        let s = Seeded::start(db).await;
        let press = s.press("T1", "UANA", "send_reply", s.draft);
        assert_eq!(s.post_signed(&press, "not the secret", 0).await, 401);
        assert_eq!(s.post_signed(&press, "signing", 301).await, 401, "stale");
        let unsigned = s.client.post(format!("{}/slack/interactions", s.base)).body("payload={}").send().await.unwrap();
        assert_eq!(unsigned.status(), 401);
        assert!(s.fake.calls.lock().unwrap().is_empty(), "nothing was looked up");
        assert_eq!(s.status(s.draft).await.0, "draft");

        assert_eq!(s.post_signed(&press, "signing", 0).await, 200);
        for _ in 0..100 {
            if !s.fake.calls("chat.update").is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(s.status(s.draft).await.0, "scheduled");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn send_reply_sends_the_draft_once_however_often_it_is_pressed(db: PgPool) {
        let s = Seeded::start(db).await;
        let press = s.press("T1", "UANA", "send_reply", s.draft);
        // Two people at once: one sends, the other is told.
        tokio::join!(handle(&s.state, press.clone()), handle(&s.state, press.clone()));
        assert_eq!(s.status(s.draft).await, ("scheduled".into(), "Hi, Tuesday works.".into()));
        assert_eq!(s.fake.told(), ["Ana Silva already sent this reply."]);

        let update = s.fake.last_update();
        assert_eq!((update["channel"].as_str(), update["ts"].as_str()), (Some("C1"), Some("1700000000.000100")));
        let blocks = update["blocks"].to_string();
        assert!(blocks.contains("*Reply sent*") && blocks.contains("Ana Silva sent this reply from Slack."), "{blocks}");
        assert!(!blocks.contains("send_reply"), "no button left to press");

        // Pressed again later, after the sender took it.
        sqlx::query("update email_messages set status = 'sent' where id = $1").bind(s.draft).execute(&s.state.db).await.unwrap();
        handle(&s.state, press).await;
        assert_eq!(s.fake.told().last().unwrap(), "Ana Silva already sent this reply.");
        assert_eq!(s.status(s.draft).await.0, "sent", "not queued again");
        let sent = "select count(*) from email_messages where direction = 'outbound' and kind in ('auto_reply', 'manual_reply') and status <> 'draft'";
        let outbound: i64 = scalar(&s.state.db, sent).await;
        assert_eq!(outbound, 1);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn edit_opens_the_draft_and_sending_the_modal_sends_the_edit(db: PgPool) {
        let s = Seeded::start(db).await;
        handle(&s.state, s.press("T1", "UANA", "edit_reply", s.lead)).await;
        let opened = s.fake.calls("views.open").pop().expect("a modal opened");
        assert_eq!(opened["trigger_id"], "trigger-1");
        let view = &opened["view"];
        assert_eq!(view["blocks"][1]["element"]["initial_value"], "Hi, Tuesday works.");
        assert_eq!(s.status(s.draft).await.0, "draft", "opening sends nothing");

        let submit = |metadata: &Value, text: &str| {
            json!({
                "type": "view_submission",
                "team": { "id": "T1" },
                "user": { "id": "UANA", "team_id": "T1" },
                "view": {
                    "callback_id": "edit_reply",
                    "private_metadata": metadata.as_str().unwrap(),
                    "state": { "values": { "reply": { "body": { "type": "plain_text_input", "value": text } } } },
                },
            })
        };
        handle(&s.state, submit(&view["private_metadata"], "Hi Sarah, Tuesday at 10 works.")).await;
        assert_eq!(s.status(s.draft).await, ("scheduled".into(), "Hi Sarah, Tuesday at 10 works.".into()));
        let update = s.fake.last_update();
        assert_eq!(update["ts"], "1700000000.000100", "the message the modal came from");
        assert!(update["blocks"].to_string().contains("Tuesday at 10"));
        // A second submission of an old modal finds the reply sent.
        handle(&s.state, submit(&view["private_metadata"], "Something else")).await;
        assert_eq!(s.status(s.draft).await.1, "Hi Sarah, Tuesday at 10 works.");
        assert_eq!(s.fake.told(), ["Ana Silva already sent this reply."]);

        // With nothing drafted, the modal starts empty and sends what was written as the inbox does.
        handle(&s.state, s.press("T1", "UANA", "edit_reply", s.quiet_lead)).await;
        let view = s.fake.calls("views.open").pop().unwrap()["view"].clone();
        assert!(view["blocks"][1]["element"].get("initial_value").is_none());
        handle(&s.state, submit(&view["private_metadata"], "Hi Sam, happy to.")).await;
        let (kind, status, body): (String, String, String) =
            sqlx::query_as("select kind, status, body from email_messages where lead_id = $1 and direction = 'outbound'")
                .bind(s.quiet_lead)
                .fetch_one(&s.state.db)
                .await
                .unwrap();
        assert_eq!((kind.as_str(), status.as_str(), body.as_str()), ("manual_reply", "scheduled", "Hi Sam, happy to."));
        handle(&s.state, submit(&view["private_metadata"], "Hi again")).await;
        assert_eq!(s.fake.told().last().unwrap(), "This lead was already answered, so your reply wasn't sent.");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn snooze_brings_the_thread_back_tomorrow(db: PgPool) {
        let s = Seeded::start(db).await;
        handle(&s.state, s.press("T1", "UANA", "snooze", s.lead)).await;
        let due: bool = sqlx::query_scalar("select remind_at between now() + interval '23 hours' and now() + interval '25 hours' from leads where id = $1")
            .bind(s.lead)
            .fetch_one(&s.state.db)
            .await
            .unwrap();
        assert!(due);
        let blocks = s.fake.last_update()["blocks"].to_string();
        assert!(blocks.contains("Ana Silva snoozed this for a day.") && blocks.contains("send_reply"), "{blocks}");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn people_outside_the_org_and_other_orgs_leads_are_refused(db: PgPool) {
        let s = Seeded::start(db).await;
        // Eve is in Slack workspace T1 but not in Acme.
        handle(&s.state, s.press("T1", "UEVE", "send_reply", s.draft)).await;
        assert_eq!(s.fake.told(), ["Only members of Acme in Splee can do this, and eve@beta.test isn't one. Ask an admin to invite you."]);
        // Someone Slack can't find.
        handle(&s.state, s.press("T1", "UNOBODY", "send_reply", s.draft)).await;
        assert_eq!(s.fake.told().len(), 2);
        assert_eq!(s.status(s.draft).await.0, "draft");

        // Ana is in Acme, and the draft is Beta's.
        for press in [s.press("T1", "UANA", "send_reply", s.beta_draft), s.press("T1", "UANA", "approve_email", s.beta_draft)] {
            handle(&s.state, press).await;
            assert_eq!(s.fake.told().last().unwrap(), NOT_YOURS);
        }
        assert_eq!(s.status(s.beta_draft).await.0, "draft");
        // A workspace nobody connected.
        handle(&s.state, s.press("T404", "UANA", "send_reply", s.draft)).await;
        assert_eq!(s.fake.told().last().unwrap(), SIGNED_OUT);
        assert!(s.fake.calls("chat.update").is_empty());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_digest_approves_and_skips_through_the_emails_code(db: PgPool) {
        let s = Seeded::start(db).await;
        let db = &s.state.db;
        let mut emails = vec![];
        for n in 0..3 {
            let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, segment_id, full_name, company_name) values ($1, $2, $3, 'Acme') returning id")
                .bind(s.project)
                .bind(s.segment)
                .bind(format!("Lead {n}"))
                .fetch_one(db)
                .await
                .unwrap();
            let email: Uuid = sqlx::query_scalar(
                "insert into email_messages (project_id, lead_id, segment_id, direction, kind, status, subject, body)
                 values ($1, $2, $3, 'outbound', 'initial', 'draft', 'Quick idea', 'Hi') returning id",
            )
            .bind(s.project)
            .bind(lead)
            .bind(s.segment)
            .fetch_one(db)
            .await
            .unwrap();
            emails.push(email);
        }
        crate::slack::post_digests(&s.state).await.unwrap();
        crate::slack::post_digests(&s.state).await.unwrap();
        let posted = s.fake.calls("chat.postMessage");
        assert_eq!(posted.len(), 1, "once a day");
        assert_eq!((posted[0]["channel"].as_str(), posted[0]["text"].as_str()), (Some("C1"), Some("3 emails waiting for approval in Fieldnote")));
        let blocks = posted[0]["blocks"].clone();

        let mut press = s.press("T1", "UANA", "approve_email", emails[0]);
        press["message"]["blocks"] = blocks.clone();
        handle(&s.state, press.clone()).await;
        assert_eq!(s.status(emails[0]).await.0, "approved");
        let lead_status: String = sqlx::query_scalar("select l.status from leads l join email_messages e on e.lead_id = l.id where e.id = $1")
            .bind(emails[0])
            .fetch_one(db)
            .await
            .unwrap();
        assert_eq!(lead_status, "queued", "as the Approve button in the app does");
        let updated = s.fake.last_update()["blocks"].clone();
        assert!(updated.to_string().contains("Ana Silva approved this email."));
        assert!(!updated.to_string().contains(&format!("email:{}", emails[0])));
        handle(&s.state, press).await;
        assert_eq!(s.fake.told(), ["Ana Silva already approved this email."]);

        let mut skip = s.press("T1", "UANA", "skip_email", emails[1]);
        skip["message"]["blocks"] = blocks.clone();
        handle(&s.state, skip).await;
        assert_eq!(s.status(emails[1]).await.0, "cancelled");
        assert!(s.fake.last_update()["blocks"].to_string().contains("Ana Silva skipped this email."));

        // Approve all takes what the digest counted, not an email written after it.
        let later: Uuid = sqlx::query_scalar(
            "insert into email_messages (project_id, lead_id, segment_id, direction, kind, status, subject, body, created_at)
             select project_id, lead_id, segment_id, direction, kind, 'draft', subject, body, now() + interval '1 minute'
             from email_messages where id = $1 returning id",
        )
        .bind(emails[2])
        .fetch_one(db)
        .await
        .unwrap();
        let campaign = blocks.as_array().unwrap().iter().find(|b| b["block_id"] == format!("campaign:{}", s.segment)).unwrap().clone();
        let mut all = s.press("T1", "UANA", "approve_campaign", campaign["accessory"]["value"].as_str().unwrap());
        all["message"]["blocks"] = blocks;
        handle(&s.state, all.clone()).await;
        assert_eq!(s.status(emails[2]).await.0, "approved");
        assert_eq!(s.status(emails[1]).await.0, "cancelled", "a skipped email stays skipped");
        assert_eq!(s.status(later).await.0, "draft");
        assert!(s.fake.last_update()["blocks"].to_string().contains("Ana Silva approved 1 email."));
        handle(&s.state, all).await;
        assert_eq!(s.fake.told().last().unwrap(), "These emails were already approved or skipped.");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_hot_reply_posts_to_the_projects_channel(db: PgPool) {
        let s = Seeded::start(db).await;
        crate::slack::post_hot_reply(&s.state, s.lead).await;
        let posted = s.fake.calls("chat.postMessage").pop().unwrap();
        assert_eq!(posted["channel"], "C1");
        assert_eq!(posted["text"], "Hot reply: Sarah Lee (Acme) replied to Fieldnote");
        let blocks = posted["blocks"].to_string();
        assert!(blocks.contains(&s.draft.to_string()) && blocks.contains("Hi, Tuesday works."));

        // A project without a channel posts nothing.
        sqlx::query("delete from slack_channels where project_id = $1").bind(s.project).execute(&s.state.db).await.unwrap();
        crate::slack::post_hot_reply(&s.state, s.lead).await;
        assert_eq!(s.fake.calls("chat.postMessage").len(), 1);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn adding_to_slack_takes_a_state_once_for_the_same_admin(db: PgPool) {
        let s = Seeded::start(db).await;
        sqlx::query("delete from slack_installs where org_id = $1").bind(s.acme).execute(&s.state.db).await.unwrap();
        let s = &s;
        let start = || async {
            let res = s.client.post(format!("{}/organization/slack/install", s.base)).header("cookie", "splee_session=session-T1").send().await.unwrap();
            assert_eq!(res.status(), 200);
            let url = Url::parse(res.json::<Value>().await.unwrap()["url"].as_str().unwrap()).unwrap();
            assert_eq!(url.as_str().split('?').next(), Some(slack::AUTHORIZE_URL));
            url.query_pairs().into_owned().collect::<HashMap<String, String>>()
        };
        let back = |session: &'static str, state: String| async move {
            let url = Url::parse_with_params(&format!("{}/slack/oauth/callback", s.base), [("code", "the-code"), ("state", state.as_str())]).unwrap();
            let res = s.client.get(url).header("cookie", format!("splee_session={session}")).send().await.unwrap();
            let location = res.headers()["location"].to_str().unwrap().to_string();
            assert!(location.starts_with("/app/notifications?"), "{location}");
            Url::parse(&format!("http://x{location}")).unwrap().query_pairs().into_owned().collect::<HashMap<String, String>>()
        };

        let q = start().await;
        assert_eq!(q["client_id"], "123.456");
        assert!(q["scope"].split(',').any(|s| s == "users:read.email") && q["scope"].contains("chat:write.public"));
        assert_eq!(q["redirect_uri"], "http://localhost:3405/api/slack/oauth/callback");
        assert_eq!(q["code_challenge_method"], "S256");

        // Someone else's session, a forged signature and an expired state get nothing.
        assert!(back("session-T2", q["state"].clone()).await["slack_error"].starts_with("You switched organizations"));
        assert_eq!(back("session-T1", q["state"].clone()).await["slack_error"], START_AGAIN, "used up by the try above");
        let q = start().await;
        let (nonce, _) = q["state"].split_once('.').unwrap();
        assert_eq!(back("session-T1", format!("{nonce}.{}", "0".repeat(64))).await["slack_error"], START_AGAIN);
        let q = start().await;
        sqlx::query("update slack_oauth_states set expires_at = now() - interval '1 second'").execute(&s.state.db).await.unwrap();
        assert_eq!(back("session-T1", q["state"].clone()).await["slack_error"], START_AGAIN);
        assert!(s.fake.calls("oauth.v2.access").is_empty(), "no code was traded");

        let q = start().await;
        assert_eq!(back("session-T1", q["state"].clone()).await["slack"], "connected");
        let traded = s.fake.calls("oauth.v2.access").pop().unwrap();
        assert_eq!((traded["code"].as_str(), traded["client_secret"].as_str()), (Some("the-code"), Some("shh")));
        assert_eq!(pkce_challenge(traded["code_verifier"].as_str().unwrap()), q["code_challenge"]);
        let (team, token): (String, String) =
            sqlx::query_as("select team_id, bot_token_enc from slack_installs where org_id = $1").bind(s.acme).fetch_one(&s.state.db).await.unwrap();
        assert_eq!((team.as_str(), crypto::decrypt("test-secret", &token).unwrap().as_str()), ("TNEW", "xoxb-new"));
        let picked: i64 = scalar(&s.state.db, "select count(*) from slack_channels").await;
        assert_eq!(picked, 2, "the old workspace's install was gone before, so nothing to clear");
        assert_eq!(back("session-T1", q["state"].clone()).await["slack_error"], START_AGAIN, "once");

        // A revoke Slack refuses keeps the token, so disconnecting can be tried again.
        let disconnect = || s.client.delete(format!("{}/organization/slack", s.base)).header("cookie", "splee_session=session-T1").send();
        *s.fake.revoke_error.lock().unwrap() = Some("ratelimited");
        assert_eq!(disconnect().await.unwrap().status(), 400);
        let kept: i64 = sqlx::query_scalar("select count(*) from slack_installs where org_id = $1").bind(s.acme).fetch_one(&s.state.db).await.unwrap();
        assert_eq!(kept, 1);

        // A token Slack already dropped counts as revoked, and disconnecting forgets it and the channel picks.
        *s.fake.revoke_error.lock().unwrap() = Some("token_revoked");
        assert_eq!(disconnect().await.unwrap().status(), 200);
        assert_eq!(s.fake.calls("auth.revoke").len(), 2);
        let left: i64 = sqlx::query_scalar("select count(*) from slack_installs where org_id = $1").bind(s.acme).fetch_one(&s.state.db).await.unwrap();
        assert_eq!(left, 0);
        let picks: i64 = sqlx::query_scalar("select count(*) from slack_channels where project_id = $1").bind(s.project).fetch_one(&s.state.db).await.unwrap();
        assert_eq!(picks, 0);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_project_picks_a_channel_the_bot_can_see(db: PgPool) {
        let s = Seeded::start(db).await;
        let pick = |channel: Option<&str>| {
            s.client
                .put(format!("{}/projects/{}/slack-channel", s.base, s.project))
                .header("cookie", "splee_session=session-T1")
                .json(&json!({ "channel_id": channel }))
                .send()
        };
        assert_eq!(pick(Some("G2")).await.unwrap().status(), 200);
        let name: String =
            sqlx::query_scalar("select channel_name from slack_channels where project_id = $1").bind(s.project).fetch_one(&s.state.db).await.unwrap();
        assert_eq!(name, "deals");
        assert_eq!(pick(Some("C404")).await.unwrap().status(), 400);
        assert_eq!(pick(None).await.unwrap().status(), 200);
        let left: i64 = sqlx::query_scalar("select count(*) from slack_channels where project_id = $1").bind(s.project).fetch_one(&s.state.db).await.unwrap();
        assert_eq!(left, 0);

        let res = s.client.get(format!("{}/organization/slack/channels", s.base)).header("cookie", "splee_session=session-T1").send().await.unwrap();
        let channels: Value = res.json().await.unwrap();
        assert_eq!(channels, json!([{ "id": "G2", "name": "deals", "private": true }, { "id": "C1", "name": "sales", "private": false }]));
    }
}
