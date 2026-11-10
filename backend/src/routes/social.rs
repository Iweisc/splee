//! The Social page: replies and posts for X, Reddit, Bluesky, Mastodon, Threads and LinkedIn, comments for YouTube, answers
//! for Stack Exchange and articles for DEV, that a social run drafted or the user wrote, which crate::social posts once
//! approved, and connecting the accounts they go out from. LinkedIn goes out through the project's Unipile connection, see
//! routes::linkedin.

use axum::extract::{Path, Query, State};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::settings::load_or_create;
use super::{util_input, util_scope};
use crate::agent::x::{self, Cookies};
use crate::agent::{self, kind};
use crate::auth::Auth;
use crate::bluesky::{self, Bluesky};
use crate::crypto;
use crate::dev::{self, Dev};
use crate::error::{AppError, AppResult};
use crate::models::{ProjectSettings, SocialPost};
use crate::mastodon::{self, Mastodon};
use crate::reddit::{self, Login, Reddit};
use crate::social::{self, Failure, PLATFORMS, length, max_length, min_length, takes_posts};
use crate::stackexchange::{self, StackExchange};
use crate::state::AppState;
use crate::threads::Threads;
use crate::youtube::{self, YouTube};

const STATUSES: [&str; 6] = ["draft", "approved", "posting", "posted", "failed", "skipped"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/social", get(list).post(create))
        .route("/projects/{id}/social/runs", post(start_run))
        .route("/social/{id}", patch(update).delete(remove))
        .route("/projects/{id}/integrations/x", put(connect_x).delete(disconnect_x))
        .route("/projects/{id}/integrations/reddit", put(connect_reddit).delete(disconnect_reddit))
        .route("/projects/{id}/integrations/bluesky", put(connect_bluesky).delete(disconnect_bluesky))
        .route("/projects/{id}/integrations/mastodon", put(connect_mastodon).delete(disconnect_mastodon))
        .route("/projects/{id}/integrations/threads", put(connect_threads).delete(disconnect_threads))
        .route("/projects/{id}/integrations/youtube", put(connect_youtube).delete(disconnect_youtube))
        .route("/projects/{id}/integrations/stackexchange", put(connect_stackexchange).delete(disconnect_stackexchange))
        .route("/projects/{id}/integrations/dev", put(connect_dev).delete(disconnect_dev))
}

#[derive(Deserialize)]
struct ListQuery {
    /// One status or several, comma-separated.
    status: Option<String>,
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(q): Query<ListQuery>) -> AppResult<Json<Vec<SocialPost>>> {
    auth.project(&state.db, id).await?;
    let statuses: Vec<&str> = q.status.as_deref().unwrap_or_default().split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    for s in &statuses {
        util_input::one_of("status", s, &STATUSES)?;
    }
    let posts = sqlx::query_as::<_, SocialPost>(
        "select * from social_posts where project_id = $1 and (cardinality($2::text[]) = 0 or status = any($2))
         order by coalesce(posted_at, created_at) desc limit 500",
    )
    .bind(id)
    .bind(&statuses)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(posts))
}

/// What a post needs before it can go out: text within the platform's limits, a subreddit and title for a new Reddit post, a
/// title for a DEV article, and something to answer for YouTube and Stack Exchange.
fn check(platform: &str, reply: bool, subreddit: &str, title: &str, body: &str, mastodon_chars: usize) -> AppResult<()> {
    if body.trim().is_empty() {
        return Err(AppError::BadRequest("Write something first".into()));
    }
    if !reply && !takes_posts(platform) {
        let what = if platform == "stackexchange" { "answers" } else { "comments" };
        return Err(AppError::BadRequest(format!("{} only takes {what}", social::name(platform))));
    }
    let what = match (platform, reply) {
        ("stackexchange", _) => "Answers",
        ("dev", _) => "Articles",
        ("linkedin" | "youtube", true) => "Comments",
        (_, true) => "Replies",
        _ => "Posts",
    };
    let (min, max, n) = (min_length(platform), max_length(platform, reply, mastodon_chars), length(platform, body.trim()));
    if n > max {
        return Err(AppError::BadRequest(format!("{what} on {} are at most {max} characters", social::name(platform))));
    }
    if n < min {
        return Err(AppError::BadRequest(format!("{what} on {} are at least {min} characters", social::name(platform))));
    }
    if platform == "reddit" && !reply && (subreddit.is_empty() || title.trim().is_empty()) {
        return Err(AppError::BadRequest("A Reddit post needs a subreddit and a title".into()));
    }
    if platform == "dev" {
        if title.trim().is_empty() {
            return Err(AppError::BadRequest("A DEV article needs a title".into()));
        }
        if title.chars().count() > dev::TITLE_CHARS {
            return Err(AppError::BadRequest(format!("DEV titles are at most {} characters", dev::TITLE_CHARS)));
        }
    }
    if title.chars().count() > 300 {
        return Err(AppError::BadRequest("Reddit titles are at most 300 characters".into()));
    }
    Ok(())
}

/// Reddit original posts and DEV articles have a title.
fn titled(platform: &str, reply: bool) -> bool {
    matches!(platform, "reddit" | "dev") && !reply
}

/// The project's Mastodon instance's post limit, for `check`.
async fn mastodon_chars(state: &AppState, project_id: Uuid) -> AppResult<usize> {
    let max: Option<i32> = sqlx::query_scalar("select mastodon_max_chars from project_settings where project_id = $1").bind(project_id).fetch_optional(&state.db).await?;
    Ok(max.unwrap_or(500).max(1) as usize)
}

/// A subreddit as typed, like "r/sales", or empty.
fn subreddit_input(raw: &str) -> AppResult<String> {
    if raw.trim().is_empty() {
        return Ok(String::new());
    }
    reddit::subreddit(raw).ok_or_else(|| AppError::BadRequest(format!("{} isn't a subreddit name", raw.trim())))
}

/// Approving a post for an account that isn't connected would leave it waiting with no word on why.
async fn require_account(state: &AppState, project_id: Uuid, platform: &str) -> AppResult<()> {
    let connected = social::accounts(&state.db, project_id).await?.is_some_and(|a| a.connected().contains(&platform));
    match (connected, platform) {
        (true, _) => Ok(()),
        (false, "linkedin") => Err(AppError::BadRequest("Connect LinkedIn sending on the integrations page first".into())),
        (false, p) => Err(AppError::BadRequest(format!("Connect {} on the integrations page first", social::name(p)))),
    }
}

#[derive(Deserialize)]
struct CreateBody {
    platform: String,
    body: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    subreddit: String,
    #[serde(default)]
    tags: Vec<String>,
    /// "draft" (the default) or "approved".
    status: Option<String>,
}

/// A post the user wrote by hand.
async fn create(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<CreateBody>) -> AppResult<Json<SocialPost>> {
    auth.project(&state.db, id).await?;
    util_input::one_of("platform", &b.platform, &PLATFORMS)?;
    let status = b.status.as_deref().unwrap_or("draft");
    util_input::one_of("status", status, &["draft", "approved"])?;
    let subreddit = if b.platform == "reddit" { subreddit_input(&b.subreddit)? } else { String::new() };
    let title = if titled(&b.platform, false) { b.title.trim() } else { "" };
    let tags = if b.platform == "dev" { dev::tags(&b.tags) } else { Vec::new() };
    check(&b.platform, false, &subreddit, title, &b.body, mastodon_chars(&state, id).await?)?;
    if status == "approved" {
        require_account(&state, id, &b.platform).await?;
    }
    let post = sqlx::query_as::<_, SocialPost>(
        "insert into social_posts (project_id, platform, subreddit, title, tags, body, status) values ($1, $2, $3, $4, $5, $6, $7) returning *",
    )
    .bind(id)
    .bind(&b.platform)
    .bind(subreddit)
    .bind(title)
    .bind(tags)
    .bind(b.body.trim())
    .bind(status)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(post))
}

#[derive(Deserialize)]
struct UpdateBody {
    body: Option<String>,
    title: Option<String>,
    subreddit: Option<String>,
    tags: Option<Vec<String>>,
    status: Option<String>,
}

async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<UpdateBody>) -> AppResult<Json<SocialPost>> {
    let post: SocialPost = util_scope::load(&state.db, "social_posts", id, auth.org_id).await?;
    if post.status == "posted" || post.status == "posting" {
        return Err(AppError::BadRequest("This is already posted".into()));
    }
    let status = b.status.unwrap_or_else(|| post.status.clone());
    if status != post.status {
        util_input::one_of("status", &status, &["draft", "approved", "skipped"])?;
    }
    let reply = post.in_reply_to.is_some();
    let body = b.body.map(|s| s.trim().to_string()).unwrap_or(post.body);
    let subreddit = match (post.platform.as_str(), reply) {
        ("reddit", false) => b.subreddit.map(|s| subreddit_input(&s)).transpose()?.unwrap_or(post.subreddit),
        _ => post.subreddit,
    };
    let title = if titled(&post.platform, reply) { b.title.map(|t| t.trim().to_string()).unwrap_or(post.title) } else { post.title };
    let tags = if post.platform == "dev" { b.tags.map(|t| dev::tags(&t)).unwrap_or(post.tags) } else { post.tags };
    if status == "approved" {
        check(&post.platform, reply, &subreddit, &title, &body, mastodon_chars(&state, post.project_id).await?)?;
        require_account(&state, post.project_id, &post.platform).await?;
    }
    let updated = sqlx::query_as::<_, SocialPost>(
        "update social_posts set body = $2, title = $3, subreddit = $4, tags = $5, status = $6,
           error = case when $6 = status then error end, updated_at = now()
         where id = $1 and status not in ('posting', 'posted') returning *",
    )
    .bind(id)
    .bind(body)
    .bind(title)
    .bind(subreddit)
    .bind(tags)
    .bind(&status)
    .fetch_optional(&state.db)
    .await?
    // The poster took it in the meantime.
    .ok_or_else(|| AppError::BadRequest("This is already posted".into()))?;
    Ok(Json(updated))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let post: SocialPost = util_scope::load(&state.db, "social_posts", id, auth.org_id).await?;
    if post.status != "draft" {
        return Err(AppError::BadRequest("Only drafts can be deleted. Skip it instead".into()));
    }
    sqlx::query("delete from social_posts where id = $1 and status = 'draft'").bind(id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn start_run(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    if social::accounts(&state.db, id).await?.is_none_or(|a| a.connected().is_empty()) {
        return Err(AppError::BadRequest("Connect X, Reddit, Bluesky, Mastodon, Threads, YouTube, Stack Exchange, DEV or LinkedIn sending on the integrations page first".into()));
    }
    let busy: bool = sqlx::query_scalar("select exists (select 1 from agent_runs where project_id = $1 and kind = $2 and status in ('queued', 'running'))")
        .bind(id)
        .bind(kind::SOCIAL)
        .fetch_one(&state.db)
        .await?;
    if busy {
        return Err(AppError::Conflict("The agent is already looking for threads. Try again when it's done.".into()));
    }
    let run_id = agent::enqueue(&state.db, kind::SOCIAL, Some(id), Some(auth.org_id), json!({})).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

#[derive(Deserialize)]
struct XBody {
    auth_token: String,
    ct0: String,
}

/// Saves the cookies once bird signs in with them, with the handle they belong to.
async fn connect_x(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<XBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let bird = state.cfg.bird_bin.as_deref().ok_or_else(|| AppError::BadRequest("Posting to X needs the bird CLI on the server. Set BIRD_BIN".into()))?;
    let cookies = Cookies { auth_token: b.auth_token.trim().to_string(), ct0: b.ct0.trim().to_string() };
    if cookies.auth_token.is_empty() || cookies.ct0.is_empty() {
        return Err(AppError::BadRequest("Paste both auth_token and ct0".into()));
    }
    let handle = x::whoami(bird, &cookies)
        .await
        .map_err(|e| AppError::BadRequest(format!("X didn't accept those cookies ({e:#}). Copy them again from a browser that's signed in to x.com")))?;
    let secret = &state.cfg.app_secret;
    set_x(&state, id, &crypto::encrypt(secret, &cookies.auth_token), &crypto::encrypt(secret, &cookies.ct0), &handle).await
}

async fn disconnect_x(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_x(&state, id, "", "", "").await
}

async fn set_x(state: &AppState, id: Uuid, token: &str, ct0: &str, handle: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set x_auth_token_enc = $2, x_ct0_enc = $3, x_handle = $4, x_error = null, x_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(token)
    .bind(ct0)
    .bind(handle)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct RedditBody {
    client_id: String,
    secret: String,
    username: String,
    password: String,
}

/// Saves the login once Reddit signs it in, with the username as Reddit spells it.
async fn connect_reddit(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<RedditBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let username = b.username.trim().trim_start_matches('/');
    // Passwords can start or end with a space, so only the other fields are trimmed.
    let login = Login {
        client_id: b.client_id.trim().into(),
        secret: b.secret.trim().into(),
        username: username.strip_prefix("u/").unwrap_or(username).into(),
        password: b.password,
    };
    if login.client_id.is_empty() || login.secret.is_empty() || login.username.is_empty() || login.password.is_empty() {
        return Err(AppError::BadRequest("Fill in the client id, secret, username and password".into()));
    }
    let refused = |e: Failure| AppError::BadRequest(e.message());
    let name = Reddit::sign_in(&login).await.map_err(refused)?.me().await.map_err(refused)?;
    let secret = &state.cfg.app_secret;
    let enc = (crypto::encrypt(secret, &login.secret), crypto::encrypt(secret, &login.password));
    set_reddit(&state, id, &login.client_id, &enc.0, &name, &enc.1).await
}

async fn disconnect_reddit(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_reddit(&state, id, "", "", "", "").await
}

async fn set_reddit(state: &AppState, id: Uuid, client_id: &str, secret: &str, username: &str, password: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set reddit_client_id = $2, reddit_secret_enc = $3, reddit_username = $4, reddit_password_enc = $5,
           reddit_error = null, reddit_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(client_id)
    .bind(secret)
    .bind(username)
    .bind(password)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct BlueskyBody {
    handle: String,
    password: String,
}

/// Saves the app password once Bluesky signs in with it, with the handle as Bluesky spells it.
async fn connect_bluesky(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<BlueskyBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let login = bluesky::Login { handle: b.handle.trim().trim_start_matches('@').to_string(), password: b.password.trim().to_string() };
    if login.handle.is_empty() || login.password.is_empty() {
        return Err(AppError::BadRequest("Fill in the handle and app password".into()));
    }
    let session = Bluesky::sign_in(&login).await.map_err(|e| AppError::BadRequest(e.message()))?;
    set_bluesky(&state, id, &session.handle, &crypto::encrypt(&state.cfg.app_secret, &login.password)).await
}

async fn disconnect_bluesky(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_bluesky(&state, id, "", "").await
}

async fn set_bluesky(state: &AppState, id: Uuid, handle: &str, password: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set bluesky_handle = $2, bluesky_password_enc = $3, bluesky_error = null, bluesky_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(handle)
    .bind(password)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct MastodonBody {
    url: String,
    token: String,
}

/// Saves the token once the instance accepts it, with the account's handle and the instance's post limit.
async fn connect_mastodon(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<MastodonBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let base = mastodon::instance_url(&b.url).ok_or_else(|| AppError::BadRequest("Write the instance's address, like mastodon.social".into()))?;
    let token = b.token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("Paste the access token".into()));
    }
    let account = Mastodon::new(&base, token).await.map_err(|e| AppError::BadRequest(e.message()))?.me().await.map_err(|e| AppError::BadRequest(e.message()))?;
    let enc = crypto::encrypt(&state.cfg.app_secret, token);
    set_mastodon(&state, id, &base, &enc, &account.handle, account.max_chars).await
}

async fn disconnect_mastodon(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_mastodon(&state, id, "", "", "", 500).await
}

async fn set_mastodon(state: &AppState, id: Uuid, url: &str, token: &str, handle: &str, max_chars: i32) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set mastodon_url = $2, mastodon_token_enc = $3, mastodon_handle = $4, mastodon_max_chars = $5,
           mastodon_error = null, mastodon_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(url)
    .bind(token)
    .bind(handle)
    .bind(max_chars)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct ThreadsBody {
    token: String,
}

/// Saves the token once Threads accepts it, with the account's user id and username.
async fn connect_threads(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<ThreadsBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let token = b.token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("Paste the access token".into()));
    }
    let account = Threads::new(token, "").map_err(|e| AppError::BadRequest(e.message()))?.me().await.map_err(|e| AppError::BadRequest(e.message()))?;
    set_threads(&state, id, &crypto::encrypt(&state.cfg.app_secret, token), &account.id, &account.username).await
}

async fn disconnect_threads(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_threads(&state, id, "", "", "").await
}

async fn set_threads(state: &AppState, id: Uuid, token: &str, user_id: &str, username: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set threads_token_enc = $2, threads_user_id = $3, threads_username = $4, threads_error = null, threads_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(token)
    .bind(user_id)
    .bind(username)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct YouTubeBody {
    client_id: String,
    secret: String,
    refresh_token: String,
}

/// Saves the OAuth client and refresh token once Google trades it for an access token, with the channel it belongs to.
async fn connect_youtube(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<YouTubeBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let login = youtube::Login { client_id: b.client_id.trim().into(), secret: b.secret.trim().into(), refresh_token: b.refresh_token.trim().into() };
    if login.client_id.is_empty() || login.secret.is_empty() || login.refresh_token.is_empty() {
        return Err(AppError::BadRequest("Fill in the client id, client secret and refresh token".into()));
    }
    let refused = |e: Failure| AppError::BadRequest(e.message());
    let channel = YouTube::sign_in(&login).await.map_err(refused)?.me().await.map_err(refused)?;
    let secret = &state.cfg.app_secret;
    let enc = (crypto::encrypt(secret, &login.secret), crypto::encrypt(secret, &login.refresh_token));
    set_youtube(&state, id, &login.client_id, &enc.0, &enc.1, &channel.id, &channel.name).await
}

async fn disconnect_youtube(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_youtube(&state, id, "", "", "", "", "").await
}

async fn set_youtube(state: &AppState, id: Uuid, client_id: &str, secret: &str, token: &str, channel_id: &str, channel: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set youtube_client_id = $2, youtube_secret_enc = $3, youtube_token_enc = $4, youtube_channel_id = $5,
           youtube_channel = $6, youtube_error = null, youtube_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(client_id)
    .bind(secret)
    .bind(token)
    .bind(channel_id)
    .bind(channel)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct StackExchangeBody {
    key: String,
    token: String,
    site: String,
}

/// Saves the key and token once Stack Exchange says the token can write and has a user on the site, with that user.
async fn connect_stackexchange(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<StackExchangeBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let (key, token) = (b.key.trim(), b.token.trim());
    if key.is_empty() || token.is_empty() || b.site.trim().is_empty() {
        return Err(AppError::BadRequest("Fill in the key, access token and site".into()));
    }
    let site = stackexchange::site_input(&b.site).ok_or_else(|| AppError::BadRequest(format!("{} isn't a Stack Exchange site", b.site.trim())))?;
    let refused = |e: Failure| AppError::BadRequest(e.message());
    let se = StackExchange::new(key, token, &site).map_err(refused)?;
    if !se.can_write().await.map_err(refused)? {
        return Err(AppError::BadRequest("The access token can't post answers. Make one with the write_access scope".into()));
    }
    let user = se.me().await.map_err(refused)?;
    let enc = crypto::encrypt(&state.cfg.app_secret, token);
    set_stackexchange(&state, id, key, &enc, &user.site, &user.account, &user.name).await
}

async fn disconnect_stackexchange(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_stackexchange(&state, id, "", "", "", "", "").await
}

async fn set_stackexchange(state: &AppState, id: Uuid, key: &str, token: &str, site: &str, account: &str, user: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set stackexchange_key = $2, stackexchange_token_enc = $3, stackexchange_site = $4, stackexchange_account = $5,
           stackexchange_user = $6, stackexchange_error = null, stackexchange_failed_at = null, stackexchange_backoff_until = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(key)
    .bind(token)
    .bind(site)
    .bind(account)
    .bind(user)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct DevBody {
    key: String,
}

/// Saves the API key once DEV accepts it, with the account's username.
async fn connect_dev(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<DevBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let key = b.key.trim();
    if key.is_empty() {
        return Err(AppError::BadRequest("Paste the API key".into()));
    }
    let username = Dev::new(key).map_err(|e| AppError::BadRequest(e.message()))?.me().await.map_err(|e| AppError::BadRequest(e.message()))?;
    set_dev(&state, id, &crypto::encrypt(&state.cfg.app_secret, key), &username).await
}

async fn disconnect_dev(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_dev(&state, id, "", "").await
}

async fn set_dev(state: &AppState, id: Uuid, key: &str, username: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set dev_key_enc = $2, dev_username = $3, dev_error = null, dev_failed_at = null where project_id = $1 returning *",
    )
    .bind(id)
    .bind(key)
    .bind(username)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn youtube_takes_comments_only_and_threads_stops_at_500_characters() {
        assert!(check("youtube", true, "", "", "Same here", 500).is_ok());
        assert!(matches!(check("youtube", false, "", "", "A lesson", 500), Err(AppError::BadRequest(m)) if m == "YouTube only takes comments"));
        assert!(check("threads", false, "", "", &"a".repeat(500), 500).is_ok());
        assert!(matches!(check("threads", true, "", "", &"a".repeat(501), 500), Err(AppError::BadRequest(m)) if m == "Replies on Threads are at most 500 characters"));
    }

    #[test]
    fn stack_exchange_takes_answers_only_and_dev_articles_need_a_title() {
        let answer = "Set the pool size to the number of cores.";
        assert!(check("stackexchange", true, "", "", answer, 500).is_ok());
        assert!(matches!(check("stackexchange", false, "", "", answer, 500), Err(AppError::BadRequest(m)) if m == "Stack Exchange only takes answers"));
        let short = check("stackexchange", true, "", "", "Use a pool.", 500);
        assert!(matches!(short, Err(AppError::BadRequest(m)) if m == "Answers on Stack Exchange are at least 30 characters"));
        assert!(check("dev", false, "", "Warming up a domain", "# Day one", 500).is_ok());
        assert!(matches!(check("dev", false, "", " ", "# Day one", 500), Err(AppError::BadRequest(m)) if m == "A DEV article needs a title"));
        assert!(matches!(check("dev", false, "", &"a".repeat(129), "Hi", 500), Err(AppError::BadRequest(m)) if m == "DEV titles are at most 128 characters"));
    }
}
