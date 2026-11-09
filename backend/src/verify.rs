//! Checks a lead's address with an email verification service before its first cold email, so addresses that don't
//! exist fail in the app instead of bouncing and hurting the mailbox's reputation. NeverBounce, MillionVerifier, Kickbox and
//! Reoon take the key in the URL, so every error drops its URL before it's logged or shown. Hunter, Bouncer, Emailable and
//! Clearout take it in a header.

use std::time::Duration;

use reqwest::Url;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::crypto;
use crate::hooks::{self, Alert};
use crate::models::Lead;
use crate::state::AppState;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verdict {
    Valid,
    /// Doesn't exist, or a throwaway inbox.
    Invalid,
    /// A catch-all domain or a server that didn't answer. The email goes out as it would without a verifier.
    Unsure,
}

#[derive(Clone, Copy, Debug)]
// MillionVerifier is the service's name.
#[allow(clippy::enum_variant_names)]
pub enum Verifier {
    NeverBounce,
    MillionVerifier,
    Hunter,
    Kickbox,
    Bouncer,
    Emailable,
    Clearout,
    Reoon,
}

impl Verifier {
    pub fn parse(name: &str) -> Option<Verifier> {
        match name {
            "neverbounce" => Some(Verifier::NeverBounce),
            "millionverifier" => Some(Verifier::MillionVerifier),
            "hunter" => Some(Verifier::Hunter),
            "kickbox" => Some(Verifier::Kickbox),
            "bouncer" => Some(Verifier::Bouncer),
            "emailable" => Some(Verifier::Emailable),
            "clearout" => Some(Verifier::Clearout),
            "reoon" => Some(Verifier::Reoon),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Verifier::NeverBounce => "NeverBounce",
            Verifier::MillionVerifier => "MillionVerifier",
            Verifier::Hunter => "Hunter",
            Verifier::Kickbox => "Kickbox",
            Verifier::Bouncer => "Bouncer",
            Verifier::Emailable => "Emailable",
            Verifier::Clearout => "Clearout",
            Verifier::Reoon => "Reoon",
        }
    }

    /// Checks the key works, before it's saved.
    pub async fn check_key(self, key: &str) -> anyhow::Result<()> {
        match self {
            Verifier::NeverBounce => neverbounce_ok(&get(Url::parse_with_params("https://api.neverbounce.com/v4.2/account/info", [("key", key)])?).await?),
            Verifier::MillionVerifier => {
                millionverifier_ok(&get(Url::parse_with_params("https://api.millionverifier.com/api/v3/credits", [("api", key)])?).await?)
            }
            Verifier::Hunter => hunter_get(Url::parse("https://api.hunter.io/v2/account")?, key).await.map(|_| ()),
            // The balance costs no credit, unlike a verification.
            Verifier::Kickbox => kickbox_get(Url::parse_with_params("https://api.kickbox.com/v2/balance", [("apikey", key)])?).await.map(|_| ()),
            // Like Kickbox's balance, the credits cost nothing.
            Verifier::Bouncer => bouncer_get(Url::parse("https://api.usebouncer.com/v1.1/credits")?, key).await.map(|_| ()),
            Verifier::Emailable => emailable_get(Url::parse("https://api.emailable.com/v1/account")?, key).await.map(|_| ()),
            Verifier::Clearout => clearout_call(client()?.get("https://api.clearout.io/v2/email_verify/getcredits"), key).await.map(|_| ()),
            Verifier::Reoon => {
                reoon_get(Url::parse_with_params("https://emailverifier.reoon.com/api/v1/check-account-balance/", [("key", key)])?).await.map(|_| ())
            }
        }
    }

    async fn verify(self, key: &str, email: &str) -> anyhow::Result<Verdict> {
        match self {
            Verifier::NeverBounce => {
                let url = Url::parse_with_params("https://api.neverbounce.com/v4.2/single/check", [("key", key), ("email", email), ("timeout", "10")])?;
                let body = get(url).await?;
                neverbounce_ok(&body)?;
                Ok(neverbounce_verdict(body["result"].as_str().unwrap_or_default()))
            }
            Verifier::MillionVerifier => {
                let url = Url::parse_with_params("https://api.millionverifier.com/api/v3/", [("api", key), ("email", email), ("timeout", "10")])?;
                let body = get(url).await?;
                millionverifier_ok(&body)?;
                Ok(millionverifier_verdict(body["result"].as_str().unwrap_or_default()))
            }
            Verifier::Hunter => {
                let body = hunter_get(Url::parse_with_params("https://api.hunter.io/v2/email-verifier", [("email", email)])?, key).await?;
                Ok(hunter_verdict(body["data"]["status"].as_str().unwrap_or_default()))
            }
            Verifier::Kickbox => {
                let url = Url::parse_with_params("https://api.kickbox.com/v2/verify", [("apikey", key), ("email", email), ("timeout", "10000")])?;
                Ok(kickbox_verdict(&kickbox_get(url).await?))
            }
            Verifier::Bouncer => {
                let url = Url::parse_with_params("https://api.usebouncer.com/v1.1/email/verify", [("email", email), ("timeout", "10")])?;
                Ok(bouncer_verdict(&bouncer_get(url, key).await?))
            }
            Verifier::Emailable => {
                let url = Url::parse_with_params("https://api.emailable.com/v1/verify", [("email", email), ("timeout", "10")])?;
                Ok(emailable_verdict(&emailable_get(url, key).await?))
            }
            Verifier::Clearout => {
                let req = client()?.post("https://api.clearout.io/v2/email_verify/instant").json(&json!({ "email": email, "timeout": 10000 }));
                Ok(clearout_verdict(&clearout_call(req, key).await?))
            }
            Verifier::Reoon => {
                // Quick mode doesn't ask the inbox and calls an address that doesn't exist valid, so this is power mode,
                // which can take over a minute. One that runs past the client's wait sends like a catch-all.
                let url = Url::parse_with_params("https://emailverifier.reoon.com/api/v1/verify", [("email", email), ("key", key), ("mode", "power")])?;
                match reoon_get(url).await {
                    Err(e) if e.downcast_ref::<reqwest::Error>().is_some_and(reqwest::Error::is_timeout) => Ok(Verdict::Unsure),
                    body => Ok(reoon_verdict(body?["status"].as_str().unwrap_or_default())),
                }
            }
        }
    }
}

/// The project's verdict on `email`, or None when it has no verifier or the service failed. A failure is logged and shown
/// on the integrations page, and the email goes out unverified rather than waiting on the service. A success clears the
/// shown failure unless `keep_error`, set when a lookup just before it failed.
pub async fn verify(state: &AppState, project_id: Uuid, email: &str, keep_error: bool) -> Option<Verdict> {
    let row: Option<(String, String)> = sqlx::query_as("select verifier, verifier_key_enc from project_settings where project_id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let (name, enc) = row?;
    let verifier = Verifier::parse(&name)?;
    let result = match crypto::decrypt(&state.cfg.app_secret, &enc) {
        Ok(key) => verifier.verify(&key, email).await,
        Err(e) => Err(e),
    };
    if !(keep_error && result.is_ok()) {
        hooks::record(&state.db, project_id, Alert::Verifier, &enc, &result.as_ref().map(|_| ()).map_err(|e| anyhow::anyhow!("{e:#}"))).await;
    }
    match result {
        Ok(verdict) => Some(verdict),
        Err(e) => {
            tracing::warn!(error = %e, verifier = verifier.label(), "email verification failed");
            None
        }
    }
}

/// The project's Hunter API key, or None when its verifier isn't Hunter.
pub async fn hunter_key(state: &AppState, project_id: Uuid) -> anyhow::Result<Option<String>> {
    let row: Option<(String, String)> = sqlx::query_as("select verifier, verifier_key_enc from project_settings where project_id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?;
    match row.filter(|(name, _)| matches!(Verifier::parse(name), Some(Verifier::Hunter))) {
        Some((_, enc)) => Ok(Some(crypto::decrypt(&state.cfg.app_secret, &enc)?)),
        None => Ok(None),
    }
}

/// An address Hunter found for a lead, to replace a guessed one.
#[derive(Debug, PartialEq)]
pub struct Found {
    pub email: String,
    /// Hunter also verified it, so it needn't be checked again.
    pub verified: bool,
}

pub enum Lookup {
    Found(Found),
    /// Not Hunter, or nothing Hunter is confident in.
    Nothing,
    /// Recorded like a failed check.
    Failed,
}

/// With Hunter connected, looks up the lead's real address at the guessed address's domain.
pub async fn find(state: &AppState, project_id: Uuid, lead: &Lead, guessed: &str) -> Lookup {
    let row: Option<(String, String)> = sqlx::query_as("select verifier, verifier_key_enc from project_settings where project_id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some((_, enc)) = row.filter(|(name, _)| matches!(Verifier::parse(name), Some(Verifier::Hunter))) else { return Lookup::Nothing };
    let Some((_, domain)) = guessed.rsplit_once('@') else { return Lookup::Nothing };
    let domain = domain.to_lowercase();
    let (first, last) = if lead.first_name.is_empty() {
        lead.full_name.trim().split_once(' ').unwrap_or((lead.full_name.trim(), ""))
    } else {
        (lead.first_name.as_str(), lead.last_name.as_str())
    };
    let result = match crypto::decrypt(&state.cfg.app_secret, &enc) {
        Ok(key) => match Url::parse_with_params(
            "https://api.hunter.io/v2/email-finder",
            [("domain", domain.as_str()), ("first_name", first.trim()), ("last_name", last.trim())],
        ) {
            Ok(url) => hunter_get(url, &key).await,
            Err(e) => Err(e.into()),
        },
        Err(e) => Err(e),
    };
    match result {
        Ok(body) => hunter_found(&body, &domain).map_or(Lookup::Nothing, Lookup::Found),
        Err(e) => {
            tracing::warn!(error = %e, "hunter email finder failed");
            hooks::record(&state.db, project_id, Alert::Verifier, &enc, &Err(anyhow::anyhow!("Email finder: {e:#}"))).await;
            Lookup::Failed
        }
    }
}

/// Hunter's answer, kept only at the same domain and when it's verified or Hunter is fairly sure (score 0 to 100), so a
/// lead is never pointed at someone at another company.
fn hunter_found(body: &Value, domain: &str) -> Option<Found> {
    let data = &body["data"];
    let email = data["email"].as_str()?.trim().to_lowercase();
    let verified = data["verification"]["status"] == "valid";
    let sure = verified || data["score"].as_i64().unwrap_or(0) >= 80;
    (email.rsplit_once('@').is_some_and(|(_, d)| d == domain) && sure).then_some(Found { email, verified })
}

async fn get(url: Url) -> anyhow::Result<Value> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let res = client.get(url).send().await.map_err(reqwest::Error::without_url)?;
    Ok(res.error_for_status().map_err(reqwest::Error::without_url)?.json().await.map_err(reqwest::Error::without_url)?)
}

/// NeverBounce answers 200 with a status. Its auth_failure message quotes the key, so that one gets a fixed message.
fn neverbounce_ok(body: &Value) -> anyhow::Result<()> {
    match body["status"].as_str() {
        Some("success") => Ok(()),
        Some("auth_failure") => anyhow::bail!("the API key was rejected"),
        status => anyhow::bail!("{}", body["message"].as_str().or(status).unwrap_or("unexpected response")),
    }
}

fn neverbounce_verdict(result: &str) -> Verdict {
    match result {
        "valid" => Verdict::Valid,
        "invalid" | "disposable" => Verdict::Invalid,
        _ => Verdict::Unsure,
    }
}

/// MillionVerifier answers 200 with an `error` string, like "Apikey not found" or "Insufficient credits".
fn millionverifier_ok(body: &Value) -> anyhow::Result<()> {
    let error = body["error"].as_str().unwrap_or_default();
    anyhow::ensure!(error.is_empty() && body["result"] != "error", "{}", if error.is_empty() { "unexpected response" } else { error });
    Ok(())
}

/// Hunter's errors come as `{"errors": [{"details": ...}]}`. A 202 means it's still checking, and the body has no status,
/// which reads as Unsure.
pub async fn hunter_get(url: Url, key: &str) -> anyhow::Result<Value> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let res = client.get(url).header("X-API-KEY", key).send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["errors"][0]["details"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// webmail is a Gmail or Outlook address Hunter doesn't check further, so it sends like a catch-all.
fn hunter_verdict(status: &str) -> Verdict {
    match status {
        "valid" => Verdict::Valid,
        "invalid" | "disposable" => Verdict::Invalid,
        _ => Verdict::Unsure,
    }
}

/// Kickbox answers a bad key or an empty balance with a 4xx and `{"success": false, "message": ...}`, and an unknown path
/// with `"success": "false"`. The message doesn't quote the key.
async fn kickbox_get(url: Url) -> anyhow::Result<Value> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let res = client.get(url).send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success() && body["success"] != false && body["success"] != "false", "{}", body["message"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// Kickbox calls a disposable address risky, but like the other services it counts as invalid here.
fn kickbox_verdict(body: &Value) -> Verdict {
    match body["result"].as_str() {
        _ if body["disposable"] == true => Verdict::Invalid,
        Some("deliverable") => Verdict::Valid,
        Some("undeliverable") => Verdict::Invalid,
        _ => Verdict::Unsure,
    }
}

/// Bouncer's errors come as `{"message": ...}`, like "user not found" for a bad key or "0 credits left".
async fn bouncer_get(url: Url, key: &str) -> anyhow::Result<Value> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let res = client.get(url).header("x-api-key", key).send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["message"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// Bouncer calls a disposable address risky, but like the other services it counts as invalid here.
fn bouncer_verdict(body: &Value) -> Verdict {
    match body["status"].as_str() {
        _ if body["domain"]["disposable"] == "yes" => Verdict::Invalid,
        Some("deliverable") => Verdict::Valid,
        Some("undeliverable") => Verdict::Invalid,
        _ => Verdict::Unsure,
    }
}

/// Emailable's errors come as `{"message": ...}`, like "Invalid API key or access token." A verification that runs past
/// the timeout answers 249 with only a message, which has no state and so reads as unsure.
async fn emailable_get(url: Url, key: &str) -> anyhow::Result<Value> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let res = client.get(url).bearer_auth(key).send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["message"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// Emailable calls a disposable address risky, but like the other services it counts as invalid here.
fn emailable_verdict(body: &Value) -> Verdict {
    match body["state"].as_str() {
        _ if body["disposable"] == true => Verdict::Invalid,
        Some("deliverable") => Verdict::Valid,
        Some("undeliverable") => Verdict::Invalid,
        _ => Verdict::Unsure,
    }
}

fn client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder().timeout(Duration::from_secs(30)).build()
}

/// Clearout's errors come as `{"status": "failed", "error": {"message": ...}}`, like "Invalid API Token, please generate new
/// token". A deep check that runs past the timeout answers 524 and goes on in the background, so it reads as unsure.
async fn clearout_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = req.bearer_auth(key).send().await?;
    let status = res.status();
    if status.as_u16() == 524 {
        return Ok(Value::Null);
    }
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success() && body["status"] != "failed", "{}", body["error"]["message"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// Clearout documents its answers as Valid, Invalid, Catch All and Unknown, without saying how they're cased.
fn clearout_verdict(body: &Value) -> Verdict {
    let is = |field: &Value, value: &str| field.as_str().is_some_and(|s| s.eq_ignore_ascii_case(value));
    match &body["data"] {
        data if is(&data["disposable"], "yes") => Verdict::Invalid,
        data if is(&data["status"], "valid") => Verdict::Valid,
        data if is(&data["status"], "invalid") => Verdict::Invalid,
        _ => Verdict::Unsure,
    }
}

/// Reoon's errors come as `{"status": "error", "reason": ...}`, like "Invalid API key.", which doesn't quote the key.
async fn reoon_get(url: Url) -> anyhow::Result<Value> {
    let res = client()?.get(url).send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success() && body["status"] != "error", "{}", body["reason"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// Power mode's answers. A spam trap or a disabled inbox is as bad to send to as one that doesn't exist. A role account
/// or a full inbox exists, so it sends like a catch-all.
fn reoon_verdict(status: &str) -> Verdict {
    match status {
        "safe" => Verdict::Valid,
        "invalid" | "disabled" | "disposable" | "spamtrap" => Verdict::Invalid,
        _ => Verdict::Unsure,
    }
}

fn millionverifier_verdict(result: &str) -> Verdict {
    match result {
        "ok" => Verdict::Valid,
        "invalid" | "disposable" => Verdict::Invalid,
        _ => Verdict::Unsure,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_both_services() {
        assert_eq!(neverbounce_verdict("valid"), Verdict::Valid);
        assert_eq!(neverbounce_verdict("disposable"), Verdict::Invalid);
        assert_eq!(neverbounce_verdict("catchall"), Verdict::Unsure);
        assert_eq!(millionverifier_verdict("ok"), Verdict::Valid);
        assert_eq!(millionverifier_verdict("invalid"), Verdict::Invalid);
        assert_eq!(millionverifier_verdict("catch_all"), Verdict::Unsure);
        assert_eq!(hunter_verdict("valid"), Verdict::Valid);
        assert_eq!(hunter_verdict("disposable"), Verdict::Invalid);
        assert_eq!(hunter_verdict("webmail"), Verdict::Unsure);
        assert_eq!(hunter_verdict(""), Verdict::Unsure);
        // Kickbox's sandbox answers for deliverable@, rejected-email@, disposable@ and accept-all@.
        assert_eq!(kickbox_verdict(&json!({"result": "deliverable", "disposable": false, "accept_all": false})), Verdict::Valid);
        assert_eq!(kickbox_verdict(&json!({"result": "undeliverable", "disposable": false, "accept_all": false})), Verdict::Invalid);
        assert_eq!(kickbox_verdict(&json!({"result": "risky", "disposable": true, "accept_all": true})), Verdict::Invalid);
        assert_eq!(kickbox_verdict(&json!({"result": "risky", "disposable": false, "accept_all": true})), Verdict::Unsure);
        // Bouncer's documented answers.
        assert_eq!(bouncer_verdict(&json!({"status": "deliverable", "domain": {"disposable": "no"}})), Verdict::Valid);
        assert_eq!(bouncer_verdict(&json!({"status": "undeliverable", "reason": "invalid_email"})), Verdict::Invalid);
        assert_eq!(bouncer_verdict(&json!({"status": "risky", "reason": "low_quality", "domain": {"disposable": "yes"}})), Verdict::Invalid);
        assert_eq!(bouncer_verdict(&json!({"status": "risky", "domain": {"acceptAll": "yes", "disposable": "no"}})), Verdict::Unsure);
        assert_eq!(bouncer_verdict(&json!({"status": "unknown", "reason": "timeout"})), Verdict::Unsure);
        // Emailable's documented answers, and the 249 it sends when a check runs long.
        assert_eq!(emailable_verdict(&json!({"state": "deliverable", "disposable": false})), Verdict::Valid);
        assert_eq!(emailable_verdict(&json!({"state": "undeliverable", "reason": "rejected_email"})), Verdict::Invalid);
        assert_eq!(emailable_verdict(&json!({"state": "risky", "reason": "low_quality", "disposable": true})), Verdict::Invalid);
        assert_eq!(emailable_verdict(&json!({"state": "risky", "accept_all": true, "disposable": false})), Verdict::Unsure);
        assert_eq!(emailable_verdict(&json!({"message": "Your request is taking longer than normal. Please send your request again."})), Verdict::Unsure);
        // Clearout's documented answers in either case, and the empty body of a 524.
        assert_eq!(clearout_verdict(&json!({"status": "success", "data": {"status": "valid", "disposable": "no"}})), Verdict::Valid);
        assert_eq!(clearout_verdict(&json!({"status": "success", "data": {"status": "Invalid", "disposable": "No"}})), Verdict::Invalid);
        assert_eq!(clearout_verdict(&json!({"status": "success", "data": {"status": "valid", "disposable": "Yes"}})), Verdict::Invalid);
        assert_eq!(clearout_verdict(&json!({"status": "success", "data": {"status": "catch_all", "disposable": "no"}})), Verdict::Unsure);
        assert_eq!(clearout_verdict(&Value::Null), Verdict::Unsure);
        assert_eq!(reoon_verdict("safe"), Verdict::Valid);
        assert_eq!(reoon_verdict("spamtrap"), Verdict::Invalid);
        assert_eq!(reoon_verdict("catch_all"), Verdict::Unsure);
        assert_eq!(reoon_verdict("role_account"), Verdict::Unsure);
        // Hunter's test key answers with this person whatever was asked.
        let found = json!({"data": {"email": "Richard@piedpiper.com", "score": 99, "verification": {"status": "valid"}}});
        assert_eq!(hunter_found(&found, "piedpiper.com"), Some(Found { email: "richard@piedpiper.com".into(), verified: true }));
        assert_eq!(hunter_found(&found, "stripe.com"), None);
        let unsure = json!({"data": {"email": "r@piedpiper.com", "score": 40, "verification": {"status": "accept_all"}}});
        assert_eq!(hunter_found(&unsure, "piedpiper.com"), None);
        assert_eq!(hunter_found(&json!({"data": {"email": null}}), "piedpiper.com"), None);
        // Real responses to a bad key, from both services.
        let nb = neverbounce_ok(&json!({"status": "auth_failure", "message": "Invalid API key 'secret123'"})).unwrap_err();
        assert!(!nb.to_string().contains("secret123"));
        let mv = millionverifier_ok(&json!({"result": "", "error": "Insufficient credits"})).unwrap_err();
        assert_eq!(mv.to_string(), "Insufficient credits");
        assert!(millionverifier_ok(&json!({"result": "error", "error": "apikey_not_found"})).is_err());
        assert!(millionverifier_ok(&json!({"result": "ok", "error": ""})).is_ok());
    }
}
