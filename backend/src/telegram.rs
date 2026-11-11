//! Telegram through the Bot API as the project's own bot, with the token BotFather gave. Bots can't read a chat's history,
//! so social_channels calls getUpdates on the social poster's cadence, without long polling, to learn the groups and
//! channels the bot is in and their new messages.

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{StatusCode, Url};
use serde_json::{Value, json};

use crate::social::Failure;
use crate::social_channels::{Found, Message};

const API: &str = "https://api.telegram.org";
pub const CHARS: usize = 4096;
/// A wait Telegram asks for that's longer than this holds the bot's posting instead of holding up the tick.
const MAX_WAIT: Duration = Duration::from_secs(5);
/// Updates a getUpdates call returns at most.
pub const PAGE: usize = 100;

/// The link for a message. Supergroups and channels, whose chat ids start with -100, have one that opens it for members. A
/// basic group has none, so its link carries the chat id to tell the message apart, and it doesn't open anything.
pub fn message_link(chat_id: &str, message: &str) -> String {
    match chat_id.strip_prefix("-100") {
        Some(n) => format!("https://t.me/c/{n}/{message}"),
        None => format!("https://t.me/c/{}/{message}?chat={chat_id}", chat_id.trim_start_matches('-')),
    }
}

/// The chat and message ids in a link in the form message_link gives, also with a topic's id before the message's.
pub fn parse_link(link: &str) -> Option<(String, String)> {
    let url = Url::parse(link.trim()).ok()?;
    if url.scheme() != "https" || url.host_str()? != "t.me" {
        return None;
    }
    let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let (n, message) = match path.as_slice() {
        ["c", n, message] | ["c", n, _, message] => (*n, *message),
        _ => return None,
    };
    let numeric = |s: &str| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit());
    if !numeric(n) || !numeric(message) {
        return None;
    }
    let chat = match url.query_pairs().find(|(k, _)| k == "chat") {
        Some((_, chat)) if chat.strip_prefix('-').is_some_and(|id| id == n) => chat.into_owned(),
        Some(_) => return None,
        None => format!("-100{n}"),
    };
    Some((chat, message.into()))
}

/// The link anyone can open for a message: by @username in a public chat, else the one for members. Basic groups have none.
pub fn public_link(chat_id: &str, username: &str, message: &str) -> String {
    match (username, chat_id.starts_with("-100")) {
        ("", true) => message_link(chat_id, message),
        ("", false) => String::new(),
        (name, _) => format!("https://t.me/{name}/{message}"),
    }
}

/// The bot a token belongs to.
pub struct Bot {
    pub id: String,
    pub username: String,
    /// False while group privacy mode is on, when the bot only sees messages that mention it or answer it.
    pub reads_all: bool,
}

/// A chat as an update shows it. Whether the bot can read and post there comes from its membership, see `rights`.
pub fn chat(c: &Value) -> Option<Found> {
    let kind = c["type"].as_str().filter(|k| matches!(*k, "group" | "supergroup" | "channel"))?;
    Some(Found {
        chat_id: c["id"].as_i64()?.to_string(),
        guild_id: String::new(),
        guild_name: String::new(),
        name: c["title"].as_str().unwrap_or_default().into(),
        kind: kind.into(),
        username: c["username"].as_str().unwrap_or_default().into(),
        can_read: kind != "channel",
        can_send: true,
    })
}

/// Whether the bot can read and post in a chat, from its ChatMember there. A channel takes posts from admins that may post,
/// and its posts aren't for replying to. In a group, privacy mode hides other people's messages from a bot that isn't an admin.
pub fn rights(kind: &str, member: &Value, reads_all: bool) -> (bool, bool) {
    let status = member["status"].as_str().unwrap_or_default();
    let admin = matches!(status, "administrator" | "creator");
    match (kind, status) {
        (_, "left" | "kicked") => (false, false),
        ("channel", _) => (false, status == "creator" || admin && member["can_post_messages"] == true),
        (_, "restricted") => (reads_all && member["is_member"] == true, member["can_send_messages"] == true),
        _ => (reads_all || admin, true),
    }
}

/// A message's text, or a photo's or file's caption, with who sent it. Bots, the chat's own posts and service messages are
/// left out.
pub fn message(m: &Value) -> Option<Message> {
    let body = m["text"].as_str().or(m["caption"].as_str()).filter(|t| !t.trim().is_empty())?;
    let from = &m["from"];
    if from.is_null() || from["is_bot"] == true || !m["sender_chat"].is_null() {
        return None;
    }
    let name = format!("{} {}", from["first_name"].as_str().unwrap_or_default(), from["last_name"].as_str().unwrap_or_default());
    let author = from["username"].as_str().map(|u| format!("@{u}")).unwrap_or_else(|| name.trim().to_string());
    let sent_at = DateTime::<Utc>::from_timestamp(m["date"].as_i64()?, 0)?;
    Some(Message { id: m["message_id"].as_i64()?.to_string(), author, body: body.into(), sent_at })
}

pub struct Telegram {
    client: reqwest::Client,
    api: String,
    token: String,
    /// The wait from the last 429 that was too long to sit out, for the caller to hold posting with.
    retry_after: Mutex<Option<f64>>,
}

impl Telegram {
    /// `base` is Config::bots_api_url: empty for Telegram itself.
    pub fn new(base: &str, token: &str) -> Result<Self, Failure> {
        let client = reqwest::Client::builder().user_agent("Splee").timeout(Duration::from_secs(20)).build().map_err(|e| Failure::Account(e.to_string()))?;
        let api = if base.is_empty() { API } else { base };
        Ok(Self { client, api: api.into(), token: token.trim().into(), retry_after: Mutex::new(None) })
    }

    pub fn retry_after(&self) -> Option<f64> {
        *self.retry_after.lock().unwrap()
    }

    /// Sits out a short 429. A bad token, a webhook or another program reading the updates, and a long 429 are about the
    /// account. A chat the bot left or can't post in, a deleted message and anything that may have gone through are about the
    /// post. The token is in the URL, so errors leave the URL out.
    async fn call(&self, method: &str, params: Value) -> Result<Value, Failure> {
        let url = format!("{}/bot{}/{method}", self.api, self.token);
        for attempt in 0..2 {
            let res = self.client.post(&url).json(&params).send().await.map_err(|e| Failure::Post(format!("Telegram didn't answer: {}", e.without_url())))?;
            let status = res.status();
            let body: Value = res.json().await.unwrap_or_default();
            let said = body["description"].as_str().unwrap_or_default();
            if status == StatusCode::TOO_MANY_REQUESTS {
                let wait = body["parameters"]["retry_after"].as_f64().unwrap_or(60.0).max(0.0);
                if attempt == 0 && wait <= MAX_WAIT.as_secs_f64() {
                    tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                    continue;
                }
                *self.retry_after.lock().unwrap() = Some(wait);
                return Err(Failure::Account(format!("Telegram is rate limiting the bot for {} seconds", wait.ceil() as i64)));
            }
            return Err(match status {
                s if s.is_success() && body["ok"] == true => return Ok(body["result"].clone()),
                // A token in the wrong form gets a 404.
                StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND => {
                    Failure::Account("Telegram didn't accept the bot token. Copy it again from BotFather and connect Telegram again".into())
                }
                StatusCode::CONFLICT if said.contains("webhook") => Failure::Account(
                    "The bot has a webhook set, so Splee can't read its messages. Remove the webhook or connect another bot".into(),
                ),
                StatusCode::CONFLICT => Failure::Account("Another program is reading this bot's messages. Stop it or connect another bot".into()),
                s if s.is_server_error() || said.is_empty() => Failure::Post(format!("Telegram answered {s}")),
                _ => Failure::Post(format!("Telegram said: {said}")),
            });
        }
        unreachable!("the second attempt always returns")
    }

    pub async fn me(&self) -> Result<Bot, Failure> {
        let me = self.call("getMe", json!({})).await?;
        let id = me["id"].as_i64().ok_or_else(|| Failure::Account("Telegram didn't say which bot this is".into()))?;
        Ok(Bot { id: id.to_string(), username: me["username"].as_str().unwrap_or_default().into(), reads_all: me["can_read_all_group_messages"] == true })
    }

    /// Up to a page of updates from `offset` on, right away. Asking from an offset confirms the ones before it, which Telegram
    /// then drops.
    pub async fn updates(&self, offset: i64) -> Result<Vec<Value>, Failure> {
        let params = json!({ "offset": offset, "limit": PAGE, "timeout": 0, "allowed_updates": ["message", "channel_post", "my_chat_member"] });
        Ok(self.call("getUpdates", params).await?.as_array().cloned().unwrap_or_default())
    }

    /// None when Telegram says the chat is gone or the bot can't see it, a 4xx with a reason that `call` words as "Telegram
    /// said: …". A network failure or a 5xx stays an error, so a hiccup doesn't drop a picked chat.
    pub async fn member(&self, chat_id: &str, bot_id: &str) -> Result<Option<Value>, Failure> {
        match self.call("getChatMember", json!({ "chat_id": chat_id, "user_id": bot_id.parse::<i64>().unwrap_or_default() })).await {
            Err(Failure::Post(said)) if said.starts_with("Telegram said: ") => Ok(None),
            answer => answer.map(Some),
        }
    }

    /// Posts in the chat, as a reply to `reply_to` when there is one, and returns the new message's link where it has one.
    pub async fn send(&self, chat_id: &str, text: &str, reply_to: Option<&str>) -> Result<String, Failure> {
        let mut params = json!({ "chat_id": chat_id, "text": text });
        if let Some(message) = reply_to {
            params["reply_parameters"] = json!({ "message_id": message.parse::<i64>().unwrap_or_default() });
        }
        let sent = self.call("sendMessage", params).await?;
        let id = sent["message_id"].as_i64().map(|id| id.to_string()).unwrap_or_default();
        // It's out now, so not getting its id back doesn't fail it.
        Ok(if id.is_empty() { String::new() } else { public_link(chat_id, sent["chat"]["username"].as_str().unwrap_or_default(), &id) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_links_tell_supergroups_and_basic_groups_apart() {
        assert_eq!(message_link("-1001234567890", "42"), "https://t.me/c/1234567890/42");
        assert_eq!(parse_link("https://t.me/c/1234567890/42"), Some(("-1001234567890".into(), "42".into())));
        // A message in a forum topic has the topic's id first.
        assert_eq!(parse_link("https://t.me/c/1234567890/7/42?single"), Some(("-1001234567890".into(), "42".into())));
        assert_eq!(message_link("-4012345678", "9"), "https://t.me/c/4012345678/9?chat=-4012345678");
        assert_eq!(parse_link(&message_link("-4012345678", "9")), Some(("-4012345678".into(), "9".into())));
        assert_eq!(parse_link("https://t.me/c/4012345678/9?chat=-55"), None);
        assert_eq!(parse_link("https://t.me/acme/42"), None);
        assert_eq!(public_link("-1001234567890", "acme_chat", "42"), "https://t.me/acme_chat/42");
        assert_eq!(public_link("-4012345678", "", "9"), "");
    }

    #[test]
    fn rights_follow_the_bots_membership_and_privacy_mode() {
        let member = json!({ "status": "member" });
        assert_eq!(rights("supergroup", &member, false), (false, true));
        assert_eq!(rights("supergroup", &member, true), (true, true));
        assert_eq!(rights("group", &json!({ "status": "administrator" }), false), (true, true));
        assert_eq!(rights("group", &json!({ "status": "restricted", "is_member": true, "can_send_messages": false }), true), (true, false));
        assert_eq!(rights("channel", &json!({ "status": "administrator", "can_post_messages": true }), true), (false, true));
        assert_eq!(rights("channel", &member, true), (false, false));
        assert_eq!(rights("supergroup", &json!({ "status": "kicked" }), true), (false, false));
    }

    #[test]
    fn messages_from_people_are_kept() {
        let m = json!({ "message_id": 42, "date": 1762588800, "text": "Anyone tried warming up a domain?", "from": { "id": 5, "first_name": "Ana", "last_name": "Lee" } });
        let kept = message(&m).unwrap();
        assert_eq!((kept.id.as_str(), kept.author.as_str(), kept.sent_at.timestamp()), ("42", "Ana Lee", 1762588800));
        let named = json!({ "message_id": 43, "date": 1762588800, "caption": "Our results", "from": { "first_name": "Bo", "username": "bo" } });
        assert_eq!(message(&named).unwrap().author, "@bo");
        for skipped in [
            json!({ "message_id": 44, "date": 1762588800, "text": "Hi", "from": { "first_name": "Bot", "is_bot": true } }),
            json!({ "message_id": 45, "date": 1762588800, "text": "Hi", "from": { "first_name": "Ana" }, "sender_chat": { "id": -100 } }),
            json!({ "message_id": 46, "date": 1762588800, "from": { "first_name": "Ana" }, "new_chat_title": "Acme" }),
        ] {
            assert!(message(&skipped).is_none(), "{skipped}");
        }
    }
}
