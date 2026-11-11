//! The Discord channels and Telegram groups and channels a project's bot can see, which the user picks to read and post in,
//! and the recent messages in the picked ones, which social runs draft replies to. `poll` reads new messages on the social
//! poster's cadence.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::discord::Discord;
use crate::hooks::{self, Alert};
use crate::models::SocialChannel;
use crate::social::{self, Accounts, Failure};
use crate::state::AppState;
use crate::telegram::{self, Telegram};

/// Messages older than this are past what a social run answers, see agent::social.
const KEEP_DAYS: i32 = 30;
/// Pages of Telegram updates read a tick. The rest wait for the next one.
const MAX_PAGES: usize = 5;

/// A channel as the platform shows it.
pub struct Found {
    pub chat_id: String,
    pub guild_id: String,
    pub guild_name: String,
    pub name: String,
    pub kind: String,
    pub username: String,
    pub can_read: bool,
    pub can_send: bool,
}

/// A message someone wrote in a channel.
pub struct Message {
    pub id: String,
    pub author: String,
    pub body: String,
    pub sent_at: DateTime<Utc>,
}

pub async fn list(db: &PgPool, project_id: Uuid) -> sqlx::Result<Vec<SocialChannel>> {
    sqlx::query_as("select * from social_channels where project_id = $1 order by platform, guild_name, created_at, name").bind(project_id).fetch_all(db).await
}

/// Adds the channel or updates what the platform says about it, keeping whether it's picked.
pub async fn upsert(db: &PgPool, project_id: Uuid, platform: &str, f: &Found) -> sqlx::Result<SocialChannel> {
    sqlx::query_as(
        "insert into social_channels (project_id, platform, chat_id, guild_id, guild_name, name, kind, username, can_read, can_send)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         on conflict (project_id, platform, chat_id) do update set guild_id = excluded.guild_id, guild_name = excluded.guild_name, name = excluded.name,
           kind = excluded.kind, username = excluded.username, can_read = excluded.can_read, can_send = excluded.can_send
         returning *",
    )
    .bind(project_id)
    .bind(platform)
    .bind(&f.chat_id)
    .bind(&f.guild_id)
    .bind(&f.guild_name)
    .bind(&f.name)
    .bind(&f.kind)
    .bind(&f.username)
    .bind(f.can_read)
    .bind(f.can_send)
    .fetch_one(db)
    .await
}

/// Saves the channels a Discord bot sees now. The ones it doesn't see anymore go, which leaves their posts without a channel.
pub async fn sync(db: &PgPool, project_id: Uuid, platform: &str, found: &[Found]) -> sqlx::Result<()> {
    for f in found {
        upsert(db, project_id, platform, f).await?;
    }
    let ids: Vec<&str> = found.iter().map(|f| f.chat_id.as_str()).collect();
    sqlx::query("delete from social_channels where project_id = $1 and platform = $2 and chat_id <> all($3)").bind(project_id).bind(platform).bind(&ids).execute(db).await?;
    Ok(())
}

/// Stores the messages a channel doesn't have yet and returns how many were new.
pub async fn store(db: &PgPool, channel_id: Uuid, messages: &[Message]) -> sqlx::Result<u64> {
    let mut added = 0;
    for m in messages {
        let saved = sqlx::query(
            "insert into social_messages (channel_id, message_id, author, body, sent_at) values ($1, $2, $3, $4, $5) on conflict do nothing",
        )
        .bind(channel_id)
        .bind(&m.id)
        .bind(&m.author)
        .bind(&m.body)
        .bind(m.sent_at)
        .execute(db)
        .await?;
        added += saved.rows_affected();
    }
    Ok(added)
}

impl Accounts {
    pub fn discord(&self, state: &AppState) -> Result<Discord, Failure> {
        let token = crate::crypto::decrypt(&state.cfg.app_secret, &self.discord_token_enc)
            .map_err(|_| Failure::Account("Couldn't read the saved Discord bot token. Connect Discord again".into()))?;
        Discord::new(&state.cfg.bots_api_url, &token)
    }

    pub fn telegram(&self, state: &AppState) -> Result<Telegram, Failure> {
        let token = crate::crypto::decrypt(&state.cfg.app_secret, &self.telegram_token_enc)
            .map_err(|_| Failure::Account("Couldn't read the saved Telegram bot token. Connect Telegram again".into()))?;
        Telegram::new(&state.cfg.bots_api_url, &token)
    }
}

/// Reads the new messages in each project's picked channels. Only runs where posting does, since getUpdates hands each
/// Telegram update out once, and a second backend on the same database would take them from the one that posts.
pub async fn poll(state: &AppState) -> anyhow::Result<()> {
    sqlx::query("delete from social_messages where sent_at < now() - make_interval(days => $1)").bind(KEEP_DAYS).execute(&state.db).await?;
    let connected = sqlx::query_as::<_, Accounts>(&format!(
        "{} join projects p on p.id = s.project_id where p.org_id is not null and (s.discord_token_enc <> '' or s.telegram_token_enc <> '')",
        social::ACCOUNTS
    ))
    .fetch_all(&state.db)
    .await?;
    for a in connected {
        if !a.discord_token_enc.is_empty() && !held(&state.db, a.project_id, "discord").await? {
            let read = poll_discord(state, &a).await;
            settle(state, &a, "discord", read).await;
        }
        if !a.telegram_token_enc.is_empty() && !held(&state.db, a.project_id, "telegram").await? {
            let read = poll_telegram(state, &a).await;
            settle(state, &a, "telegram", read).await;
        }
    }
    Ok(())
}

/// Whether the platform asked the bot to wait and that wait isn't over. `platform` is "discord" or "telegram", never user input.
async fn held(db: &PgPool, project_id: Uuid, platform: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar(&format!("select coalesce({platform}_held_until > now(), false) from project_settings where project_id = $1")).bind(project_id).fetch_one(db).await
}

/// Saves how reading went, on the integrations page, and the wait a long 429 asked for.
async fn settle(state: &AppState, a: &Accounts, platform: &'static str, read: (Result<(), Failure>, Option<f64>)) {
    let (result, wait) = read;
    if let Some(wait) = wait {
        hold(&state.db, a.project_id, platform, wait).await;
    }
    let (alert, enc) = if platform == "discord" { (Alert::Discord, &a.discord_token_enc) } else { (Alert::Telegram, &a.telegram_token_enc) };
    match result {
        Ok(()) => hooks::record(&state.db, a.project_id, alert, enc, &Ok(())).await,
        // A channel's own problem, like a missing permission, is on that channel. See poll_discord.
        Err(Failure::Post(e)) => tracing::warn!(project_id = %a.project_id, platform, error = %e, "couldn't read the channels"),
        Err(Failure::Account(e)) => hooks::record(&state.db, a.project_id, alert, enc, &Err(anyhow::anyhow!(e))).await,
    }
}

/// Holds the bot's posting and reading for the wait the platform asked for. `platform` is "discord" or "telegram".
pub async fn hold(db: &PgPool, project_id: Uuid, platform: &str, wait: f64) {
    let held = sqlx::query(&format!("update project_settings set {platform}_held_until = now() + make_interval(secs => $2) where project_id = $1"))
        .bind(project_id)
        .bind(wait)
        .execute(db)
        .await;
    if let Err(e) = held {
        tracing::warn!(error = %e, "couldn't save the bot's wait");
    }
}

/// Reads each picked channel's messages since the newest one stored. A channel the bot can't read anymore is marked so on
/// the integrations page, and the others still get read.
async fn poll_discord(state: &AppState, a: &Accounts) -> (Result<(), Failure>, Option<f64>) {
    let discord = match a.discord(state) {
        Ok(d) => d,
        Err(e) => return (Err(e), None),
    };
    let read = async {
        let picked: Vec<SocialChannel> = sqlx::query_as("select * from social_channels where project_id = $1 and platform = 'discord' and picked and can_read")
            .bind(a.project_id)
            .fetch_all(&state.db)
            .await
            .map_err(|e| Failure::Post(e.to_string()))?;
        for c in picked {
            match discord.messages(&c.chat_id, &c.last_message_id).await {
                Ok(messages) => {
                    let db = |e: sqlx::Error| Failure::Post(e.to_string());
                    store(&state.db, c.id, &messages).await.map_err(db)?;
                    if let Some(newest) = messages.last() {
                        sqlx::query("update social_channels set last_message_id = $2 where id = $1").bind(c.id).bind(&newest.id).execute(&state.db).await.map_err(db)?;
                    }
                }
                Err(Failure::Post(e)) => {
                    tracing::warn!(channel = %c.id, error = %e, "couldn't read a Discord channel");
                    sqlx::query("update social_channels set can_read = false where id = $1").bind(c.id).execute(&state.db).await.map_err(|e| Failure::Post(e.to_string()))?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    };
    let result = read.await;
    (result, discord.retry_after())
}

/// Reads the bot's new updates: the chats it joined or left, and messages in the picked ones. The rest are dropped, since
/// Telegram only hands each update out once.
async fn poll_telegram(state: &AppState, a: &Accounts) -> (Result<(), Failure>, Option<f64>) {
    let tg = match a.telegram(state) {
        Ok(t) => t,
        Err(e) => return (Err(e), None),
    };
    let result = read_updates(&state.db, a.project_id, &tg, a.telegram_offset, a.telegram_reads_all).await;
    (result, tg.retry_after())
}

pub async fn read_updates(db: &PgPool, project_id: Uuid, tg: &Telegram, mut offset: i64, reads_all: bool) -> Result<(), Failure> {
    let failed = |e: sqlx::Error| Failure::Post(e.to_string());
    for _ in 0..MAX_PAGES {
        let updates = tg.updates(offset).await?;
        for u in &updates {
            apply(db, project_id, u, reads_all).await.map_err(failed)?;
            offset = offset.max(u["update_id"].as_i64().unwrap_or_default() + 1);
        }
        sqlx::query("update project_settings set telegram_offset = $2 where project_id = $1").bind(project_id).bind(offset).execute(db).await.map_err(failed)?;
        if updates.len() < telegram::PAGE {
            break;
        }
    }
    Ok(())
}

async fn apply(db: &PgPool, project_id: Uuid, u: &Value, reads_all: bool) -> sqlx::Result<()> {
    if let Some(found) = telegram::chat(&u["my_chat_member"]["chat"]) {
        let member = &u["my_chat_member"]["new_chat_member"];
        if matches!(member["status"].as_str(), Some("left" | "kicked")) {
            let left = "delete from social_channels where project_id = $1 and platform = 'telegram' and chat_id = $2";
            sqlx::query(left).bind(project_id).bind(&found.chat_id).execute(db).await?;
            return Ok(());
        }
        let (can_read, can_send) = telegram::rights(&found.kind, member, reads_all);
        upsert(db, project_id, "telegram", &Found { can_read, can_send, ..found }).await?;
        return Ok(());
    }
    let m = if u["message"].is_object() { &u["message"] } else { &u["channel_post"] };
    let Some(seen) = telegram::chat(&m["chat"]) else { return Ok(()) };
    // A basic group that became a supergroup gets a new chat id, and its last message points at it.
    if let Some(to) = m["migrate_to_chat_id"].as_i64() {
        sqlx::query(
            "update social_channels set chat_id = $3, kind = 'supergroup' where project_id = $1 and platform = 'telegram' and chat_id = $2
               and not exists (select 1 from social_channels o where o.project_id = $1 and o.platform = 'telegram' and o.chat_id = $3)",
        )
        .bind(project_id)
        .bind(&seen.chat_id)
        .bind(to.to_string())
        .execute(db)
        .await?;
        return Ok(());
    }
    // A chat the bot only learns of from a message keeps the rights it had, and starts with what privacy mode allows.
    let channel: SocialChannel = sqlx::query_as(
        "insert into social_channels (project_id, platform, chat_id, name, kind, username, can_read, can_send) values ($1, 'telegram', $2, $3, $4, $5, $6, true)
         on conflict (project_id, platform, chat_id) do update set name = excluded.name, username = excluded.username
         returning *",
    )
    .bind(project_id)
    .bind(&seen.chat_id)
    .bind(&seen.name)
    .bind(&seen.kind)
    .bind(&seen.username)
    .bind(seen.kind != "channel" && reads_all)
    .fetch_one(db)
    .await?;
    if let (true, Some(message)) = (channel.picked && channel.kind != "channel", telegram::message(m)) {
        store(db, channel.id, &[message]).await?;
    }
    Ok(())
}

/// Asks Telegram what the bot may do in each chat it knows, since a message alone doesn't say. A chat it left goes.
pub async fn refresh_telegram_rights(db: &PgPool, project_id: Uuid, tg: &Telegram, bot_id: &str, reads_all: bool) -> Result<(), Failure> {
    let failed = |e: sqlx::Error| Failure::Post(e.to_string());
    let known: Vec<SocialChannel> =
        sqlx::query_as("select * from social_channels where project_id = $1 and platform = 'telegram'").bind(project_id).fetch_all(db).await.map_err(failed)?;
    let mut gone = Vec::new();
    for c in known {
        let (can_read, can_send) = match tg.member(&c.chat_id, bot_id).await? {
            Some(member) => telegram::rights(&c.kind, &member, reads_all),
            None => (false, false),
        };
        if !can_read && !can_send {
            gone.push(c.id);
        }
        sqlx::query("update social_channels set can_read = $2, can_send = $3 where id = $1").bind(c.id).bind(can_read).bind(can_send).execute(db).await.map_err(failed)?;
    }
    // Like a bot that left, which apply() drops even when picked. This also clears a previous bot's chats on reconnect.
    sqlx::query("update social_posts set status = 'draft', updated_at = now() where channel_id = any($1) and status = 'approved'")
        .bind(&gone)
        .execute(db)
        .await
        .map_err(failed)?;
    sqlx::query("delete from social_channels where id = any($1)").bind(&gone).execute(db).await.map_err(failed)?;
    Ok(())
}
