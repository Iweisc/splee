//! Discord through its REST API (v10) as the project's own bot, with the token from the Bot page of the user's Discord
//! application. No gateway: social_channels polls the picked channels' messages on the social poster's cadence.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::{Method, StatusCode, Url};
use serde_json::{Value, json};

use crate::social::Failure;
use crate::social_channels::{Found, Message};

const API: &str = "https://discord.com/api/v10";
pub const CHARS: usize = 2000;
/// A wait Discord asks for that's longer than this holds the bot's posting instead of holding up the tick.
const MAX_WAIT: Duration = Duration::from_secs(5);
/// More servers than a company's bot is in.
const MAX_GUILDS: usize = 25;

const ADMINISTRATOR: u64 = 1 << 3;
const VIEW_CHANNEL: u64 = 1 << 10;
const SEND_MESSAGES: u64 = 1 << 11;
const READ_MESSAGE_HISTORY: u64 = 1 << 16;
/// The application flags that say the bot gets message text, for a verified bot and for one in under 100 servers.
const MESSAGE_CONTENT: u64 = (1 << 18) | (1 << 19);

/// The link Discord shows for a message, which opens it in the app.
pub fn message_link(guild: &str, channel: &str, message: &str) -> String {
    format!("https://discord.com/channels/{guild}/{channel}/{message}")
}

/// The server, channel and message ids in a message link from discord.com, its ptb. and canary. hosts or discordapp.com.
pub fn parse_link(link: &str) -> Option<(String, String, String)> {
    let url = Url::parse(link.trim()).ok()?;
    let host = url.host_str()?.trim_start_matches("www.");
    if url.scheme() != "https" || !matches!(host, "discord.com" | "ptb.discord.com" | "canary.discord.com" | "discordapp.com") {
        return None;
    }
    let ids: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    match ids.as_slice() {
        ["channels", g, c, m] if [g, c, m].iter().all(|id| is_id(id)) => Some((g.to_string(), c.to_string(), m.to_string())),
        _ => None,
    }
}

/// Discord's ids are snowflakes, up to 20 digits.
fn is_id(id: &str) -> bool {
    (1..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

/// The bot a token belongs to.
pub struct Bot {
    pub id: String,
    pub username: String,
    /// Whether Message Content Intent is on, without which Discord sends messages with their text left out.
    pub reads_text: bool,
}

pub struct Discord {
    client: reqwest::Client,
    api: String,
    token: String,
    /// When the bucket the last call used refills, once Discord said it's empty.
    refill: Mutex<Option<Instant>>,
    /// The wait from the last 429 that was too long to sit out, for the caller to hold posting with.
    retry_after: Mutex<Option<f64>>,
}

impl Discord {
    /// `base` is Config::bots_api_url: empty for Discord itself.
    pub fn new(base: &str, token: &str) -> Result<Self, Failure> {
        let api = if base.is_empty() { API.to_string() } else { format!("{base}/api/v10") };
        let client = reqwest::Client::builder().user_agent("DiscordBot (Splee, 1.0)").timeout(Duration::from_secs(20)).build();
        let client = client.map_err(|e| Failure::Account(e.to_string()))?;
        Ok(Self { client, api, token: token.trim().trim_start_matches("Bot ").into(), refill: Mutex::new(None), retry_after: Mutex::new(None) })
    }

    pub fn retry_after(&self) -> Option<f64> {
        *self.retry_after.lock().unwrap()
    }

    /// Waits out an empty rate limit bucket and short 429s. A bad token is about the account, and so is a long 429. A missing
    /// permission, a deleted channel or message and anything that may have gone through are about the post.
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value, Failure> {
        for attempt in 0..2 {
            let refill = self.refill.lock().unwrap().take();
            if let Some(wait) = refill.map(|at| at.saturating_duration_since(Instant::now())) {
                tokio::time::sleep(wait.min(MAX_WAIT)).await;
            }
            let mut request = self.client.request(method.clone(), format!("{}{path}", self.api)).header("Authorization", format!("Bot {}", self.token));
            if let Some(body) = &body {
                request = request.json(body);
            }
            let res = request.send().await.map_err(|e| Failure::Post(format!("Discord didn't answer: {}", e.without_url())))?;
            let status = res.status();
            let header = |name: &str| res.headers().get(name).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<f64>().ok());
            if header("x-ratelimit-remaining") == Some(0.0) {
                let reset = header("x-ratelimit-reset-after").unwrap_or(1.0);
                *self.refill.lock().unwrap() = Some(Instant::now() + Duration::from_secs_f64(reset.clamp(0.0, 60.0)));
            }
            let header_wait = header("retry-after");
            let body: Value = res.json().await.unwrap_or_default();
            let said = body["message"].as_str().unwrap_or_default();
            let code = body["code"].as_i64().unwrap_or_default();
            if status == StatusCode::TOO_MANY_REQUESTS {
                let wait = body["retry_after"].as_f64().or(header_wait).unwrap_or(60.0).max(0.0);
                if attempt == 0 && wait <= MAX_WAIT.as_secs_f64() {
                    tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                    continue;
                }
                *self.retry_after.lock().unwrap() = Some(wait);
                return Err(Failure::Account(format!("Discord is rate limiting the bot for {} seconds", wait.ceil() as i64)));
            }
            return Err(match (status, code) {
                (s, _) if s.is_success() => return Ok(body),
                (StatusCode::UNAUTHORIZED, _) => {
                    Failure::Account("Discord didn't accept the bot token. Reset it on the Bot page of your application and connect Discord again".into())
                }
                // 50001 is Missing Access and 50013 Missing Permissions.
                (StatusCode::FORBIDDEN, 50001 | 50013) => Failure::Post(
                    "The bot can't do that in this channel. Give it View Channel, Send Messages and Read Message History there".into(),
                ),
                // 10003 is Unknown Channel and 10008 Unknown Message.
                (StatusCode::NOT_FOUND, 10003 | 10008) => Failure::Post("Discord couldn't find it. It may have been deleted".into()),
                (s, _) if s.is_server_error() => Failure::Post(format!("Discord answered {s}")),
                (s, _) if said.is_empty() => Failure::Post(format!("Discord answered {s}")),
                (_, _) => Failure::Post(format!("Discord said: {said}")),
            });
        }
        unreachable!("the second attempt always returns")
    }

    async fn get(&self, path: &str) -> Result<Value, Failure> {
        self.call(Method::GET, path, None).await
    }

    pub async fn me(&self) -> Result<Bot, Failure> {
        let user = self.get("/users/@me").await?;
        let id = user["id"].as_str().filter(|id| is_id(id)).ok_or_else(|| Failure::Account("Discord didn't say which bot this is".into()))?;
        let app = self.get("/applications/@me").await?;
        let flags = app["flags"].as_u64().unwrap_or_default();
        Ok(Bot { id: id.into(), username: user["username"].as_str().unwrap_or_default().into(), reads_text: flags & MESSAGE_CONTENT != 0 })
    }

    /// The text and announcement channels in each server the bot is in, with whether it can read and post in each.
    pub async fn channels(&self, bot_id: &str) -> Result<Vec<Found>, Failure> {
        let guilds = self.get(&format!("/users/@me/guilds?limit={MAX_GUILDS}")).await?;
        let mut found = Vec::new();
        for g in guilds.as_array().into_iter().flatten() {
            let Some(guild_id) = g["id"].as_str() else { continue };
            let guild = self.get(&format!("/guilds/{guild_id}")).await?;
            let member = self.get(&format!("/guilds/{guild_id}/members/{bot_id}")).await?;
            let roles: Vec<&str> = member["roles"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let mut channels = self.get(&format!("/guilds/{guild_id}/channels")).await?.as_array().cloned().unwrap_or_default();
            channels.sort_by_key(|c| c["position"].as_i64().unwrap_or_default());
            for c in channels {
                let kind = match c["type"].as_i64() {
                    Some(0) => "text",
                    Some(5) => "announcement",
                    _ => continue,
                };
                let can = permissions(&guild, &roles, bot_id, &c);
                found.push(Found {
                    chat_id: c["id"].as_str().unwrap_or_default().into(),
                    guild_id: guild_id.into(),
                    guild_name: guild["name"].as_str().unwrap_or_default().into(),
                    name: c["name"].as_str().unwrap_or_default().into(),
                    kind: kind.into(),
                    username: String::new(),
                    can_read: can & (VIEW_CHANNEL | READ_MESSAGE_HISTORY) == VIEW_CHANNEL | READ_MESSAGE_HISTORY,
                    can_send: can & (VIEW_CHANNEL | SEND_MESSAGES) == VIEW_CHANNEL | SEND_MESSAGES,
                });
            }
        }
        Ok(found)
    }

    /// Up to 50 of the channel's messages after `after`, or its latest 50, oldest first, leaving out bots and system messages.
    pub async fn messages(&self, channel: &str, after: &str) -> Result<Vec<Message>, Failure> {
        let after = if after.is_empty() { String::new() } else { format!("&after={after}") };
        let found = self.get(&format!("/channels/{channel}/messages?limit=50{after}")).await?;
        let mut out: Vec<Message> = found
            .as_array()
            .into_iter()
            .flatten()
            // 0 is a message and 19 a reply.
            .filter(|m| matches!(m["type"].as_i64(), Some(0 | 19)) && m["author"]["bot"] != true)
            .filter_map(|m| {
                let author = m["author"]["global_name"].as_str().or(m["author"]["username"].as_str()).unwrap_or_default();
                let sent_at = m["timestamp"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok())?.with_timezone(&Utc);
                Some(Message { id: m["id"].as_str()?.into(), author: author.into(), body: m["content"].as_str().unwrap_or_default().into(), sent_at })
            })
            .collect();
        out.sort_by_key(|m| (m.id.len(), m.id.clone()));
        Ok(out)
    }

    /// Posts in the channel, as a reply to `reply_to` when there is one, and returns the new message's link. Mentions in the
    /// text stay text, so a post can't ping @everyone. `nonce` makes Discord drop a second send of the same post.
    pub async fn send(&self, guild: &str, channel: &str, text: &str, reply_to: Option<&str>, nonce: &str) -> Result<String, Failure> {
        let mut body = json!({ "content": text, "allowed_mentions": { "parse": [] }, "nonce": nonce, "enforce_nonce": true });
        if let Some(message) = reply_to {
            body["message_reference"] = json!({ "message_id": message, "channel_id": channel, "fail_if_not_exists": true });
        }
        let sent = self.call(Method::POST, &format!("/channels/{channel}/messages"), Some(body)).await?;
        // It's out now, so not getting its id back doesn't fail it.
        Ok(sent["id"].as_str().map(|id| message_link(guild, channel, id)).unwrap_or_default())
    }
}

/// What the bot may do in the channel: its roles' permissions, then the channel's overwrites for @everyone, its roles and
/// itself, in Discord's order. The owner and administrators may do anything.
fn permissions(guild: &Value, member_roles: &[&str], bot_id: &str, channel: &Value) -> u64 {
    let guild_id = guild["id"].as_str().unwrap_or_default();
    if guild["owner_id"].as_str() == Some(bot_id) {
        return u64::MAX;
    }
    let bits = |v: &Value| v.as_str().and_then(|s| s.parse::<u64>().ok()).unwrap_or_default();
    let mut can = 0;
    for role in guild["roles"].as_array().into_iter().flatten() {
        let id = role["id"].as_str().unwrap_or_default();
        if id == guild_id || member_roles.contains(&id) {
            can |= bits(&role["permissions"]);
        }
    }
    if can & ADMINISTRATOR != 0 {
        return u64::MAX;
    }
    let overwrites: Vec<&Value> = channel["permission_overwrites"].as_array().into_iter().flatten().collect();
    let apply = |can: u64, which: &dyn Fn(&Value) -> bool| {
        let (allow, deny) = overwrites.iter().filter(|o| which(o)).fold((0, 0), |(a, d), o| (a | bits(&o["allow"]), d | bits(&o["deny"])));
        (can & !deny) | allow
    };
    can = apply(can, &|o| o["id"] == guild_id);
    can = apply(can, &|o| o["type"] == 0 && o["id"] != guild_id && member_roles.contains(&o["id"].as_str().unwrap_or_default()));
    apply(can, &|o| o["type"] == 1 && o["id"] == bot_id)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn message_links_get_one_form() {
        let ids = ("81384788765712384".to_string(), "81384788765712385".to_string(), "1213475887111111111".to_string());
        assert_eq!(parse_link("https://canary.discord.com/channels/81384788765712384/81384788765712385/1213475887111111111"), Some(ids.clone()));
        assert_eq!(parse_link(&message_link(&ids.0, &ids.1, &ids.2)), Some(ids));
        assert_eq!(parse_link("https://discord.com/channels/@me/81384788765712385/1213475887111111111"), None);
        assert_eq!(parse_link("https://discord.gg/acme"), None);
        assert_eq!(parse_link("https://evil.example/channels/1/2/3"), None);
    }

    #[test]
    fn permissions_follow_roles_then_overwrites() {
        let everyone = (VIEW_CHANNEL | SEND_MESSAGES | READ_MESSAGE_HISTORY).to_string();
        let guild = json!({ "id": "1", "owner_id": "9", "roles": [{ "id": "1", "permissions": everyone }, { "id": "5", "permissions": "0" }] });
        let open = json!({ "permission_overwrites": [] });
        assert_eq!(permissions(&guild, &[], "7", &open) & SEND_MESSAGES, SEND_MESSAGES);
        // @everyone can't post, the bot's role can.
        let announce = json!({ "permission_overwrites": [
            { "id": "1", "type": 0, "allow": "0", "deny": SEND_MESSAGES.to_string() },
            { "id": "5", "type": 0, "allow": SEND_MESSAGES.to_string(), "deny": "0" },
        ] });
        assert_eq!(permissions(&guild, &[], "7", &announce) & SEND_MESSAGES, 0);
        assert_eq!(permissions(&guild, &["5"], "7", &announce) & SEND_MESSAGES, SEND_MESSAGES);
        // An overwrite for the bot itself comes last.
        let muted = json!({ "permission_overwrites": [{ "id": "7", "type": 1, "allow": "0", "deny": VIEW_CHANNEL.to_string() }] });
        assert_eq!(permissions(&guild, &["5"], "7", &muted) & VIEW_CHANNEL, 0);
        let admin = json!({ "id": "1", "owner_id": "9", "roles": [{ "id": "1", "permissions": "0" }, { "id": "5", "permissions": ADMINISTRATOR.to_string() }] });
        assert_eq!(permissions(&admin, &["5"], "7", &muted), u64::MAX);
    }
}
