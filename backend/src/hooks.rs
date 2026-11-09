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

use crate::models::Lead;
use crate::state::AppState;

pub async fn post_json(raw: &str, body: &Value, secret: Option<&str>) -> anyhow::Result<()> {
    let url = Url::parse(raw.trim())?;
    anyhow::ensure!(matches!(url.scheme(), "http" | "https"), "only http and https URLs are allowed");
    let host = url.host_str().context("the URL has no host")?.to_string();
    let port = url.port_or_known_default().context("the URL has no port")?;
    let addrs: Vec<SocketAddr> = match host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host.as_str(), port)).await?.collect(),
    };
    anyhow::ensure!(!addrs.is_empty() && addrs.iter().all(|a| is_public(a.ip())), "{host} is not a public address");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        // A proxy from HTTP(S)_PROXY would resolve the host itself and skip the check above.
        .no_proxy()
        .resolve_to_addrs(&host, &addrs)
        .build()?;
    // Serialized once so the signature covers exactly the bytes sent.
    let body = serde_json::to_vec(body)?;
    let mut request = client.post(url).header(CONTENT_TYPE, "application/json");
    if let Some(secret) = secret {
        let timestamp = chrono::Utc::now().timestamp().to_string();
        request = request.header("Splee-Timestamp", &timestamp).header("Splee-Signature", format!("v1={}", sign(secret, &timestamp, &body)));
    }
    request.body(body).send().await?.error_for_status()?;
    Ok(())
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
    e.downcast_ref::<reqwest::Error>().is_some_and(|e| {
        e.is_timeout() || e.is_connect() || e.status().is_some_and(|s| s.is_server_error() || s == reqwest::StatusCode::TOO_MANY_REQUESTS)
    })
}

#[derive(Clone, Copy)]
pub enum Alert {
    Webhook,
    Slack,
    /// Matched on {name}_token_enc, the stored ciphertext.
    Crm(crate::crm::Crm),
}

/// Saves how an alert went, so the integrations page can show a broken endpoint: the error on failure, nothing on success.
/// Matching on the URL keeps a slow delivery to an old URL from marking a URL that was saved since.
pub async fn record(db: &sqlx::PgPool, project_id: uuid::Uuid, alert: Alert, url: &str, result: &anyhow::Result<()>) {
    // The names are fixed strings, never user input.
    let (prefix, column) = match alert {
        Alert::Webhook => ("webhook", "webhook_url".to_string()),
        Alert::Slack => ("slack", "slack_webhook_url".to_string()),
        Alert::Crm(crm) => (crm.name(), format!("{}_token_enc", crm.name())),
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

/// The lead events a project's webhook can subscribe to. Keep in sync with EVENTS on the integrations page.
pub const EVENTS: [&str; 6] = ["hot_lead", "lead_replied", "meeting_booked", "lead_unsubscribed", "lead_bounced", "email_sent"];

/// What an event carries besides the lead: the reply that set it off, or the email Splee sent.
/// Its key is "reply" or "email", and "reply" stays in every payload, null when there's none.
pub type Detail = Option<(&'static str, Value)>;

/// The URL, secret and payload for a lead event, or None when the project's webhook doesn't want it.
async fn lead_event(
    db: &sqlx::PgPool,
    public_url: &str,
    project_id: Uuid,
    event: &str,
    lead: &Lead,
    detail: Detail,
) -> sqlx::Result<Option<(String, String, Value)>> {
    let settings: Option<(String, String, Vec<String>, String)> = sqlx::query_as(
        "select s.webhook_url, s.webhook_secret, s.webhook_events, p.name from project_settings s join projects p on p.id = s.project_id
         where s.project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await?;
    let Some((url, secret, events, name)) = settings else { return Ok(None) };
    if url.trim().is_empty() || !events.iter().any(|e| e == event) {
        return Ok(None);
    }
    // Keep in sync with PAYLOAD on the integrations page.
    let mut payload = json!({
        "event": event,
        "project": {"id": project_id, "name": name},
        "lead": {
            "id": lead.id, "full_name": lead.full_name, "email": lead.email, "title": lead.title,
            "company_name": lead.company_name, "company_domain": lead.company_domain,
            "linkedin_url": lead.linkedin_url, "x_url": lead.x_url,
        },
        "reply": null,
        "url": lead_url(public_url, project_id, lead.id),
    });
    if let Some((key, value)) = detail {
        payload[key] = value;
    }
    Ok(Some((url, secret, payload)))
}

/// Posts a lead event to the project's webhook when it subscribes to it. Delivery runs in the background, so a slow
/// receiver never holds up a run or a request. Failures are logged and shown on the integrations page.
pub async fn send_lead_event(state: &AppState, project_id: Uuid, event: &'static str, lead: &Lead, detail: Detail) {
    match lead_event(&state.db, &state.cfg.public_url, project_id, event, lead, detail).await {
        Ok(Some((url, secret, payload))) => {
            let db = state.db.clone();
            tokio::spawn(async move {
                let result = post_json_retrying(&url, &payload, Some(&secret)).await;
                if let Err(e) = &result {
                    tracing::warn!(error = %e, event, "webhook failed");
                }
                record(&db, project_id, Alert::Webhook, &url, &result).await;
            });
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, event, "couldn't load the webhook settings"),
    }
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

/// The JSON a chat webhook takes. Discord and Teams (Workflows and the older connectors) have their own shapes. Anything
/// else gets Slack's `{text}`, which Google Chat also reads, links included.
pub fn chat_payload(url: &str, a: &ChatAlert) -> Value {
    let host = Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
    let on = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    let quote = a.quote.map(|q| format!("\n> {}", q.replace('\n', "\n> "))).unwrap_or_default();
    if on("discord.com") || on("discordapp.com") {
        json!({"content": format!("**{}:** {}{quote}\n[{}]({})", a.label, a.summary, a.link_text, a.link)})
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
    } else {
        json!({"text": format!("*{}:* {}{quote}\n<{}|{}>", a.label, a.summary, a.link, a.link_text)})
    }
}

/// Posts an alert to the project's chat webhook (slack_webhook_url, which also takes Discord, Teams and Google Chat) when
/// it has one, and records how it went for the integrations page.
pub async fn post_chat(db: &sqlx::PgPool, project_id: Uuid, alert: &ChatAlert<'_>) {
    let url: String = sqlx::query_scalar("select slack_webhook_url from project_settings where project_id = $1")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    if url.trim().is_empty() {
        return;
    }
    let result = post_json_retrying(&url, &chat_payload(&url, alert), None).await;
    if let Err(e) = &result {
        tracing::warn!(error = %e, "chat notification failed");
    }
    record(db, project_id, Alert::Slack, &url, &result).await;
}

/// Whether an update moved the lead to meeting_booked. `before` is its status before the update.
pub fn booked(before: &str, lead: &Lead) -> bool {
    lead.status == "meeting_booked" && before != "meeting_booked"
}

/// Tells the team in chat and HubSpot that a lead booked a meeting. It runs in the background, so the request that set
/// the status doesn't wait on either.
pub fn announce_booked(state: &AppState, lead: &Lead) {
    let (state, lead, project_id) = (state.clone(), lead.clone(), lead.project_id);
    let (who, link) = (format!("{} ({})", lead.full_name, lead.company_name), lead_url(&state.cfg.public_url, project_id, lead.id));
    tokio::spawn(async move {
        let project: String =
            sqlx::query_scalar("select name from projects where id = $1").bind(project_id).fetch_one(&state.db).await.unwrap_or_default();
        let summary = format!("{who} for {project}");
        let alert = ChatAlert { label: "Meeting booked", summary: &summary, quote: None, link: &link, link_text: "Open the conversation" };
        post_chat(&state.db, project_id, &alert).await;
        crate::crm::sync(&state, project_id, &lead, &format!("Booked a meeting through Splee.\n{link}")).await;
    });
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
        // A lookalike host isn't Discord.
        assert!(chat_payload("https://notdiscord.com/x", &a)["text"].is_string());
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
        let status = |code: u16| -> anyhow::Error {
            let response = axum::http::Response::builder().status(code).body("").unwrap();
            reqwest::Response::from(response).error_for_status().unwrap_err().into()
        };
        assert!(transient(&status(503)));
        assert!(transient(&status(429)));
        assert!(!transient(&status(404)));
        // Port 9 (discard) is closed on loopback, so this is a refused connection.
        let refused = reqwest::Client::new().get("http://127.0.0.1:9/").send().await.unwrap_err();
        assert!(transient(&refused.into()));
        assert!(!transient(&post_json("http://127.0.0.1/hook", &Value::Null, None).await.unwrap_err()));
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
        sqlx::query("insert into project_settings (project_id, webhook_url) values ($1, 'https://crm.example/hook')").bind(project).execute(&db).await.unwrap();
        let lead = sqlx::query_as::<_, Lead>("insert into leads (project_id, full_name, email) values ($1, 'Sarah Chen', 'sarah@northwind.example') returning *")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        let event = |name: &'static str| lead_event(&db, "https://app.test", project, name, &lead, None);

        let (url, _, payload) = event("hot_lead").await.unwrap().unwrap();
        assert_eq!(url, "https://crm.example/hook");
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

        sqlx::query("update project_settings set webhook_url = '' where project_id = $1").bind(project).execute(&db).await.unwrap();
        assert!(event("lead_bounced").await.unwrap().is_none());
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn record_keeps_the_last_failure_for_the_saved_url(db: sqlx::PgPool) {
        let org: uuid::Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: uuid::Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        sqlx::query("insert into project_settings (project_id, webhook_url) values ($1, 'https://new.example/hook')").bind(project).execute(&db).await.unwrap();
        let state = || async {
            sqlx::query_as::<_, (Option<String>, bool)>("select webhook_error, webhook_failed_at is not null from project_settings where project_id = $1")
                .bind(project)
                .fetch_one(&db)
                .await
                .unwrap()
        };

        record(&db, project, Alert::Webhook, "https://old.example/hook", &Err(anyhow::anyhow!("stale"))).await;
        assert_eq!(state().await, (None, false));
        record(&db, project, Alert::Webhook, "https://new.example/hook", &Err(anyhow::anyhow!("HTTP 500"))).await;
        assert_eq!(state().await, (Some("HTTP 500".into()), true));
        record(&db, project, Alert::Webhook, "https://new.example/hook", &Ok(())).await;
        assert_eq!(state().await, (None, false));
    }
}
