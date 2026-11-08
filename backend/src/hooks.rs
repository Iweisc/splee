//! Posts JSON to the URLs users paste into project settings (the hot-lead webhook and Slack).
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
use serde_json::Value;
use sha2::Sha256;

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

#[derive(Clone, Copy)]
pub enum Alert {
    Webhook,
    Slack,
}

/// Saves how a hot-lead alert went, so the integrations page can show a broken endpoint: the error on failure, nothing on success.
/// Matching on the URL keeps a slow delivery to an old URL from marking a URL that was saved since.
pub async fn record(db: &sqlx::PgPool, project_id: uuid::Uuid, alert: Alert, url: &str, result: &anyhow::Result<()>) {
    let sql = match alert {
        Alert::Webhook => {
            "update project_settings set webhook_error = $3, webhook_failed_at = case when $3::text is null then null else now() end
             where project_id = $1 and webhook_url = $2"
        }
        Alert::Slack => {
            "update project_settings set slack_error = $3, slack_failed_at = case when $3::text is null then null else now() end
             where project_id = $1 and slack_webhook_url = $2"
        }
    };
    let error = result.as_ref().err().map(|e| format!("{e:#}").chars().take(500).collect::<String>());
    if let Err(e) = sqlx::query(sql).bind(project_id).bind(url).bind(error).execute(db).await {
        tracing::warn!(error = %e, "couldn't save the alert outcome");
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

    #[test]
    fn signature_matches_a_receiver_computing_it_independently() {
        // Expected value from: printf '1700000000.{"a":1}' | openssl dgst -sha256 -hmac secret
        assert_eq!(sign("secret", "1700000000", br#"{"a":1}"#), "49f24e537407743fa4a0242bb63b94b9a47ee99cbbe071ccd8a22550ae411686");
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
