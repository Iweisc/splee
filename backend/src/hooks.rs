//! Posts JSON to the URLs users paste into project settings (the lead-event webhook and Slack).
//! Those URLs can point anywhere, including this server's own network, so a host that resolves to a
//! private, loopback or link-local address is refused. The request is pinned to the addresses that
//! were checked, with no proxy and no redirects, so neither a second DNS answer nor a 302 can get around it.
//! With a secret, the request carries Splee-Timestamp and Splee-Signature: v1=<hex HMAC-SHA256 of "{timestamp}.{body}">.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::Context;
use hmac::{Hmac, KeyInit, Mac};
use reqwest::Url;
use reqwest::header::CONTENT_TYPE;
use serde_json::{Value, json};
use sha2::Sha256;
use uuid::Uuid;

use crate::models::{Lead, Ts, WebhookDelivery};
use crate::state::AppState;

/// A client for a URL someone typed, which only reaches that URL's host at the public addresses it resolved to now, so the
/// URL can't point the server at itself or its network. It doesn't follow redirects, which could lead anywhere.
pub async fn public_client(url: &Url) -> anyhow::Result<reqwest::Client> {
    anyhow::ensure!(matches!(url.scheme(), "http" | "https"), "only http and https URLs are allowed");
    let host = url.host_str().context("the URL has no host")?;
    let port = url.port_or_known_default().context("the URL has no port")?;
    let addrs: Vec<SocketAddr> = match host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host, port)).await?.collect(),
    };
    anyhow::ensure!(!addrs.is_empty() && addrs.iter().all(|a| is_public(a.ip())), "{host} is not a public address");
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        // A proxy from HTTP(S)_PROXY would resolve the host itself and skip the check above.
        .no_proxy()
        .resolve_to_addrs(host, &addrs)
        .build()?)
}

pub async fn post_json(raw: &str, body: &Value, secret: Option<&str>) -> anyhow::Result<()> {
    let url = Url::parse(raw.trim())?;
    let client = public_client(&url).await?;
    let host = url.host_str().unwrap_or_default().to_string();
    // Serialized once so the signature covers exactly the bytes sent.
    let body = serde_json::to_vec(body)?;
    let mut request = client.post(url).header(CONTENT_TYPE, "application/json");
    if let Some(secret) = secret {
        let timestamp = chrono::Utc::now().timestamp().to_string();
        request = request.header("Splee-Timestamp", &timestamp).header("Splee-Signature", format!("v1={}", sign(secret, &timestamp, &body)));
    }
    // Without the URL: Slack, Discord and Telegram URLs carry their secret, and errors are logged and shown.
    let res = request.body(body).send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    if status.is_client_error() || status.is_server_error() {
        return Err(Refused::new(status, &first_bytes(res).await).into());
    }
    if let Some((code_key, message_key)) = error_in_body(&host.to_ascii_lowercase()) {
        let reply: Value = serde_json::from_slice(&first_bytes(res).await).unwrap_or_default();
        if let Some(code) = reply[code_key].as_i64().filter(|&code| code != 0) {
            // WeCom follows its reason with a request id and this server's address, which only add noise.
            let message = reply[message_key].as_str().map(|m| m.split(", hint:").next().unwrap_or(m).trim()).filter(|m| !m.is_empty());
            anyhow::bail!("{}", message.map_or_else(|| format!("code {code}"), str::to_string));
        }
    }
    Ok(())
}

/// Lark and its China edition Feishu: the same bot webhooks on two hosts.
const LARK: [&str; 2] = ["open.larksuite.com", "open.feishu.cn"];
const DINGTALK: &str = "oapi.dingtalk.com";
const WECOM: &str = "qyapi.weixin.qq.com";
const WEBEX: &str = "webexapis.com";

/// The chat apps that answer 200 to every post and put a failure, like a wrong token, in the body: the keys of its
/// code, which is 0 when the post went through, and of its reason.
fn error_in_body(host: &str) -> Option<(&'static str, &'static str)> {
    match host {
        h if LARK.contains(&h) => Some(("code", "msg")),
        DINGTALK | WECOM => Some(("errcode", "errmsg")),
        _ => None,
    }
}

/// An endpoint that answered with an error status, and the reason it gave, like Discord's "Unknown Webhook".
#[derive(Debug)]
struct Refused {
    status: reqwest::StatusCode,
    reason: Option<String>,
}

impl Refused {
    /// Telegram's "Unauthorized" only repeats the status, so it's left out.
    fn new(status: reqwest::StatusCode, body: &[u8]) -> Refused {
        let reason = reason(body).filter(|r| !status.canonical_reason().is_some_and(|c| c.eq_ignore_ascii_case(r)));
        Refused { status, reason }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.status)?;
        if let Some(reason) = &self.reason {
            write!(f, ": {reason}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Refused {}

/// Enough of an error body to find its reason in, without reading whatever a broken endpoint streams back.
async fn first_bytes(mut res: reqwest::Response) -> Vec<u8> {
    let mut body = Vec::new();
    while body.len() < 16 * 1024
        && let Ok(Some(chunk)) = res.chunk().await
    {
        body.extend_from_slice(&chunk);
    }
    body
}

/// What an endpoint said when it refused a post. The chat apps put it in different places: Telegram in description,
/// Discord in message, Google Chat and Teams in error.message, Gotify in errorDescription, ntfy in error, Pushover in
/// errors, and Slack in a plain-text body.
fn reason(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let said = match serde_json::from_str::<Value>(&text) {
        Ok(json) => ["/description", "/message", "/error/message", "/errorDescription", "/error", "/errors/0"].iter().find_map(|p| json.pointer(p)?.as_str())?.to_string(),
        // An HTML error page says nothing a person can act on.
        Err(_) if text.trim_start().starts_with('<') => return None,
        Err(_) => text.into_owned(),
    };
    let said = said.split_whitespace().collect::<Vec<_>>().join(" ");
    match said.char_indices().nth(200) {
        _ if said.is_empty() => None,
        Some((end, _)) => Some(format!("{}…", &said[..end])),
        None => Some(said),
    }
}

const RETRY_AFTER: Duration = Duration::from_secs(10);

/// Like post_json, with one more try after a short wait when the failure may pass: a timeout, a refused connection,
/// 429 or a 5xx. Each try is signed afresh, so receivers that dedupe should use the payload's ids.
pub async fn post_json_retrying(raw: &str, body: &Value, secret: Option<&str>) -> anyhow::Result<()> {
    match post_json(raw, body, secret).await {
        Err(e) if transient(&e) => {
            tracing::info!(error = %e, "alert failed, retrying once");
            tokio::time::sleep(RETRY_AFTER).await;
            post_json(raw, body, secret).await
        }
        result => result,
    }
}

fn transient(e: &anyhow::Error) -> bool {
    if let Some(refused) = e.downcast_ref::<Refused>() {
        return refused.status.is_server_error() || refused.status == reqwest::StatusCode::TOO_MANY_REQUESTS;
    }
    e.downcast_ref::<reqwest::Error>().is_some_and(|e| e.is_timeout() || e.is_connect())
}

#[derive(Clone, Copy)]
pub enum Alert {
    /// Matched on webhook_endpoints.url.
    Webhook,
    /// Matched on chat_channels.url.
    Chat,
    /// Matched on {name}_token_enc, the stored ciphertext.
    Crm(crate::crm::Crm),
    /// Matched on verifier_key_enc.
    Verifier,
    /// Matched on finder_key_enc.
    Finder,
    /// Matched on unipile_token_enc.
    Unipile,
    /// Matched on bcc_email, the address the copy went to.
    Bcc,
}

/// Saves how an alert went, so the integrations page can show a broken endpoint: the error on failure, nothing on success.
/// Matching on the URL keeps a slow delivery to an old URL from marking a URL that was saved since.
pub async fn record(db: &sqlx::PgPool, project_id: uuid::Uuid, alert: Alert, url: &str, result: &anyhow::Result<()>) {
    // The names are fixed strings, never user input.
    let (prefix, column) = match alert {
        Alert::Webhook => return record_endpoint(db, "webhook_endpoints", project_id, url, result).await,
        Alert::Chat => return record_endpoint(db, "chat_channels", project_id, url, result).await,
        Alert::Crm(crm) => (crm.name(), format!("{}_token_enc", crm.name())),
        Alert::Verifier => ("verifier", "verifier_key_enc".to_string()),
        Alert::Finder => ("finder", "finder_key_enc".to_string()),
        Alert::Unipile => ("unipile", "unipile_token_enc".to_string()),
        Alert::Bcc => ("bcc", "bcc_email".to_string()),
    };
    let sql = format!(
        "update project_settings set {prefix}_error = $3, {prefix}_failed_at = case when $3::text is null then null else now() end
         where project_id = $1 and {column} = $2"
    );
    let error = result.as_ref().err().map(|e| format!("{e:#}").chars().take(500).collect::<String>());
    if let Err(e) = sqlx::query(&sql).bind(project_id).bind(url).bind(error).execute(db).await {
        tracing::warn!(error = %e, "couldn't save the alert outcome");
    }
}

/// `table` is chat_channels or webhook_endpoints, never user input.
async fn record_endpoint(db: &sqlx::PgPool, table: &str, project_id: Uuid, url: &str, result: &anyhow::Result<()>) {
    let error = result.as_ref().err().map(|e| format!("{e:#}").chars().take(500).collect::<String>());
    let sql = format!("update {table} set error = $3, failed_at = case when $3::text is null then null else now() end where project_id = $1 and url = $2");
    if let Err(e) = sqlx::query(&sql).bind(project_id).bind(url).bind(error).execute(db).await {
        tracing::warn!(error = %e, "couldn't save the alert outcome");
    }
}

/// The lead events a project's webhook can subscribe to. Keep in sync with EVENTS on the integrations page.
pub const EVENTS: [&str; 11] = [
    "hot_lead",
    "lead_replied",
    "meeting_booked",
    "meeting_rescheduled",
    "meeting_cancelled",
    "lead_unsubscribed",
    "lead_bounced",
    "email_sent",
    "follow_up_due",
    "form_submitted",
    "status_changed",
];

/// What an event carries besides the lead: the reply that set it off, the email Splee sent, a reminder, a form's answers or
/// a moved call's old time or status. Its key is "reply", "email", "reminder", "form", "previous_meeting_at" or "previous_status", and "reply" stays in every payload, null when there's none.
pub type Detail = Option<(&'static str, Value)>;

/// The endpoint URLs, secret and payload for a lead event, or None when the project has no endpoint or doesn't send it.
async fn lead_event(
    db: &sqlx::PgPool,
    public_url: &str,
    project_id: Uuid,
    event: &str,
    lead: &Lead,
    detail: Detail,
) -> sqlx::Result<Option<(Vec<String>, String, Value)>> {
    let settings: Option<(String, Vec<String>, String)> = sqlx::query_as(
        "select s.webhook_secret, s.webhook_events, p.name from project_settings s join projects p on p.id = s.project_id where s.project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await?;
    let Some((secret, events, name)) = settings else { return Ok(None) };
    if !events.iter().any(|e| e == event) {
        return Ok(None);
    }
    let urls: Vec<String> = sqlx::query_scalar("select url from webhook_endpoints where project_id = $1 order by created_at")
        .bind(project_id)
        .fetch_all(db)
        .await?;
    if urls.is_empty() {
        return Ok(None);
    }
    // Keep in sync with PAYLOAD on the integrations page.
    let mut payload = json!({
        "event": event,
        "project": {"id": project_id, "name": name},
        "lead": {
            "id": lead.id, "full_name": lead.full_name, "email": lead.email, "title": lead.title, "status": lead.status,
            "company_name": lead.company_name, "company_domain": lead.company_domain,
            "linkedin_url": lead.linkedin_url, "x_url": lead.x_url, "meeting_at": lead.meeting_at,
        },
        "reply": null,
        "url": lead_url(public_url, project_id, lead.id),
    });
    if let Some((key, value)) = detail {
        payload[key] = value;
    }
    Ok(Some((urls, secret, payload)))
}

/// Posts a lead event to each of the project's webhook endpoints when it sends that event. Delivery runs in the
/// background, one task per endpoint, so a slow receiver never holds up a run, a request or the other endpoints.
/// Failures are logged and shown on the integrations page.
pub async fn send_lead_event(state: &AppState, project_id: Uuid, event: &'static str, lead: &Lead, detail: Detail) {
    match lead_event(&state.db, &state.cfg.public_url, project_id, event, lead, detail).await {
        Ok(Some((urls, secret, payload))) => {
            for url in urls {
                let (db, secret, payload) = (state.db.clone(), secret.clone(), payload.clone());
                tokio::spawn(async move {
                    let result = post_json_retrying(&url, &payload, Some(&secret)).await;
                    if let Err(e) = &result {
                        tracing::warn!(error = %e, event, "webhook failed");
                    }
                    record(&db, project_id, Alert::Webhook, &url, &result).await;
                    if let Err(e) = log_delivery(&db, project_id, event, &url, &payload, &result).await {
                        tracing::warn!(error = %e, "couldn't log the webhook delivery");
                    }
                });
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, event, "couldn't load the webhook settings"),
    }
}

/// How many deliveries the integrations page keeps per project.
const DELIVERIES_KEPT: i64 = 50;

/// Keeps a webhook delivery for the integrations page and drops the oldest past DELIVERIES_KEPT.
pub async fn log_delivery(
    db: &sqlx::PgPool,
    project_id: Uuid,
    event: &str,
    url: &str,
    payload: &Value,
    result: &anyhow::Result<()>,
) -> sqlx::Result<WebhookDelivery> {
    let error = result.as_ref().err().map(|e| format!("{e:#}").chars().take(500).collect::<String>());
    let delivery = sqlx::query_as::<_, WebhookDelivery>(
        "insert into webhook_deliveries (project_id, event, url, payload, error) values ($1, $2, $3, $4, $5) returning *",
    )
    .bind(project_id)
    .bind(event)
    .bind(url)
    .bind(payload)
    .bind(error)
    .fetch_one(db)
    .await?;
    sqlx::query(
        "delete from webhook_deliveries where project_id = $1 and id not in
           (select id from webhook_deliveries where project_id = $1 order by created_at desc limit $2)",
    )
    .bind(project_id)
    .bind(DELIVERIES_KEPT)
    .execute(db)
    .await?;
    Ok(delivery)
}

/// Opens this lead's conversation, not just the inbox.
pub fn lead_url(public_url: &str, project_id: Uuid, lead_id: Uuid) -> String {
    format!("{public_url}/app/p/{project_id}/inbox?lead={lead_id}")
}

/// A chat alert, written once and formatted for whichever app the project's chat webhook belongs to.
pub struct ChatAlert<'a> {
    /// Bold, before the summary, like "Hot lead".
    pub label: &'a str,
    pub summary: &'a str,
    /// Quoted under the summary, like the lead's reply.
    pub quote: Option<&'a str>,
    pub link: &'a str,
    pub link_text: &'a str,
}

fn host(url: &str) -> String {
    Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default()
}

/// Where a chat alert is posted. ntfy takes JSON only at its root, with the topic from the user's https://ntfy.sh/{topic}
/// URL in the body, and keeps the query, where a self-hosted server's `?auth=` goes. Pushover reads its token and user key
/// from the body only, so they move there from the query. Everything else is posted to the URL as given.
pub fn chat_endpoint(url: &str) -> String {
    if ntfy(url)
        && let Ok(mut root) = Url::parse(url)
    {
        root.set_path("/");
        return root.to_string();
    }
    match host(url).as_str() {
        PUSHOVER => format!("https://{PUSHOVER}/1/messages.json"),
        _ => url.to_string(),
    }
}

/// ntfy.sh, or a self-hosted ntfy server, which can be on any host, so it's told apart by a name starting with ntfy.,
/// like ntfy.example.com, the way ntfy's docs name one. Keep in sync with chatApp in the integrations page.
fn ntfy(url: &str) -> bool {
    host(url).starts_with("ntfy.")
}

const PUSHOVER: &str = "api.pushover.net";

/// A Gotify server can be on any host, so it's told apart by its URL for sending a message: /message with the app's
/// `?token=`. Keep in sync with chatApp in the integrations page.
fn gotify(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| u.path_segments().and_then(|mut s| s.next_back()) == Some("message") && u.query_pairs().any(|(k, _)| k == "token"))
}

/// A Mattermost server can be on any host, so its incoming webhook is told apart by the path, which ends in /hooks/ and
/// the webhook's 26-character id. Keep in sync with chatApp in the integrations page.
fn mattermost(url: &str) -> bool {
    let Ok(url) = Url::parse(url) else { return false };
    let segments: Vec<&str> = url.path_segments().map(Iterator::collect).unwrap_or_default();
    matches!(segments.as_slice(), [.., "hooks", id] if id.len() == 26 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

fn query(url: &str, key: &str) -> Option<String> {
    Url::parse(url).ok()?.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

/// The JSON a chat webhook takes. Discord, Teams (Workflows and the older connectors), Telegram (a bot's sendMessage
/// URL with `?chat_id=`), ntfy (a topic URL on ntfy.sh or an ntfy.* server), Pushover (its messages.json URL with `?token=` and `&user=`), Lark or
/// Feishu (a custom bot's URL), DingTalk (a robot's `?access_token=` URL), WeCom (a group robot's `?key=` URL) and Webex
/// (an incoming webhook's URL) have their own shapes, as does Zoho Cliq (a channel's `?zapikey=` URL), and Gotify (its /message?token= URL),
/// Mattermost and Pumble want markdown. Anything else gets Slack's `{text}`,
/// which Google Chat, Rocket.Chat and Zulip's Slack compatible webhook also read, links included.
pub fn chat_payload(url: &str, a: &ChatAlert) -> Value {
    let host = host(url);
    let on = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    let quote = a.quote.map(|q| format!("\n> {}", q.replace('\n', "\n> "))).unwrap_or_default();
    // ntfy and Pushover show the reply as plain text under the summary.
    let message = match a.quote {
        Some(q) => format!("{}\n\n{q}", a.summary),
        None => a.summary.to_string(),
    };
    if ntfy(url) {
        let topic = Url::parse(url).ok().and_then(|u| u.path_segments()?.rfind(|s| !s.is_empty()).map(str::to_string)).unwrap_or_default();
        json!({
            "topic": topic, "title": a.label, "message": message, "click": a.link,
            "actions": [{"action": "view", "label": a.link_text, "url": a.link}],
        })
    } else if host == PUSHOVER {
        // Pushover refuses a message over 1024 characters, and a long reply would pass that.
        let message = match message.char_indices().nth(1023) {
            Some((end, _)) if message.chars().nth(1024).is_some() => format!("{}…", &message[..end]),
            _ => message,
        };
        json!({
            "token": query(url, "token"), "user": query(url, "user"), "title": a.label, "message": message,
            "url": a.link, "url_title": a.link_text,
        })
    } else if LARK.contains(&host.as_str()) {
        let mut lines = vec![json!([{"tag": "text", "text": a.summary}])];
        if let Some(q) = a.quote {
            lines.push(json!([{"tag": "text", "text": q}]));
        }
        lines.push(json!([{"tag": "a", "text": a.link_text, "href": a.link}]));
        json!({"msg_type": "post", "content": {"post": {"en_us": {"title": a.label, "content": lines}}}})
    } else if host == DINGTALK || host == WECOM || host == WEBEX {
        let (head, tail) = (format!("**{}:** {}", a.label, a.summary), format!("\n\n[{}]({})", a.link_text, a.link));
        // A blank line between quoted lines, or they run together into one.
        let quote = quote.replace("\n>", "\n\n>");
        if host == DINGTALK {
            json!({"msgtype": "markdown", "markdown": {"title": a.label, "text": format!("{head}{quote}{tail}")}})
        } else {
            // WeCom refuses markdown over 4096 bytes and Webex a message over 7439, and a long reply would pass that. The
            // reply is cut, not the link.
            let max: usize = if host == WECOM { 4096 } else { 7439 };
            let content = format!("{head}{}{tail}", cut_bytes(&quote, max.saturating_sub(head.len() + tail.len())));
            if host == WECOM { json!({"msgtype": "markdown", "markdown": {"content": content}}) } else { json!({"markdown": content}) }
        }
    } else if on("discord.com") || on("discordapp.com") {
        json!({"content": format!("**{}:** {}{quote}\n[{}]({})", a.label, a.summary, a.link_text, a.link)})
    } else if gotify(url) {
        // Markdown, so the link can be followed in the web app too. Tapping the phone notification opens it. Without a
        // priority it gets the app's, 0 by default, which the phone app doesn't notify about. Markdown joins a reply's lines
        // unless each ends in a hard break.
        let quote = quote.replace("\n>", "  \n>");
        json!({
            "title": a.label, "message": format!("{}{quote}\n\n[{}]({})", a.summary, a.link_text, a.link), "priority": 5,
            "extras": {"client::display": {"contentType": "text/markdown"}, "client::notification": {"click": {"url": a.link}}},
        })
    } else if mattermost(url) || host == "api.pumble.com" {
        // Mattermost and Pumble read Slack's links but show Slack's *bold* as italics.
        json!({"text": format!("**{}:** {}{quote}\n[{}]({})", a.label, a.summary, a.link_text, a.link)})
    } else if host == "api.telegram.org" {
        // chat_id goes in the body too, so it doesn't matter whether Telegram reads the query of a JSON request.
        let chat_id = query(url, "chat_id");
        let quote = a.quote.map(|q| format!("\n<blockquote>{}</blockquote>", html(q))).unwrap_or_default();
        let text = format!("<b>{}:</b> {}{quote}\n<a href=\"{}\">{}</a>", html(a.label), html(a.summary), html(a.link), html(a.link_text));
        json!({"chat_id": chat_id, "text": text, "parse_mode": "HTML", "link_preview_options": {"is_disabled": true}})
    } else if on("logic.azure.com") || on("powerplatform.com") || on("webhook.office.com") {
        let mut body = vec![json!({"type": "TextBlock", "text": format!("**{}:** {}", a.label, a.summary), "wrap": true})];
        if let Some(q) = a.quote {
            body.push(json!({"type": "TextBlock", "text": q, "wrap": true, "isSubtle": true}));
        }
        json!({"type": "message", "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "content": {
                "$schema": "http://adaptivecards.io/schemas/adaptive-card.json", "type": "AdaptiveCard", "version": "1.4", "body": body,
                "actions": [{"type": "Action.OpenUrl", "title": a.link_text, "url": a.link}],
            },
        }]})
    } else if host.starts_with("cliq.zoho") {
        // Zoho Cliq, on cliq.zoho.com, cliq.zoho.eu and its other regions' hosts, shows the label as the card's title and
        // reads markdown links. Its quote mark isn't markdown's, so the reply goes under the summary as it is. Cliq refuses
        // text over 10,000 characters, and a long reply would pass that. The reply is cut, not the link.
        let tail = format!("\n\n[{}]({})", a.link_text, a.link);
        let reply = a.quote.map(|q| format!("\n\n{q}")).unwrap_or_default();
        let text = format!("{}{}{tail}", a.summary, cut_bytes(&reply, 10_000usize.saturating_sub(a.summary.len() + tail.len())));
        json!({"text": text, "card": {"title": a.label}})
    } else {
        json!({"text": format!("*{}:* {}{quote}\n<{}|{}>", a.label, a.summary, a.link, a.link_text)})
    }
}

/// `text` cut to at most `max` bytes, at a character boundary, ending in an ellipsis when it was cut.
fn cut_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    if max < '…'.len_utf8() {
        return String::new();
    }
    let end = (0..=max - '…'.len_utf8()).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    format!("{}…", &text[..end])
}

/// For Telegram's HTML mode, which needs only these escaped.
fn html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Posts an alert to each of the project's chats at once, so a slow or broken one doesn't hold up the rest, and records
/// how each went for the integrations page.
pub async fn post_chat(db: &sqlx::PgPool, project_id: Uuid, alert: &ChatAlert<'_>) {
    let urls: Vec<String> = match sqlx::query_scalar("select url from chat_channels where project_id = $1").bind(project_id).fetch_all(db).await {
        Ok(urls) => urls,
        Err(e) => return tracing::warn!(error = %e, "couldn't load the chat channels"),
    };
    futures::future::join_all(urls.iter().map(|url| async move {
        let result = post_json_retrying(&chat_endpoint(url), &chat_payload(url, alert), None).await;
        if let Err(e) = &result {
            tracing::warn!(error = %e, "chat notification failed");
        }
        record(db, project_id, Alert::Chat, url, &result).await;
    }))
    .await;
}

/// Whether an update moved the lead to meeting_booked. `before` is its status before the update.
pub fn booked(before: &str, lead: &Lead) -> bool {
    lead.status == "meeting_booked" && before != "meeting_booked"
}

/// A meeting's start in the project's timezone, like "Tue 10 Nov at 14:00 GMT". An unknown timezone falls back to UTC.
fn meeting_time(at: Ts, timezone: &str) -> String {
    let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
    at.with_timezone(&tz).format("%a %-d %b at %H:%M %Z").to_string()
}

/// "Sarah Chen (Brightline) for Acme, Tue 10 Nov at 14:00 GMT", the time in the project's timezone, for meeting alerts.
async fn meeting_summary(db: &sqlx::PgPool, lead: &Lead, at: Option<Ts>) -> String {
    let (project, timezone): (String, String) = sqlx::query_as(
        "select p.name, coalesce(s.timezone, 'UTC') from projects p left join project_settings s on s.project_id = p.id where p.id = $1",
    )
    .bind(lead.project_id)
    .fetch_one(db)
    .await
    .unwrap_or_default();
    let who = format!("{} ({})", lead.full_name, lead.company_name);
    match at {
        Some(at) => format!("{who} for {project}, {}", meeting_time(at, &timezone)),
        None => format!("{who} for {project}"),
    }
}

/// Tells the team in chat and HubSpot that a lead booked a meeting. It runs in the background, so the request that set
/// the status doesn't wait on either.
pub fn announce_booked(state: &AppState, lead: &Lead) {
    let (state, lead, project_id) = (state.clone(), lead.clone(), lead.project_id);
    let link = lead_url(&state.cfg.public_url, project_id, lead.id);
    tokio::spawn(async move {
        let summary = meeting_summary(&state.db, &lead, lead.meeting_at).await;
        let alert = ChatAlert { label: "Meeting booked", summary: &summary, quote: None, link: &link, link_text: "Open the conversation" };
        post_chat(&state.db, project_id, &alert).await;
        crate::crm::sync(&state, project_id, &lead, &format!("Booked a meeting through Splee.\n{link}")).await;
    });
}

/// Tells chat that a lead wrote back on LinkedIn, which stopped their sequence and emails.
pub fn announce_linkedin_reply(state: &AppState, lead: &Lead, text: &str) {
    let (state, lead, text) = (state.clone(), lead.clone(), text.to_string());
    let link = lead_url(&state.cfg.public_url, lead.project_id, lead.id);
    tokio::spawn(async move {
        let summary = format!("{} ({}) wrote back on LinkedIn", lead.full_name, lead.company_name);
        let alert = ChatAlert { label: "LinkedIn reply", summary: &summary, quote: (!text.is_empty()).then_some(text.as_str()), link: &link, link_text: "Open the lead" };
        post_chat(&state.db, lead.project_id, &alert).await;
    });
}

/// Tells the team in chat, the webhook and the CRMs that a lead cancelled the call they booked. `was` is when it would have
/// been, since the lead no longer has the time.
/// Tells chat, the webhook and the CRMs that a booked lead moved their call. `lead.meeting_at` is the new time.
pub fn announce_rescheduled(state: &AppState, lead: &Lead, was: Option<Ts>) {
    let (state, lead, project_id) = (state.clone(), lead.clone(), lead.project_id);
    let link = lead_url(&state.cfg.public_url, project_id, lead.id);
    tokio::spawn(async move {
        let summary = meeting_summary(&state.db, &lead, lead.meeting_at).await;
        let alert = ChatAlert { label: "Meeting moved", summary: &summary, quote: None, link: &link, link_text: "Open the conversation" };
        post_chat(&state.db, project_id, &alert).await;
        send_lead_event(&state, project_id, "meeting_rescheduled", &lead, Some(("previous_meeting_at", json!(was)))).await;
        crate::crm::sync(&state, project_id, &lead, &format!("Moved the meeting they booked.\n{link}")).await;
    });
}

pub fn announce_cancelled(state: &AppState, lead: &Lead, was: Option<Ts>) {
    let (state, lead, project_id) = (state.clone(), lead.clone(), lead.project_id);
    let link = lead_url(&state.cfg.public_url, project_id, lead.id);
    tokio::spawn(async move {
        let summary = meeting_summary(&state.db, &lead, was).await;
        let alert = ChatAlert { label: "Meeting cancelled", summary: &summary, quote: None, link: &link, link_text: "Open the conversation" };
        post_chat(&state.db, project_id, &alert).await;
        send_lead_event(&state, project_id, "meeting_cancelled", &lead, None).await;
        crate::crm::sync(&state, project_id, &lead, &format!("Cancelled the meeting they booked.\n{link}")).await;
    });
}

/// Tells each project's chat and webhook about inbox reminders that just came due. A reminder more than a day overdue is skipped, so
/// one set before this ran, or while the server was down, doesn't page anyone.
pub async fn announce_due_reminders(state: &AppState) -> anyhow::Result<()> {
    for lead in claim_due_reminders(&state.db).await? {
        let project: String =
            sqlx::query_scalar("select name from projects where id = $1").bind(lead.project_id).fetch_one(&state.db).await.unwrap_or_default();
        let who = if lead.company_name.is_empty() { lead.full_name.clone() } else { format!("{} ({})", lead.full_name, lead.company_name) };
        let summary = format!("{who} for {project}");
        let link = lead_url(&state.cfg.public_url, lead.project_id, lead.id);
        let notes = lead.notes.trim();
        let alert = ChatAlert { label: "Follow-up due", summary: &summary, quote: (!notes.is_empty()).then_some(notes), link: &link, link_text: "Open the conversation" };
        post_chat(&state.db, lead.project_id, &alert).await;
        let reminder = json!({"at": lead.remind_at, "notes": lead.notes});
        send_lead_event(state, lead.project_id, "follow_up_due", &lead, Some(("reminder", reminder))).await;
    }
    Ok(())
}

/// Marks the reminders that just came due as announced and returns their leads. One statement, so a reminder is
/// announced once even if two runs overlap.
async fn claim_due_reminders(db: &sqlx::PgPool) -> sqlx::Result<Vec<Lead>> {
    sqlx::query_as(
        "update leads set remind_alerted_at = now()
         where remind_at <= now() and remind_at > now() - interval '1 day'
           and (remind_alerted_at is null or remind_alerted_at < remind_at)
         returning *",
    )
    .fetch_all(db)
    .await
}

/// A status someone on the team set in the app, alone or in bulk, sends the status_changed event on top of what
/// status_changed below sends. Replies, bounces and bookings have their own events, so they don't come through here.
pub async fn team_set_status(state: &AppState, before: &str, lead: &Lead) {
    status_changed(state, before, lead).await;
    if before != lead.status {
        send_lead_event(state, lead.project_id, "status_changed", lead, Some(("previous_status", json!(before)))).await;
    }
}

/// Sends meeting_booked to chat and the webhook when someone in the app moves a lead there. The API only posts to chat
/// (see api_v1::update_lead).
pub async fn status_changed(state: &AppState, before: &str, lead: &Lead) {
    if booked(before, lead) {
        announce_booked(state, lead);
        send_lead_event(state, lead.project_id, "meeting_booked", lead, None).await;
    }
}

fn sign(secret: &str, timestamp: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            let shared = a == 100 && (64..128).contains(&b); // 100.64.0.0/10, carrier-grade NAT
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_documentation()
                || v4.is_multicast()
                || shared
                || a == 0
                || a >= 240)
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public(IpAddr::V4(v4)),
            None => !(v6.is_loopback() || v6.is_unspecified() || v6.is_unique_local() || v6.is_unicast_link_local() || v6.is_multicast()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_alerts_match_each_app() {
        let a = ChatAlert { label: "Hot lead", summary: "Sarah replied to Acme", quote: Some("Sounds good\nTuesday?"), link: "https://app/x", link_text: "Open the inbox" };
        let discord = chat_payload("https://discord.com/api/webhooks/1/abc", &a);
        assert_eq!(discord["content"], "**Hot lead:** Sarah replied to Acme\n> Sounds good\n> Tuesday?\n[Open the inbox](https://app/x)");
        let teams = chat_payload("https://prod-12.westus.logic.azure.com:443/workflows/abc", &a);
        let card = &teams["attachments"][0]["content"];
        assert_eq!(card["body"][0]["text"], "**Hot lead:** Sarah replied to Acme");
        assert_eq!(card["body"][1]["text"], "Sounds good\nTuesday?");
        assert_eq!(card["actions"][0]["url"], "https://app/x");
        let slack = "*Hot lead:* Sarah replied to Acme\n> Sounds good\n> Tuesday?\n<https://app/x|Open the inbox>";
        assert_eq!(chat_payload("https://hooks.slack.com/services/T/B/x", &a)["text"], slack);
        assert_eq!(chat_payload("https://chat.googleapis.com/v1/spaces/x/messages?key=k", &a)["text"], slack);
        let telegram = chat_payload("https://api.telegram.org/bot123:abc/sendMessage?chat_id=-1001", &ChatAlert { summary: "Sam <CTO> replied", ..a });
        assert_eq!(telegram["chat_id"], "-1001");
        assert_eq!(telegram["parse_mode"], "HTML");
        assert_eq!(
            telegram["text"],
            "<b>Hot lead:</b> Sam &lt;CTO&gt; replied\n<blockquote>Sounds good\nTuesday?</blockquote>\n<a href=\"https://app/x\">Open the inbox</a>"
        );
        let ntfy = chat_payload("https://ntfy.sh/acme-hot-leads", &a);
        assert_eq!(ntfy["topic"], "acme-hot-leads");
        assert_eq!(ntfy["title"], "Hot lead");
        assert_eq!(ntfy["message"], "Sarah replied to Acme\n\nSounds good\nTuesday?");
        assert_eq!(ntfy["click"], "https://app/x");
        assert_eq!(chat_endpoint("https://ntfy.sh/acme-hot-leads"), "https://ntfy.sh/");
        let own = "https://ntfy.acme.example:8443/hot-leads?auth=QmVhcmVy";
        assert_eq!((chat_payload(own, &a)["topic"].as_str(), chat_endpoint(own)), (Some("hot-leads"), "https://ntfy.acme.example:8443/?auth=QmVhcmVy".to_string()));
        assert_eq!(chat_endpoint("https://hooks.slack.com/services/T/B/x"), "https://hooks.slack.com/services/T/B/x");
        let pushover_url = "https://api.pushover.net/1/messages.json?token=app123&user=user456";
        let pushover = chat_payload(pushover_url, &a);
        assert_eq!((pushover["token"].as_str(), pushover["user"].as_str()), (Some("app123"), Some("user456")));
        assert_eq!(pushover["message"], "Sarah replied to Acme\n\nSounds good\nTuesday?");
        assert_eq!((pushover["url"].as_str(), pushover["url_title"].as_str()), (Some("https://app/x"), Some("Open the inbox")));
        assert_eq!(chat_endpoint(pushover_url), "https://api.pushover.net/1/messages.json");
        let long = "é".repeat(2000);
        let cut = chat_payload(pushover_url, &ChatAlert { quote: Some(&long), ..a })["message"].as_str().unwrap().to_string();
        assert_eq!((cut.chars().count(), cut.ends_with("é…")), (1024, true));
        let fits = "é".repeat(1024 - "Sarah replied to Acme\n\n".len());
        assert_eq!(chat_payload(pushover_url, &ChatAlert { quote: Some(&fits), ..a })["message"].as_str().unwrap().chars().last(), Some('é'));
        // A lookalike host isn't Discord.
        let lark = chat_payload("https://open.larksuite.com/open-apis/bot/v2/hook/abc", &a);
        assert_eq!(lark["msg_type"], "post");
        let post = &lark["content"]["post"]["en_us"];
        assert_eq!(post["title"], "Hot lead");
        assert_eq!(post["content"][0][0]["text"], "Sarah replied to Acme");
        assert_eq!(post["content"][1][0]["text"], "Sounds good\nTuesday?");
        assert_eq!((post["content"][2][0]["tag"].as_str(), post["content"][2][0]["href"].as_str()), (Some("a"), Some("https://app/x")));
        assert_eq!(chat_payload("https://open.feishu.cn/open-apis/bot/v2/hook/abc", &a)["msg_type"], "post");
        let dingtalk = chat_payload("https://oapi.dingtalk.com/robot/send?access_token=abc", &a);
        assert_eq!(dingtalk["markdown"]["title"], "Hot lead");
        let markdown = "**Hot lead:** Sarah replied to Acme\n\n> Sounds good\n\n> Tuesday?\n\n[Open the inbox](https://app/x)";
        assert_eq!(dingtalk["markdown"]["text"], markdown);
        let wecom = chat_payload("https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=abc", &a);
        assert_eq!((wecom["msgtype"].as_str(), wecom["markdown"]["content"].as_str()), (Some("markdown"), Some(markdown)));
        let long = "é".repeat(3000);
        let content = chat_payload("https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=abc", &ChatAlert { quote: Some(&long), ..a });
        let content = content["markdown"]["content"].as_str().unwrap();
        assert!(content.len() <= 4096 && content.ends_with("…\n\n[Open the inbox](https://app/x)"));
        let webex = "https://webexapis.com/v1/webhooks/incoming/abc";
        assert_eq!(chat_payload(webex, &a), json!({"markdown": markdown}));
        let long = "é".repeat(5000);
        let content = chat_payload(webex, &ChatAlert { quote: Some(&long), ..a });
        let content = content["markdown"].as_str().unwrap();
        assert!(content.len() <= 7439 && content.len() > 7400 && content.ends_with("…\n\n[Open the inbox](https://app/x)"));
        assert!(chat_payload("https://notdiscord.com/x", &a)["text"].is_string());
        let cliq = "https://cliq.zoho.eu/api/v2/channelsbyname/sales/message?zapikey=abc";
        let text = "Sarah replied to Acme\n\nSounds good\nTuesday?\n\n[Open the inbox](https://app/x)";
        assert_eq!(chat_payload(cliq, &a), json!({"text": text, "card": {"title": "Hot lead"}}));
        let long = "é".repeat(6000);
        let content = chat_payload(cliq, &ChatAlert { quote: Some(&long), ..a });
        let content = content["text"].as_str().unwrap();
        assert!(content.len() <= 10_000 && content.len() > 9_950 && content.ends_with("…\n\n[Open the inbox](https://app/x)"));
        let mattermost = "**Hot lead:** Sarah replied to Acme\n> Sounds good\n> Tuesday?\n[Open the inbox](https://app/x)";
        assert_eq!(chat_payload("https://chat.acme.dev/hooks/xjc7qbxb8jfgtmhtk1s5rwyeky", &a), json!({"text": mattermost}));
        assert_eq!(chat_payload("https://acme.dev/mattermost/hooks/xjc7qbxb8jfgtmhtk1s5rwyeky", &a)["text"], mattermost);
        assert_eq!(chat_payload("https://api.pumble.com/workspaces/6650f0c4/incomingWebhooks/postMessage/abc", &a), json!({"text": mattermost}));
        let gotify = chat_payload("https://push.acme.dev/gotify/message?token=AbCdEf", &a);
        assert_eq!(gotify["title"], "Hot lead");
        assert_eq!(gotify["message"], "Sarah replied to Acme  \n> Sounds good  \n> Tuesday?\n\n[Open the inbox](https://app/x)");
        assert_eq!(gotify["extras"]["client::notification"]["click"]["url"], "https://app/x");
        assert_eq!(gotify["priority"], 5);
        assert!(chat_payload("https://push.acme.dev/message", &a)["text"].is_string());
        assert_eq!(reason(br#"{"error":"Unauthorized","errorCode":401,"errorDescription":"you need to provide a valid access token"}"#).as_deref(), Some("you need to provide a valid access token"));
        // Rocket.Chat's /hooks/{id}/{token} and Zulip read Slack's format.
        assert_eq!(chat_payload("https://chat.acme.dev/hooks/66c9b2b8b6a2d5a1e8c1f0a2/AbCdEfGhIjKlMnOpQrStUvWxYz1234567890abcdefghij", &a)["text"], slack);
        assert_eq!(chat_payload("https://acme.zulipchat.com/api/v1/external/slack_incoming?api_key=k&stream=sales", &a)["text"], slack);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_due_reminder_is_announced_once_until_it_moves(db: sqlx::PgPool) {
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let lead = |ago: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, full_name, remind_at) values ($1, 'Sam Lee', now() - $2::interval) returning id")
                .bind(project)
                .bind(ago)
                .fetch_one(&db)
        };
        let due = lead("1 minute").await.unwrap();
        lead("2 days").await.unwrap();
        sqlx::query("insert into leads (project_id, full_name, remind_at) values ($1, 'Later', now() + interval '1 hour')")
            .bind(project)
            .execute(&db)
            .await
            .unwrap();
        let ids = |leads: Vec<Lead>| leads.into_iter().map(|l| l.id).collect::<Vec<_>>();

        assert_eq!(ids(claim_due_reminders(&db).await.unwrap()), [due]);
        assert!(claim_due_reminders(&db).await.unwrap().is_empty());
        // Moving the reminder to a later time that has also passed announces it again.
        sqlx::query("update leads set remind_at = now() where id = $1").bind(due).execute(&db).await.unwrap();
        assert_eq!(ids(claim_due_reminders(&db).await.unwrap()), [due]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_delivery_log_keeps_the_newest_fifty(db: sqlx::PgPool) {
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let mut first = None;
        for i in 0..DELIVERIES_KEPT + 2 {
            let result = if i == 0 { Err(anyhow::anyhow!("refused")) } else { Ok(()) };
            let d = log_delivery(&db, project, "hot_lead", "https://crm.example/hook", &json!({"n": i}), &result).await.unwrap();
            first.get_or_insert(d);
        }
        assert_eq!(first.unwrap().error.as_deref(), Some("refused"));
        let kept: Vec<i64> = sqlx::query_scalar("select (payload->>'n')::bigint from webhook_deliveries where project_id = $1 order by created_at")
            .bind(project)
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(kept, (2..DELIVERIES_KEPT + 2).collect::<Vec<_>>());
    }

    #[test]
    fn meeting_times_read_in_the_projects_timezone() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-10T14:00:00Z").unwrap().with_timezone(&chrono::Utc);
        assert_eq!(meeting_time(at, "Europe/Berlin"), "Tue 10 Nov at 15:00 CET");
        assert_eq!(meeting_time(at, "America/New_York"), "Tue 10 Nov at 09:00 EST");
        assert_eq!(meeting_time(at, "Not/AZone"), "Tue 10 Nov at 14:00 UTC");
    }

    #[test]
    fn only_public_addresses_pass() {
        for ip in ["127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:127.0.0.1"] {
            assert!(!is_public(ip.parse().unwrap()), "{ip} should be refused");
        }
        for ip in ["1.1.1.1", "52.1.2.3", "2606:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip} should be allowed");
        }
    }

    #[tokio::test]
    async fn internal_hosts_are_refused_before_connecting() {
        for url in ["http://127.0.0.1:8787/api", "http://localhost/hook", "http://[::1]/hook", "http://169.254.169.254/latest/meta-data"] {
            let err = post_json(url, &Value::Null, None).await.unwrap_err().to_string();
            assert!(err.ends_with("is not a public address"), "{url}: {err}");
        }
        assert!(post_json("ftp://example.com/x", &Value::Null, None).await.is_err());
    }

    #[tokio::test]
    async fn only_failures_that_may_pass_are_retried() {
        let status = |code: u16| -> anyhow::Error { Refused { status: reqwest::StatusCode::from_u16(code).unwrap(), reason: None }.into() };
        assert!(transient(&status(503)));
        assert!(transient(&status(429)));
        assert!(!transient(&status(404)));
        // Port 9 (discard) is closed on loopback, so this is a refused connection.
        let refused = reqwest::Client::new().get("http://127.0.0.1:9/").send().await.unwrap_err();
        assert!(transient(&refused.into()));
        assert!(!transient(&post_json("http://127.0.0.1/hook", &Value::Null, None).await.unwrap_err()));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_failed_post_marks_only_its_own_chat(db: sqlx::PgPool) {
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        for url in ["https://hooks.slack.com/services/T/B/x", "https://ntfy.sh/acme"] {
            sqlx::query("insert into chat_channels (project_id, url) values ($1, $2)").bind(project).bind(url).execute(&db).await.unwrap();
        }
        let errors = || {
            sqlx::query_as::<_, (String, Option<String>)>("select url, error from chat_channels order by url").fetch_all(&db)
        };
        record(&db, project, Alert::Chat, "https://ntfy.sh/acme", &Err(anyhow::anyhow!("404 Not Found"))).await;
        let failed = errors().await.unwrap();
        assert_eq!(failed[0], ("https://hooks.slack.com/services/T/B/x".to_string(), None));
        assert_eq!(failed[1], ("https://ntfy.sh/acme".to_string(), Some("404 Not Found".to_string())));
        record(&db, project, Alert::Chat, "https://ntfy.sh/acme", &Ok(())).await;
        assert!(errors().await.unwrap().iter().all(|(_, e)| e.is_none()));
    }

    #[test]
    fn a_refusal_says_why_in_the_apps_own_words() {
        // What each app answered a made-up URL with.
        let said = |body: &str| reason(body.as_bytes());
        assert_eq!(said(r#"{"message": "Unknown Webhook", "code": 10015}"#).as_deref(), Some("Unknown Webhook"));
        assert_eq!(said(r#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#).as_deref(), Some("Unauthorized"));
        assert_eq!(said("no_team").as_deref(), Some("no_team"));
        let google = r#"{"error": {"code": 400, "message": "API key not valid. Please pass a valid API key.", "status": "INVALID_ARGUMENT"}}"#;
        assert_eq!(said(google).as_deref(), Some("API key not valid. Please pass a valid API key."));
        let teams = r#"{"error":{"code":"WorkflowNotFound","message":"The workflow 'abc' could not be found."}}"#;
        assert_eq!(said(teams).as_deref(), Some("The workflow 'abc' could not be found."));
        assert_eq!(said(r#"{"token":"invalid","errors":["application token is invalid"],"status":0}"#).as_deref(), Some("application token is invalid"));
        assert_eq!(said(r#"{"code":40101,"http":401,"error":"unauthorized"}"#).as_deref(), Some("unauthorized"));
        assert_eq!(said("<html><body><h1>502 Bad Gateway</h1></body></html>"), None);
        assert_eq!(said(r#"{"ok": false}"#), None);
        assert_eq!(said("  \n "), None);
        assert_eq!(said(&"x".repeat(300)).map(|r| r.chars().count()), Some(201));
        let refused = |code: u16, body: &str| Refused::new(reqwest::StatusCode::from_u16(code).unwrap(), body.as_bytes()).to_string();
        assert_eq!(refused(404, r#"{"message": "Unknown Webhook", "code": 10015}"#), "404 Not Found: Unknown Webhook");
        assert_eq!(refused(401, r#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#), "401 Unauthorized");
    }

    #[test]
    fn signature_matches_a_receiver_computing_it_independently() {
        // Expected value from: printf '1700000000.{"a":1}' | openssl dgst -sha256 -hmac secret
        assert_eq!(sign("secret", "1700000000", br#"{"a":1}"#), "49f24e537407743fa4a0242bb63b94b9a47ee99cbbe071ccd8a22550ae411686");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_webhook_gets_only_the_events_it_subscribes_to(db: sqlx::PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(&db).await.unwrap();
        // Settings saved before webhook_events existed get the default: hot leads only.
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(&db).await.unwrap();
        for url in ["https://crm.example/hook", "https://hooks.zapier.example/1"] {
            sqlx::query("insert into webhook_endpoints (project_id, url) values ($1, $2)").bind(project).bind(url).execute(&db).await.unwrap();
        }
        let lead = sqlx::query_as::<_, Lead>("insert into leads (project_id, full_name, email) values ($1, 'Sarah Chen', 'sarah@northwind.example') returning *")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        let event = |name: &'static str| lead_event(&db, "https://app.test", project, name, &lead, None);

        let (urls, _, payload) = event("hot_lead").await.unwrap().unwrap();
        assert_eq!(urls, ["https://crm.example/hook", "https://hooks.zapier.example/1"]);
        assert_eq!(payload["event"], "hot_lead");
        assert_eq!(payload["project"]["name"], "Acme");
        assert_eq!(payload["lead"]["email"], "sarah@northwind.example");
        assert_eq!(payload["url"], format!("https://app.test/app/p/{project}/inbox?lead={}", lead.id));
        for name in ["lead_replied", "meeting_booked", "lead_unsubscribed", "lead_bounced", "email_sent"] {
            assert!(event(name).await.unwrap().is_none(), "{name}");
        }

        sqlx::query("update project_settings set webhook_events = '{lead_unsubscribed,lead_bounced}' where project_id = $1")
            .bind(project)
            .execute(&db)
            .await
            .unwrap();
        assert!(event("hot_lead").await.unwrap().is_none());
        assert_eq!(event("lead_bounced").await.unwrap().unwrap().2["reply"], Value::Null);

        sqlx::query("update project_settings set webhook_events = '{email_sent}' where project_id = $1").bind(project).execute(&db).await.unwrap();
        let sent = lead_event(&db, "https://app.test", project, "email_sent", &lead, Some(("email", json!({"subject": "Hi Sarah"})))).await.unwrap().unwrap().2;
        assert_eq!(sent["email"]["subject"], "Hi Sarah");
        assert_eq!(sent["reply"], Value::Null);

        sqlx::query("update project_settings set webhook_events = '{lead_bounced}' where project_id = $1").bind(project).execute(&db).await.unwrap();
        sqlx::query("delete from webhook_endpoints where project_id = $1").bind(project).execute(&db).await.unwrap();
        assert!(event("lead_bounced").await.unwrap().is_none());
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn record_keeps_the_last_failure_on_its_own_endpoint(db: sqlx::PgPool) {
        let org: uuid::Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: uuid::Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        for url in ["https://new.example/hook", "https://other.example/hook"] {
            sqlx::query("insert into webhook_endpoints (project_id, url) values ($1, $2)").bind(project).bind(url).execute(&db).await.unwrap();
        }
        let state = || async {
            sqlx::query_as::<_, (String, Option<String>, bool)>(
                "select url, error, failed_at is not null from webhook_endpoints where project_id = $1 order by url",
            )
            .bind(project)
            .fetch_all(&db)
            .await
            .unwrap()
        };
        let fine = |url: &str| (url.to_string(), None, false);

        // A delivery to a URL that was removed since changes nothing.
        record(&db, project, Alert::Webhook, "https://old.example/hook", &Err(anyhow::anyhow!("stale"))).await;
        assert_eq!(state().await, [fine("https://new.example/hook"), fine("https://other.example/hook")]);
        record(&db, project, Alert::Webhook, "https://new.example/hook", &Err(anyhow::anyhow!("HTTP 500"))).await;
        assert_eq!(state().await, [("https://new.example/hook".into(), Some("HTTP 500".into()), true), fine("https://other.example/hook")]);
        record(&db, project, Alert::Webhook, "https://new.example/hook", &Ok(())).await;
        assert_eq!(state().await, [fine("https://new.example/hook"), fine("https://other.example/hook")]);
    }
}
