//! Google Postmaster Tools: Gmail's own view of a domain, for the domains the user verified there. Connect Google Postmaster
//! signs in through the Google app mailboxes use (routes/deliverability.rs), and each sync pulls the last 30 days of domain
//! reputation, user-reported spam rate and SPF, DKIM and DMARC pass rates into postmaster_days.

use std::time::Duration;

use anyhow::{Context, anyhow};
use chrono::{Datelike, NaiveDate, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::health::PostmasterDay;
use crate::crypto;
use crate::mail::oauth::{self, TokenError};
use crate::models::Ts;
use crate::state::AppState;

pub const SCOPE: &str = "https://www.googleapis.com/auth/postmaster.readonly openid email";
pub const API_URL: &str = "https://gmailpostmastertools.googleapis.com";
const DAYS: u64 = 30;
pub const SIGNED_OUT: &str = "Google signed Splee out of Postmaster Tools. Connect it again to keep Gmail's numbers coming.";

pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    /// `base` is Config::postmaster_api_url: empty for Google's.
    pub fn new(base: &str, token: &str) -> anyhow::Result<Self> {
        let base = if base.is_empty() { API_URL } else { base };
        let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?;
        Ok(Self { http, base: base.trim_end_matches('/').to_string(), token: token.to_string() })
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> anyhow::Result<Value> {
        let url = reqwest::Url::parse_with_params(&format!("{}{path}", self.base), query)?;
        let res = self.http.get(url).bearer_auth(&self.token).send().await.map_err(reqwest::Error::without_url)?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        if status.is_success() {
            return Ok(body);
        }
        let said = body.pointer("/error/message").and_then(Value::as_str).unwrap_or(status.as_str());
        Err(anyhow!("Postmaster Tools answered {}: {said}", status.as_u16()))
    }

    /// Every page of a list, following nextPageToken.
    async fn all(&self, path: &str, query: &[(&str, String)], key: &str) -> anyhow::Result<Vec<Value>> {
        let mut items = vec![];
        let mut page = String::new();
        // Google pages 30 days of one domain in one go. The cap only stops a server that keeps answering with more.
        for _ in 0..20 {
            let mut query = query.to_vec();
            if !page.is_empty() {
                query.push(("pageToken", page.clone()));
            }
            let body = self.get(path, &query).await?;
            items.extend(body[key].as_array().cloned().unwrap_or_default());
            match body["nextPageToken"].as_str().filter(|t| !t.is_empty()) {
                Some(next) => page = next.to_string(),
                None => break,
            }
        }
        Ok(items)
    }

    /// The domains the signed-in account verified in Postmaster Tools.
    pub async fn domains(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.all("/v1/domains", &[("pageSize", "100".into())], "domains").await?.iter().filter_map(domain_name).collect())
    }

    /// One domain's days from `from` up to, not including, `to`.
    pub async fn days(&self, domain: &str, from: NaiveDate, to: NaiveDate) -> anyhow::Result<Vec<PostmasterDay>> {
        let date = |prefix: &str, d: NaiveDate| {
            [(format!("{prefix}.year"), d.year().to_string()), (format!("{prefix}.month"), d.month().to_string()), (format!("{prefix}.day"), d.day().to_string())]
        };
        let pairs: Vec<(String, String)> = date("startDate", from).into_iter().chain(date("endDate", to)).collect();
        let mut query: Vec<(&str, String)> = pairs.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        query.push(("pageSize", "100".into()));
        let stats = self.all(&format!("/v1/domains/{domain}/trafficStats"), &query, "trafficStats").await?;
        Ok(stats.iter().filter_map(|s| day(domain, s)).collect())
    }
}

/// "example.com" from {"name": "domains/example.com"}.
fn domain_name(d: &Value) -> Option<String> {
    Some(d["name"].as_str()?.strip_prefix("domains/")?.to_lowercase())
}

/// A trafficStats entry, named "domains/example.com/trafficStats/20261110". Google leaves out a number it has too little
/// traffic for, and says REPUTATION_CATEGORY_UNSPECIFIED for no reputation.
fn day(domain: &str, s: &Value) -> Option<PostmasterDay> {
    let date = s["name"].as_str()?.rsplit('/').next()?;
    let day = NaiveDate::parse_from_str(date, "%Y%m%d").ok()?;
    let reputation = s["domainReputation"].as_str().filter(|r| matches!(*r, "HIGH" | "MEDIUM" | "LOW" | "BAD")).unwrap_or_default();
    let ratio = |key: &str| s[key].as_f64().filter(|r| (0.0..=1.0).contains(r));
    Some(PostmasterDay {
        domain: domain.to_string(),
        day,
        reputation: reputation.to_string(),
        spam_rate: ratio("userReportedSpamRatio"),
        spf_rate: ratio("spfSuccessRatio"),
        dkim_rate: ratio("dkimSuccessRatio"),
        dmarc_rate: ratio("dmarcSuccessRatio"),
    })
}

/// An access token for the org's Postmaster sign-in: the stored one while it has a minute left, else a new one. A refused
/// refresh token is blanked, so nothing asks Google with it again.
async fn access_token(state: &AppState, org_id: Uuid) -> anyhow::Result<String> {
    let (refresh_enc, access_enc, expires_at): (String, String, Option<Ts>) =
        sqlx::query_as("select refresh_token_enc, access_token_enc, expires_at from postmaster_connections where org_id = $1")
            .bind(org_id)
            .fetch_optional(&state.db)
            .await?
            .context("Postmaster Tools isn't connected")?;
    anyhow::ensure!(!refresh_enc.is_empty(), SIGNED_OUT);
    let secret = &state.cfg.app_secret;
    if !access_enc.is_empty() && expires_at.is_some_and(|at| at > Utc::now() + chrono::Duration::seconds(60)) {
        return crypto::decrypt(secret, &access_enc);
    }
    let app = state.cfg.google.as_ref().context("Google sign-in isn't set up on this Splee anymore")?;
    let refresh = crypto::decrypt(secret, &refresh_enc)?;
    match oauth::token_request("Google", app, &[("grant_type", "refresh_token"), ("refresh_token", &refresh)]).await {
        Ok(tokens) => {
            sqlx::query("update postmaster_connections set access_token_enc = $2, expires_at = $3 where org_id = $1 and refresh_token_enc = $4")
                .bind(org_id)
                .bind(crypto::encrypt(secret, &tokens.access_token))
                .bind(tokens.expires_at())
                .bind(&refresh_enc)
                .execute(&state.db)
                .await?;
            Ok(tokens.access_token)
        }
        Err(TokenError::Revoked(_)) => {
            sqlx::query("update postmaster_connections set refresh_token_enc = '', access_token_enc = '', expires_at = null where org_id = $1 and refresh_token_enc = $2")
                .bind(org_id)
                .bind(&refresh_enc)
                .execute(&state.db)
                .await?;
            Err(anyhow!(SIGNED_OUT))
        }
        Err(e) => Err(e.into()),
    }
}

/// Pulls the last 30 days for each domain verified in Postmaster Tools that the org sends from, and saves how it went.
pub async fn sync(state: &AppState, org_id: Uuid) -> anyhow::Result<()> {
    let result = pull(state, org_id).await;
    let error = result.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
    sqlx::query("update postmaster_connections set error = $2, synced_at = now() where org_id = $1").bind(org_id).bind(&error).execute(&state.db).await?;
    result
}

async fn pull(state: &AppState, org_id: Uuid) -> anyhow::Result<()> {
    let client = Client::new(&state.cfg.postmaster_api_url, &access_token(state, org_id).await?)?;
    let verified = client.domains().await?;
    sqlx::query("update postmaster_connections set domains = $2 where org_id = $1").bind(org_id).bind(&verified).execute(&state.db).await?;
    let ours = super::org_domains(&state.db, org_id).await?;
    let today = Utc::now().date_naive();
    let from = today - chrono::Days::new(DAYS);
    for domain in verified.iter().filter(|d| ours.contains(d)) {
        for d in client.days(domain, from, today + chrono::Days::new(1)).await? {
            sqlx::query(
                "insert into postmaster_days (org_id, domain, day, reputation, spam_rate, spf_rate, dkim_rate, dmarc_rate)
                 values ($1, $2, $3, $4, $5, $6, $7, $8)
                 on conflict (org_id, domain, day) do update set reputation = excluded.reputation, spam_rate = excluded.spam_rate,
                   spf_rate = excluded.spf_rate, dkim_rate = excluded.dkim_rate, dmarc_rate = excluded.dmarc_rate",
            )
            .bind(org_id)
            .bind(&d.domain)
            .bind(d.day)
            .bind(&d.reputation)
            .bind(d.spam_rate)
            .bind(d.spf_rate)
            .bind(d.dkim_rate)
            .bind(d.dmarc_rate)
            .execute(&state.db)
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;

    /// Postmaster Tools for one account with try-acme.com and other.test verified. try-acme.com's days come in two pages.
    /// Every request's path and query is kept, and a token other than `pm-token` gets a 401.
    pub(crate) async fn fake_postmaster() -> (String, Arc<Mutex<Vec<String>>>) {
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        async fn domains(State(seen): State<Arc<Mutex<Vec<String>>>>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
            seen.lock().unwrap().push("/v1/domains".into());
            if headers.get("authorization").and_then(|h| h.to_str().ok()) != Some("Bearer pm-token") {
                return (StatusCode::UNAUTHORIZED, Json(json!({"error": {"code": 401, "message": "Request had invalid authentication credentials."}})));
            }
            (StatusCode::OK, Json(json!({"domains": [{"name": "domains/Try-Acme.com", "permission": "OWNER"}, {"name": "domains/other.test"}]})))
        }
        async fn stats(
            State(seen): State<Arc<Mutex<Vec<String>>>>,
            Path(domain): Path<String>,
            Query(q): Query<std::collections::HashMap<String, String>>,
        ) -> Json<Value> {
            let mut keys: Vec<String> = q.iter().map(|(k, v)| format!("{k}={v}")).collect();
            keys.sort();
            seen.lock().unwrap().push(format!("/v1/domains/{domain}/trafficStats?{}", keys.join("&")));
            if q.get("pageToken").map(String::as_str) == Some("p2") {
                return Json(json!({"trafficStats": [{"name": format!("domains/{domain}/trafficStats/20261110"), "domainReputation": "LOW",
                    "userReportedSpamRatio": 0.0035, "spfSuccessRatio": 1.0, "dkimSuccessRatio": 0.91, "dmarcSuccessRatio": 0.9}]}));
            }
            Json(json!({"trafficStats": [
                {"name": format!("domains/{domain}/trafficStats/20261109"), "domainReputation": "MEDIUM", "spfSuccessRatio": 0.99},
                {"name": format!("domains/{domain}/trafficStats/20261108"), "domainReputation": "REPUTATION_CATEGORY_UNSPECIFIED", "userReportedSpamRatio": 7.0},
                {"name": "not a day"}
            ], "nextPageToken": "p2"}))
        }
        let app = Router::new()
            .route("/v1/domains", get(domains))
            .route("/v1/domains/{domain}/trafficStats", get(stats))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        (base, seen)
    }

    #[tokio::test]
    async fn days_are_read_from_every_page() {
        let (base, seen) = fake_postmaster().await;
        let client = Client::new(&base, "pm-token").unwrap();
        assert_eq!(client.domains().await.unwrap(), vec!["try-acme.com", "other.test"]);
        let from = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
        let days = client.days("try-acme.com", from, NaiveDate::from_ymd_opt(2026, 11, 12).unwrap()).await.unwrap();
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        assert_eq!(
            days.iter().map(|d| (d.day, d.reputation.as_str(), d.spam_rate, d.dkim_rate)).collect::<Vec<_>>(),
            vec![(day(9), "MEDIUM", None, None), (day(8), "", None, None), (day(10), "LOW", Some(0.0035), Some(0.91))]
        );
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[1],
            "/v1/domains/try-acme.com/trafficStats?endDate.day=12&endDate.month=11&endDate.year=2026&pageSize=100&startDate.day=12&startDate.month=10&startDate.year=2026"
        );
        assert!(seen[2].contains("pageToken=p2"), "{}", seen[2]);
    }

    #[tokio::test]
    async fn a_refused_token_says_what_google_said() {
        let (base, _) = fake_postmaster().await;
        let e = Client::new(&base, "stale-token").unwrap().domains().await.unwrap_err();
        assert_eq!(e.to_string(), "Postmaster Tools answered 401: Request had invalid authentication credentials.");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_sync_keeps_days_for_the_orgs_own_verified_domains(db: PgPool) {
        let (base, _) = fake_postmaster().await;
        let fake = crate::mail::oauth::tests::FakeProvider::start().await;
        let mut cfg = crate::mail::oauth::tests::config(&fake);
        cfg.postmaster_api_url = base;
        let state = AppState { db: db.clone(), cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(1).0 };
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, 'ana@try-acme.com', 'smtp.test', 'ana', 'x')")
            .bind(org)
            .execute(&db)
            .await
            .unwrap();
        let secret = &state.cfg.app_secret;
        sqlx::query(
            "insert into postmaster_connections (org_id, email, refresh_token_enc, access_token_enc, expires_at) values ($1, 'ana@try-acme.com', $2, $3, now() + interval '1 hour')",
        )
        .bind(org)
        .bind(crypto::encrypt(secret, "refresh"))
        .bind(crypto::encrypt(secret, "pm-token"))
        .execute(&db)
        .await
        .unwrap();

        sync(&state, org).await.unwrap();
        let days: Vec<(String, String)> = sqlx::query_as("select domain, reputation from postmaster_days where org_id = $1 order by day").bind(org).fetch_all(&db).await.unwrap();
        // other.test is verified but nobody here sends from it.
        assert_eq!(days, vec![("try-acme.com".into(), "".into()), ("try-acme.com".into(), "MEDIUM".into()), ("try-acme.com".into(), "LOW".into())]);
        let (domains, error): (Vec<String>, String) =
            sqlx::query_as("select domains, error from postmaster_connections where org_id = $1").bind(org).fetch_one(&db).await.unwrap();
        assert_eq!((domains, error), (vec!["try-acme.com".to_string(), "other.test".to_string()], String::new()));

        // An expired token is renewed with the Google app. The fake provider hands out tokens Postmaster doesn't take, and the
        // error is kept for the page.
        sqlx::query("update postmaster_connections set expires_at = now() where org_id = $1").bind(org).execute(&db).await.unwrap();
        assert!(sync(&state, org).await.is_err());
        assert_eq!(fake.hits(), 1);
        let error: String = sqlx::query_scalar("select error from postmaster_connections where org_id = $1").bind(org).fetch_one(&db).await.unwrap();
        assert!(error.starts_with("Postmaster Tools answered 401"), "{error}");

        // A revoked sign-in is blanked and says to connect again.
        sqlx::query("update postmaster_connections set expires_at = now(), refresh_token_enc = $2 where org_id = $1")
            .bind(org)
            .bind(crypto::encrypt(secret, "revoked"))
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(sync(&state, org).await.unwrap_err().to_string(), SIGNED_OUT);
        let refresh: String = sqlx::query_scalar("select refresh_token_enc from postmaster_connections where org_id = $1").bind(org).fetch_one(&db).await.unwrap();
        assert_eq!(refresh, "");
    }
}
