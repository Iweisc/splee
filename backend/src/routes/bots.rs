//! Connecting the Discord and Telegram bots the agent posts as, testing them, and picking the channels they read and post in,
//! and the Facebook Pages and Instagram accounts too, which routes::meta lists.
//! The posts themselves go through the Social page like every other platform's, see routes::social.

use axum::extract::{Path, State};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use super::settings::load_or_create;
use super::util_scope;
use crate::auth::Auth;
use crate::crypto;
use crate::discord::Discord;
use crate::error::{AppError, AppResult};
use crate::hooks::{self, Alert};
use crate::models::{ProjectSettings, SocialChannel};
use crate::social::{self, Accounts, Failure};
use crate::social_channels;
use crate::state::AppState;
use crate::telegram::Telegram;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/integrations/discord", put(connect_discord).delete(disconnect_discord))
        .route("/projects/{id}/integrations/discord/test", post(test_discord))
        .route("/projects/{id}/integrations/discord/channels", post(refresh_discord))
        .route("/projects/{id}/integrations/telegram", put(connect_telegram).delete(disconnect_telegram))
        .route("/projects/{id}/integrations/telegram/test", post(test_telegram))
        .route("/projects/{id}/integrations/telegram/channels", post(refresh_telegram))
        .route("/projects/{id}/social-channels", get(list))
        .route("/social-channels/{id}", patch(pick))
}

#[derive(Deserialize)]
struct TokenBody {
    token: String,
}

const NO_TEXT: &str = "Turn on Message Content Intent on the Bot page of your Discord application, so the bot can read messages. Then try again";

fn refused(e: Failure) -> AppError {
    AppError::BadRequest(e.message())
}

/// How a test went, for hooks::record.
fn outcome<T>(tested: &Result<T, Failure>) -> anyhow::Result<()> {
    match tested {
        Ok(_) => Ok(()),
        Err(Failure::Post(e) | Failure::Account(e)) => Err(anyhow::anyhow!(e.clone())),
    }
}

/// The project's accounts, when the platform's bot is connected.
async fn connected(state: &AppState, id: Uuid, platform: &str) -> AppResult<Accounts> {
    social::accounts(&state.db, id)
        .await?
        .filter(|a| !a.account(platform).1.is_empty())
        .ok_or_else(|| AppError::BadRequest(format!("Connect {} first", social::name(platform))))
}

async fn settings(state: &AppState, id: Uuid) -> AppResult<Json<ProjectSettings>> {
    Ok(Json(sqlx::query_as("select * from project_settings where project_id = $1").bind(id).fetch_one(&state.db).await?))
}

/// Saves the token once Discord accepts it and the bot can read message text, with the channels it can see.
async fn connect_discord(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<TokenBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let token = b.token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("Paste the bot token".into()));
    }
    let discord = Discord::new(&state.cfg.bots_api_url, token).map_err(refused)?;
    let bot = discord.me().await.map_err(refused)?;
    if !bot.reads_text {
        return Err(AppError::BadRequest(NO_TEXT.into()));
    }
    let found = discord.channels(&bot.id).await.map_err(refused)?;
    sqlx::query(
        "update project_settings set discord_token_enc = $2, discord_bot_id = $3, discord_bot = $4, discord_error = null, discord_failed_at = null,
           discord_held_until = null
         where project_id = $1",
    )
    .bind(id)
    .bind(crypto::encrypt(&state.cfg.app_secret, token))
    .bind(&bot.id)
    .bind(&bot.username)
    .execute(&state.db)
    .await?;
    social_channels::sync(&state.db, id, "discord", &found).await?;
    settings(&state, id).await
}

/// Drops the token. The channels and picks stay for when the bot connects again, and approved posts wait for it.
async fn disconnect_discord(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    sqlx::query(
        "update project_settings set discord_token_enc = '', discord_bot_id = '', discord_bot = '', discord_error = null, discord_failed_at = null,
           discord_held_until = null
         where project_id = $1",
    )
    .bind(id)
    .execute(&state.db)
    .await?;
    settings(&state, id).await
}

/// Signs in as the bot, and saves how it went like a post would.
async fn test_discord(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    let a = connected(&state, id, "discord").await?;
    let tested = async {
        let bot = a.discord(&state)?.me().await?;
        if !bot.reads_text {
            return Err(Failure::Account(NO_TEXT.into()));
        }
        Ok(bot)
    };
    let tested = tested.await;
    hooks::record(&state.db, id, Alert::Discord, &a.discord_token_enc, &outcome(&tested)).await;
    let bot = tested.map_err(refused)?;
    sqlx::query("update project_settings set discord_bot = $2 where project_id = $1").bind(id).bind(&bot.username).execute(&state.db).await?;
    settings(&state, id).await
}

/// Lists the bot's channels again, after it joined a server or got new permissions.
async fn refresh_discord(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<SocialChannel>>> {
    auth.project(&state.db, id).await?;
    let a = connected(&state, id, "discord").await?;
    let found = a.discord(&state).map_err(refused)?.channels(&a.discord_bot_id).await.map_err(refused)?;
    social_channels::sync(&state.db, id, "discord", &found).await?;
    Ok(Json(social_channels::list(&state.db, id).await?))
}

/// Saves the token once Telegram accepts it, with the groups and channels its waiting updates show. Fails when another
/// program reads the bot's updates, since Splee couldn't read any.
async fn connect_telegram(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<TokenBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let token = b.token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("Paste the bot token".into()));
    }
    let tg = Telegram::new(&state.cfg.bots_api_url, token).map_err(refused)?;
    let bot = tg.me().await.map_err(refused)?;
    social_channels::read_updates(&state.db, id, &tg, 0, bot.reads_all).await.map_err(refused)?;
    social_channels::refresh_telegram_rights(&state.db, id, &tg, &bot.id, bot.reads_all).await.map_err(refused)?;
    sqlx::query(
        "update project_settings set telegram_token_enc = $2, telegram_bot_id = $3, telegram_bot = $4, telegram_reads_all = $5, telegram_error = null,
           telegram_failed_at = null, telegram_held_until = null
         where project_id = $1",
    )
    .bind(id)
    .bind(crypto::encrypt(&state.cfg.app_secret, token))
    .bind(&bot.id)
    .bind(&bot.username)
    .bind(bot.reads_all)
    .execute(&state.db)
    .await?;
    settings(&state, id).await
}

/// Drops the token, and where the updates were read up to, which belongs to this bot. The groups and picks stay.
async fn disconnect_telegram(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    sqlx::query(
        "update project_settings set telegram_token_enc = '', telegram_bot_id = '', telegram_bot = '', telegram_reads_all = false, telegram_offset = 0,
           telegram_error = null, telegram_failed_at = null, telegram_held_until = null
         where project_id = $1",
    )
    .bind(id)
    .execute(&state.db)
    .await?;
    settings(&state, id).await
}

/// Signs in as the bot, saves how it went like a post would, and whether privacy mode is off now.
async fn test_telegram(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    let a = connected(&state, id, "telegram").await?;
    let tested = async { a.telegram(&state)?.me().await }.await;
    hooks::record(&state.db, id, Alert::Telegram, &a.telegram_token_enc, &outcome(&tested)).await;
    let bot = tested.map_err(refused)?;
    sqlx::query("update project_settings set telegram_bot = $2, telegram_reads_all = $3 where project_id = $1")
        .bind(id)
        .bind(&bot.username)
        .bind(bot.reads_all)
        .execute(&state.db)
        .await?;
    settings(&state, id).await
}

/// Reads the bot's waiting updates for groups it joined, and asks what it may do in each group it knows.
async fn refresh_telegram(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<SocialChannel>>> {
    auth.project(&state.db, id).await?;
    let a = connected(&state, id, "telegram").await?;
    let tg = a.telegram(&state).map_err(refused)?;
    let bot = tg.me().await.map_err(refused)?;
    sqlx::query("update project_settings set telegram_reads_all = $2 where project_id = $1").bind(id).bind(bot.reads_all).execute(&state.db).await?;
    social_channels::read_updates(&state.db, id, &tg, a.telegram_offset, bot.reads_all).await.map_err(refused)?;
    social_channels::refresh_telegram_rights(&state.db, id, &tg, &bot.id, bot.reads_all).await.map_err(refused)?;
    Ok(Json(social_channels::list(&state.db, id).await?))
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<SocialChannel>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(social_channels::list(&state.db, id).await?))
}

#[derive(Deserialize)]
struct PickBody {
    picked: bool,
}

/// Picks a channel to read and post in, or drops it. Dropping one sends its approved posts back to drafts, and reading starts
/// from its latest messages when it's picked again.
async fn pick(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<PickBody>) -> AppResult<Json<SocialChannel>> {
    let c: SocialChannel = util_scope::load(&state.db, "social_channels", id, auth.org_id).await?;
    let meta = matches!(c.platform.as_str(), "facebook" | "instagram");
    if b.picked && meta && c.token_enc.is_empty() {
        return Err(AppError::BadRequest("Facebook signed Splee out. Sign in with Facebook again first".into()));
    }
    if b.picked && meta && !c.can_read && !c.can_send {
        return Err(AppError::BadRequest(format!("Your role on {} doesn't let you post or answer comments there. Ask a Page admin for more", c.label())));
    }
    if b.picked && !c.can_read && !c.can_send {
        return Err(AppError::BadRequest(format!("The bot can't read or post in {}. Check its permissions there, then refresh the list", c.label())));
    }
    let mut tx = state.db.begin().await?;
    if !b.picked {
        sqlx::query("update social_posts set status = 'draft', updated_at = now() where channel_id = $1 and status = 'approved'").bind(id).execute(&mut *tx).await?;
    }
    let picked = sqlx::query_as::<_, SocialChannel>(
        "update social_channels set picked = $2, last_message_id = case when $2 then last_message_id else '' end where id = $1 returning *",
    )
    .bind(id)
    .bind(b.picked)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(picked))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::Bytes;
    use axum::extract::{Path, RawQuery};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::any;
    use chrono::Utc;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use super::*;
    use crate::crypto::sha256_hex;
    use crate::models::SocialPost;

    /// What the fake was asked: each request's path and body, and the Telegram updates it hands out.
    #[derive(Default)]
    struct Fake {
        requests: Vec<(String, Value)>,
        updates: Vec<Value>,
        /// Discord's 429 on the first read of #general, short enough to wait out.
        throttled: bool,
    }
    type Shared = Arc<Mutex<Fake>>;

    /// Discord's REST API and Telegram's Bot API as their docs describe them, on one server. The token picks whether the bot
    /// works, "NOTEXT" being a Discord bot without Message Content Intent.
    async fn fake_bots() -> (String, Shared) {
        async fn answer(State(fake): State<Shared>, Path(path): Path<String>, RawQuery(query): RawQuery, headers: HeaderMap, body: Bytes) -> Response {
            let body: Value = serde_json::from_slice(&body).unwrap_or_default();
            fake.lock().unwrap().requests.push((path.clone(), body.clone()));
            if let Some(rest) = path.strip_prefix("api/v10/") {
                return discord(&fake, rest, query.unwrap_or_default(), headers["authorization"].to_str().unwrap(), &body);
            }
            let (token, method) = path.trim_start_matches("bot").split_once('/').unwrap();
            telegram(&fake, token, method, &body)
        }
        fn discord(fake: &Shared, path: &str, query: String, auth: &str, body: &Value) -> Response {
            let ok = |v: Value| Json(v).into_response();
            if auth != "Bot GOOD" && auth != "Bot NOTEXT" {
                return (StatusCode::UNAUTHORIZED, Json(json!({ "message": "401: Unauthorized", "code": 0 }))).into_response();
            }
            let all = (1u64 << 10 | 1 << 11 | 1 << 16).to_string();
            let deny = |bits: u64| json!([{ "id": "100", "type": 0, "allow": "0", "deny": bits.to_string() }]);
            let now = Utc::now().to_rfc3339();
            let message = |id: &str, author: &str, bot: bool, text: &str| {
                json!({ "id": id, "type": 0, "content": text, "timestamp": now, "author": { "id": "5", "username": author, "bot": bot } })
            };
            match path {
                "users/@me" => ok(json!({ "id": "900", "username": "acme-bot", "bot": true })),
                "applications/@me" => ok(json!({ "id": "900", "flags": if auth == "Bot NOTEXT" { 0 } else { 1 << 19 } })),
                "users/@me/guilds" => ok(json!([{ "id": "100", "name": "Acme Community" }])),
                "guilds/100" => ok(json!({ "id": "100", "name": "Acme Community", "owner_id": "1", "roles": [{ "id": "100", "permissions": all }] })),
                "guilds/100/members/900" => ok(json!({ "roles": [] })),
                "guilds/100/channels" => ok(json!([
                    { "id": "201", "type": 5, "name": "announcements", "position": 1, "permission_overwrites": deny(1 << 11) },
                    { "id": "200", "type": 0, "name": "general", "position": 0, "permission_overwrites": [] },
                    { "id": "202", "type": 2, "name": "Lounge", "position": 2 },
                    { "id": "203", "type": 0, "name": "staff", "position": 3, "permission_overwrites": deny(1 << 10) },
                ])),
                "channels/200/messages" if body.is_null() => {
                    let mut f = fake.lock().unwrap();
                    if !f.throttled {
                        f.throttled = true;
                        let wait = json!({ "message": "You are being rate limited.", "retry_after": 0.05, "global": false });
                        return (StatusCode::TOO_MANY_REQUESTS, Json(wait)).into_response();
                    }
                    let after: u64 = query.split('&').find_map(|p| p.strip_prefix("after=")).map(|a| a.parse().unwrap()).unwrap_or_default();
                    let all = [
                        message("1003", "ana", false, "How do you warm up a new domain?"),
                        message("1002", "acme-bot", true, "Welcome!"),
                        message("1001", "bo", false, "Hi all"),
                    ];
                    let mut res = Json(json!(all.into_iter().filter(|m| m["id"].as_str().unwrap().parse::<u64>().unwrap() > after).collect::<Vec<_>>())).into_response();
                    // The bucket is empty now, for a moment.
                    res.headers_mut().insert("x-ratelimit-remaining", "0".parse().unwrap());
                    res.headers_mut().insert("x-ratelimit-reset-after", "0.01".parse().unwrap());
                    res
                }
                "channels/201/messages" if body.is_null() => {
                    (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "message": "You are being rate limited.", "retry_after": 120.0, "global": false }))).into_response()
                }
                "channels/200/messages" => match body["content"].as_str() {
                    Some("nope") => (StatusCode::FORBIDDEN, Json(json!({ "message": "Missing Permissions", "code": 50013 }))).into_response(),
                    _ => ok(json!({ "id": "5000", "channel_id": "200", "content": body["content"] })),
                },
                _ => (StatusCode::NOT_FOUND, Json(json!({ "message": "Unknown Channel", "code": 10003 }))).into_response(),
            }
        }
        fn telegram(fake: &Shared, token: &str, method: &str, body: &Value) -> Response {
            let ok = |v: Value| Json(json!({ "ok": true, "result": v })).into_response();
            if token != "42:GOOD" {
                return (StatusCode::UNAUTHORIZED, Json(json!({ "ok": false, "error_code": 401, "description": "Unauthorized" }))).into_response();
            }
            match method {
                "getMe" => ok(json!({ "id": 777, "is_bot": true, "username": "acme_bot", "can_read_all_group_messages": true })),
                "getUpdates" => {
                    let offset = body["offset"].as_i64().unwrap();
                    ok(json!(fake.lock().unwrap().updates.iter().filter(|u| u["update_id"].as_i64().unwrap() >= offset).cloned().collect::<Vec<_>>()))
                }
                "getChatMember" => match body["chat_id"].as_str() {
                    Some("-1005555") => ok(json!({ "status": "administrator", "can_post_messages": true, "user": { "id": 777 } })),
                    Some("-1008888") => (StatusCode::BAD_GATEWAY, "").into_response(),
                    Some("-1009999") => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error_code": 400, "description": "Bad Request: chat not found" })))
                        .into_response(),
                    _ => ok(json!({ "status": "member", "user": { "id": 777 } })),
                },
                "sendMessage" if body["text"] == "slow" => (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(json!({ "ok": false, "error_code": 429, "description": "Too Many Requests: retry after 120", "parameters": { "retry_after": 120 } })),
                )
                    .into_response(),
                "sendMessage" => ok(json!({ "message_id": 99, "chat": { "id": -1001234, "type": "supergroup", "title": "Acme Users" }, "text": body["text"] })),
                _ => (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error_code": 404, "description": "Not Found" }))).into_response(),
            }
        }
        let fake = Shared::default();
        let app = Router::new().route("/{*path}", any(answer)).with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, fake)
    }

    fn update(id: i64, kind: &str, value: Value) -> Value {
        let mut u = json!({ "update_id": id });
        u[kind] = value;
        u
    }

    /// A message someone wrote in the Acme Users supergroup.
    fn said(id: i64, text: &str) -> Value {
        json!({ "message_id": id, "date": Utc::now().timestamp(), "text": text, "chat": { "id": -1001234, "type": "supergroup", "title": "Acme Users" },
            "from": { "id": 5, "first_name": "Ana" } })
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn bots_connect_read_their_picked_channels_and_post_what_was_approved(db: PgPool) {
        let (bots, fake) = fake_bots().await;
        let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('ana@acme.test', 'Ana', 'x') returning id").fetch_one(&db).await.unwrap();
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(user).execute(&db).await.unwrap();
        sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
            .bind(sha256_hex("session-acme"))
            .bind(user)
            .bind(org)
            .execute(&db)
            .await
            .unwrap();
        let ours: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(&db).await.unwrap();
        let other: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Other') returning id").fetch_one(&db).await.unwrap();
        let theirs: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Other') returning id").bind(other).fetch_one(&db).await.unwrap();
        let their_channel: Uuid =
            sqlx::query_scalar("insert into social_channels (project_id, platform, chat_id, name, kind) values ($1, 'discord', '300', 'general', 'text') returning id")
                .bind(theirs)
                .fetch_one(&db)
                .await
                .unwrap();
        let provider = crate::mail::oauth::tests::FakeProvider::start().await;
        let cfg = crate::config::Config { bots_api_url: bots, ..crate::mail::oauth::tests::config(&provider) };
        let state = AppState { db: db.clone(), cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(1).0 };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", crate::routes::router()).with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        let client = reqwest::Client::new();
        let send = |method: reqwest::Method, path: String, body: Value| {
            client.request(method, format!("{base}{path}")).header("cookie", "splee_session=session-acme").json(&body).send()
        };
        let put = |project: Uuid, platform: &str, token: &str| send(reqwest::Method::PUT, format!("/projects/{project}/integrations/{platform}"), json!({ "token": token }));
        let text = |res: reqwest::Response| async move { (res.status(), res.text().await.unwrap()) };

        // Discord turns down a bad token, and a bot without Message Content Intent can't read what people write.
        let (status, said_no) = text(put(ours, "discord", "BAD").await.unwrap()).await;
        assert!(status == 400 && said_no.contains("didn't accept the bot token"), "{said_no}");
        assert!(text(put(ours, "discord", "NOTEXT").await.unwrap()).await.1.contains("Message Content Intent"));
        let res = put(ours, "discord", "GOOD").await.unwrap();
        assert_eq!(res.status(), 200);
        let settings: Value = res.json().await.unwrap();
        assert_eq!((settings["discord_connected"].clone(), settings["discord_bot"].clone()), (json!(true), json!("acme-bot")));
        assert!(settings.get("discord_token_enc").is_none(), "the token stays on the server");
        let saved: String = sqlx::query_scalar("select discord_token_enc from project_settings where project_id = $1").bind(ours).fetch_one(&db).await.unwrap();
        assert!(!saved.contains("GOOD") && crypto::decrypt("test-secret", &saved).unwrap() == "GOOD");

        let tested = send(reqwest::Method::POST, format!("/projects/{ours}/integrations/discord/test"), json!({})).await.unwrap();
        assert_eq!(tested.status(), 200);

        // Text and announcement channels, in their order, with what the bot may do in each.
        let channels: Vec<Value> = send(reqwest::Method::GET, format!("/projects/{ours}/social-channels"), json!({})).await.unwrap().json().await.unwrap();
        let listed: Vec<(&str, bool, bool)> = channels.iter().map(|c| (c["name"].as_str().unwrap(), c["can_read"] == true, c["can_send"] == true)).collect();
        assert_eq!(listed, [("general", true, true), ("announcements", true, false), ("staff", false, false)]);
        let id = |name: &str| channels.iter().find(|c| c["name"] == name).unwrap()["id"].as_str().unwrap().parse::<Uuid>().unwrap();
        let pick = |channel: Uuid, picked: bool| send(reqwest::Method::PATCH, format!("/social-channels/{channel}"), json!({ "picked": picked }));
        assert!(text(pick(id("staff"), true).await.unwrap()).await.1.contains("can't read or post in #staff in Acme Community"));
        assert_eq!(pick(id("general"), true).await.unwrap().status(), 200);

        // Another org's project and channel are out of reach.
        assert_eq!(put(theirs, "discord", "GOOD").await.unwrap().status(), 404);
        assert_eq!(pick(their_channel, true).await.unwrap().status(), 404);
        assert_eq!(send(reqwest::Method::GET, format!("/projects/{theirs}/social-channels"), json!({})).await.unwrap().status(), 404);

        // Polling waits out the short 429 and the empty bucket, and keeps people's messages, not the bot's.
        crate::social_channels::poll(&state).await.unwrap();
        let stored = || async {
            sqlx::query_as::<_, (String, String)>("select message_id, author from social_messages order by message_id").fetch_all(&db).await.unwrap()
        };
        assert_eq!(stored().await, [("1001".to_string(), "bo".to_string()), ("1003".to_string(), "ana".to_string())]);
        // Read from the start again, it stores each message once.
        pick(id("general"), false).await.unwrap();
        pick(id("general"), true).await.unwrap();
        crate::social_channels::poll(&state).await.unwrap();
        assert_eq!(stored().await.len(), 2);
        let after: Vec<String> = fake.lock().unwrap().requests.iter().filter(|(p, _)| p.ends_with("channels/200/messages")).map(|(p, _)| p.clone()).collect();
        assert_eq!(after.len(), 3, "the 429 was tried again once");

        // A drafted reply is approved and goes out as a reply that pings nobody, with a link to it.
        let link = crate::discord::message_link("100", "200", "1003");
        let draft: Uuid = sqlx::query_scalar(
            "insert into social_posts (project_id, platform, in_reply_to, body, channel_id) values ($1, 'discord', $2, 'Four weeks, 20 a day', $3) returning id",
        )
        .bind(ours)
        .bind(&link)
        .bind(id("general"))
        .fetch_one(&db)
        .await
        .unwrap();
        let approve = |post: Uuid| send(reqwest::Method::PATCH, format!("/social/{post}"), json!({ "status": "approved" }));
        assert_eq!(approve(draft).await.unwrap().status(), 200);
        let unpicked: Uuid = sqlx::query_scalar("insert into social_posts (project_id, platform, body, channel_id) values ($1, 'discord', 'Hi', $2) returning id")
            .bind(ours)
            .bind(id("announcements"))
            .fetch_one(&db)
            .await
            .unwrap();
        assert!(text(approve(unpicked).await.unwrap()).await.1.contains("Pick #announcements in Acme Community"));
        crate::social::tick(&state).await.unwrap();
        let posted: SocialPost = sqlx::query_as("select * from social_posts where id = $1").bind(draft).fetch_one(&db).await.unwrap();
        assert_eq!((posted.status.as_str(), posted.url.as_str(), posted.account.as_str()), ("posted", "https://discord.com/channels/100/200/5000", "900"));
        let sent = fake.lock().unwrap().requests.iter().rev().find(|(p, b)| p.ends_with("channels/200/messages") && !b.is_null()).unwrap().1.clone();
        assert_eq!(sent["message_reference"], json!({ "message_id": "1003", "channel_id": "200", "fail_if_not_exists": true }));
        assert_eq!((sent["content"].clone(), sent["allowed_mentions"].clone()), (json!("Four weeks, 20 a day"), json!({ "parse": [] })));
        assert_eq!(sent["nonce"].as_str().unwrap().len(), 25);

        // A long 429 holds the bot, and says so on the integrations page.
        sqlx::query("update social_channels set picked = true where id = $1").bind(id("announcements")).execute(&db).await.unwrap();
        crate::social_channels::poll(&state).await.unwrap();
        let (held, error): (bool, Option<String>) =
            sqlx::query_as("select discord_held_until > now() + interval '100 seconds', discord_error from project_settings where project_id = $1")
                .bind(ours)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(held && error.is_some_and(|e| e.contains("rate limiting the bot for 120 seconds")));
        assert!(crate::social::accounts(&db, ours).await.unwrap().unwrap().connected().contains(&"discord"));

        // Telegram learns its groups and channels from the updates waiting for it.
        fake.lock().unwrap().updates = vec![
            update(1, "my_chat_member", json!({ "chat": { "id": -1001234, "type": "supergroup", "title": "Acme Users" }, "new_chat_member": { "status": "member" } })),
            update(2, "message", said(10, "Before Splee was told about this group")),
            update(3, "my_chat_member", json!({ "chat": { "id": -1005555, "type": "channel", "title": "Acme News", "username": "acmenews" },
                "new_chat_member": { "status": "administrator", "can_post_messages": true } })),
            update(4, "message", json!({ "message_id": 12, "date": 1, "text": "I'm a bot", "chat": { "id": -1001234, "type": "supergroup", "title": "Acme Users" },
                "from": { "id": 8, "is_bot": true, "first_name": "B" } })),
        ];
        assert!(text(put(ours, "telegram", "42:BAD").await.unwrap()).await.1.contains("didn't accept the bot token"));
        let settings: Value = put(ours, "telegram", "42:GOOD").await.unwrap().json().await.unwrap();
        assert_eq!((settings["telegram_bot"].clone(), settings["telegram_reads_all"].clone()), (json!("acme_bot"), json!(true)));
        let saved: (String, i64) =
            sqlx::query_as("select telegram_token_enc, telegram_offset from project_settings where project_id = $1").bind(ours).fetch_one(&db).await.unwrap();
        assert_eq!((crypto::decrypt("test-secret", &saved.0).unwrap().as_str(), saved.1), ("42:GOOD", 5));
        let chats: Vec<SocialChannel> = sqlx::query_as("select * from social_channels where platform = 'telegram' order by name").fetch_all(&db).await.unwrap();
        let listed: Vec<(&str, &str, bool, bool)> = chats.iter().map(|c| (c.name.as_str(), c.kind.as_str(), c.can_read, c.can_send)).collect();
        assert_eq!(listed, [("Acme News", "channel", false, true), ("Acme Users", "supergroup", true, true)]);
        let users = chats.iter().find(|c| c.name == "Acme Users").unwrap().id;
        pick(users, true).await.unwrap();
        fake.lock().unwrap().updates.push(update(5, "message", said(11, "Anyone tried warming up a domain?")));
        crate::social_channels::poll(&state).await.unwrap();
        let kept: Vec<(String, String)> = sqlx::query_as("select message_id, body from social_messages where channel_id = $1").bind(users).fetch_all(&db).await.unwrap();
        assert_eq!(kept, [("11".to_string(), "Anyone tried warming up a domain?".to_string())]);

        // A reply goes out with reply_parameters, and a long 429 puts the post back and holds the bot.
        let reply: Uuid = sqlx::query_scalar(
            "insert into social_posts (project_id, platform, in_reply_to, body, channel_id, status) values ($1, 'telegram', $2, 'Yes, four weeks', $3, 'approved')
             returning id",
        )
        .bind(ours)
        .bind(crate::telegram::message_link("-1001234", "11"))
        .bind(users)
        .fetch_one(&db)
        .await
        .unwrap();
        crate::social::tick(&state).await.unwrap();
        let posted: SocialPost = sqlx::query_as("select * from social_posts where id = $1").bind(reply).fetch_one(&db).await.unwrap();
        assert_eq!((posted.status.as_str(), posted.url.as_str()), ("posted", "https://t.me/c/1234/99"));
        let sent = fake.lock().unwrap().requests.iter().rev().find(|(p, _)| p.ends_with("sendMessage")).unwrap().1.clone();
        assert_eq!(sent, json!({ "chat_id": "-1001234", "text": "Yes, four weeks", "reply_parameters": { "message_id": 11 } }));
        let news = chats.iter().find(|c| c.name == "Acme News").unwrap().id;
        pick(news, true).await.unwrap();
        let slow = "insert into social_posts (project_id, platform, body, channel_id, status) values ($1, 'telegram', 'slow', $2, 'approved') returning id";
        let slow: Uuid = sqlx::query_scalar(slow)
            .bind(ours)
            .bind(news)
            .fetch_one(&db)
            .await
            .unwrap();
        crate::social::tick(&state).await.unwrap();
        let waiting: String = sqlx::query_scalar("select status from social_posts where id = $1").bind(slow).fetch_one(&db).await.unwrap();
        let held: bool = sqlx::query_scalar("select telegram_held_until > now() from project_settings where project_id = $1").bind(ours).fetch_one(&db).await.unwrap();
        assert_eq!((waiting.as_str(), held), ("approved", true));

        // Disconnecting drops the token and keeps the picks for when it's back.
        let settings: Value = send(reqwest::Method::DELETE, format!("/projects/{ours}/integrations/telegram"), json!({})).await.unwrap().json().await.unwrap();
        assert_eq!(settings["telegram_connected"], false);
        let token: String = sqlx::query_scalar("select telegram_token_enc from project_settings where project_id = $1").bind(ours).fetch_one(&db).await.unwrap();
        let picked: bool = sqlx::query_scalar("select picked from social_channels where id = $1").bind(users).fetch_one(&db).await.unwrap();
        assert_eq!((token.as_str(), picked), ("", true));

        // A chat the reconnected bot isn't in, like a previous bot's, goes even when picked, and its approved post is a draft again.
        let stranger: Uuid = sqlx::query_scalar(
            "insert into social_channels (project_id, platform, chat_id, name, kind, picked) values ($1, 'telegram', '-1009999', 'Old', 'supergroup', true) returning id",
        )
        .bind(ours)
        .fetch_one(&db)
        .await
        .unwrap();
        let orphan: Uuid = sqlx::query_scalar("insert into social_posts (project_id, platform, body, channel_id, status) values ($1, 'telegram', 'Hi', $2, 'approved') returning id")
            .bind(ours)
            .bind(stranger)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(put(ours, "telegram", "42:GOOD").await.unwrap().status(), 200);
        let left: Vec<String> = sqlx::query_scalar("select name from social_channels where platform = 'telegram' and picked order by name").fetch_all(&db).await.unwrap();
        let status: String = sqlx::query_scalar("select status from social_posts where id = $1").bind(orphan).fetch_one(&db).await.unwrap();
        assert_eq!((left, status.as_str()), (vec!["Acme News".to_string(), "Acme Users".into()], "draft"));

        // A Telegram outage fails the refresh and leaves the chat alone.
        let flaky: Uuid = sqlx::query_scalar(
            "insert into social_channels (project_id, platform, chat_id, name, kind, picked) values ($1, 'telegram', '-1008888', 'Flaky', 'supergroup', true) returning id",
        )
        .bind(ours)
        .fetch_one(&db)
        .await
        .unwrap();
        let refreshed = send(reqwest::Method::POST, format!("/projects/{ours}/integrations/telegram/channels"), json!({})).await.unwrap();
        assert!(refreshed.status().is_client_error() || refreshed.status().is_server_error());
        let kept: bool = sqlx::query_scalar("select picked from social_channels where id = $1").bind(flaky).fetch_one(&db).await.unwrap();
        assert!(kept);
    }
}
