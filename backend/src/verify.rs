//! Checks a lead's address with an email verification service before its first cold email, so addresses that don't
//! exist fail in the app instead of bouncing and hurting the mailbox's reputation. NeverBounce, MillionVerifier, Kickbox,
//! Reoon and DeBounce take the key in the URL, so every error drops its URL before it's logged or shown. Hunter, Bouncer, Emailable,
//! Clearout and EmailListVerify take it in a header. It also finds the real address behind a guessed one, with Hunter, Findymail, Prospeo or
//! Anymail Finder or LeadMagic.

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
    DeBounce,
    EmailListVerify,
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
            "debounce" => Some(Verifier::DeBounce),
            "emaillistverify" => Some(Verifier::EmailListVerify),
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
            Verifier::DeBounce => "DeBounce",
            Verifier::EmailListVerify => "EmailListVerify",
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
            Verifier::DeBounce => debounce_get(Url::parse_with_params("https://api.debounce.io/v1/balance/", [("api", key)])?).await.map(|_| ()),
            Verifier::EmailListVerify => elv_get(Url::parse(&format!("{ELV}/credits"))?, key).await.map(|_| ()),
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
            Verifier::DeBounce => {
                let body = debounce_get(Url::parse_with_params("https://api.debounce.io/v1/", [("api", key), ("email", email)])?).await?;
                Ok(debounce_verdict(body["debounce"]["code"].as_str().unwrap_or_default()))
            }
            Verifier::EmailListVerify => {
                let result = elv_get(Url::parse_with_params(&format!("{ELV}/verifyEmail"), [("email", email)])?, key).await?;
                anyhow::ensure!(result != "error_credit", "the account is out of credits");
                Ok(elv_verdict(&result))
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

/// The project's Hunter API key, from Email finder or Email verification, or None when neither is Hunter.
pub async fn hunter_key(state: &AppState, project_id: Uuid) -> anyhow::Result<Option<String>> {
    let row: Option<(String, String, String, String)> =
        sqlx::query_as("select finder, finder_key_enc, verifier, verifier_key_enc from project_settings where project_id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await?;
    let Some((finder, finder_enc, verifier, verifier_enc)) = row else { return Ok(None) };
    let enc = if finder == "hunter" { finder_enc } else if verifier == "hunter" { verifier_enc } else { return Ok(None) };
    Ok(Some(crypto::decrypt(&state.cfg.app_secret, &enc)?))
}

/// A service that finds a person's work address from their name and company domain.
#[derive(Clone, Copy, Debug)]
pub enum Finder {
    Hunter,
    Findymail,
    Prospeo,
    Anymail,
    LeadMagic,
}

impl Finder {
    pub fn parse(name: &str) -> Option<Finder> {
        match name {
            "hunter" => Some(Finder::Hunter),
            "findymail" => Some(Finder::Findymail),
            "prospeo" => Some(Finder::Prospeo),
            "anymailfinder" => Some(Finder::Anymail),
            "leadmagic" => Some(Finder::LeadMagic),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Finder::Hunter => "Hunter",
            Finder::Findymail => "Findymail",
            Finder::Prospeo => "Prospeo",
            Finder::Anymail => "Anymail Finder",
            Finder::LeadMagic => "LeadMagic",
        }
    }

    /// Checks the key works, before it's saved. Each of these costs no credit.
    pub async fn check_key(self, key: &str) -> anyhow::Result<()> {
        match self {
            Finder::Hunter => hunter_get(Url::parse("https://api.hunter.io/v2/account")?, key).await.map(|_| ()),
            Finder::Findymail => findymail_call(client()?.get("https://app.findymail.com/api/credits"), key).await.map(|_| ()),
            Finder::Prospeo => prospeo_call(client()?.get("https://api.prospeo.io/account-information"), key).await.map(|_| ()),
            Finder::Anymail => anymailfinder_call(client()?.get("https://api.anymailfinder.com/v5.1/account"), key).await.map(|_| ()),
            Finder::LeadMagic => leadmagic_call(client()?.get(format!("{LEADMAGIC}/credits")), key).await.map(|_| ()),
        }
    }

    /// The person's address at `domain`, or None when the service has nothing it's sure of there.
    async fn find(self, key: &str, (first, last): (&str, &str), domain: &str) -> anyhow::Result<Option<Found>> {
        match self {
            Finder::Hunter => {
                let query = [("domain", domain), ("first_name", first), ("last_name", last)];
                let url = Url::parse_with_params("https://api.hunter.io/v2/email-finder", query)?;
                Ok(hunter_found(&hunter_get(url, key).await?, domain))
            }
            Finder::Findymail => {
                let name = format!("{first} {last}");
                let req = client()?.post("https://app.findymail.com/api/search/name").json(&json!({ "name": name.trim(), "domain": domain }));
                Ok(findymail_found(&findymail_call(req, key).await?, domain))
            }
            Finder::Prospeo => {
                // Only a verified address costs a credit, and it's the only kind wanted here.
                let person = if last.is_empty() { json!({ "full_name": first }) } else { json!({ "first_name": first, "last_name": last }) };
                let mut body = json!({ "only_verified_email": true, "data": person });
                body["data"]["company_website"] = json!(domain);
                Ok(prospeo_found(&prospeo_call(client()?.post("https://api.prospeo.io/enrich-person").json(&body), key).await?, domain))
            }
            Finder::Anymail => {
                let mut body = if last.is_empty() { json!({ "full_name": first }) } else { json!({ "first_name": first, "last_name": last }) };
                body["domain"] = json!(domain);
                let req = client()?.post("https://api.anymailfinder.com/v5.1/find-email/person").json(&body);
                // It searches live and suggests waiting up to 3 minutes. One that runs past the client's wait counts as
                // nothing found, and asking again within 30 days is free.
                match anymailfinder_call(req, key).await {
                    Err(e) if e.downcast_ref::<reqwest::Error>().is_some_and(reqwest::Error::is_timeout) => Ok(None),
                    body => Ok(anymailfinder_found(&body?, domain)),
                }
            }
            Finder::LeadMagic => {
                let mut body = if last.is_empty() { json!({ "full_name": first }) } else { json!({ "first_name": first, "last_name": last }) };
                body["domain"] = json!(domain);
                let req = client()?.post(format!("{LEADMAGIC}/people/email-finder")).json(&body);
                Ok(leadmagic_found(&leadmagic_call(req, key).await?, domain))
            }
        }
    }
}

/// An address a finder found for a lead, to replace a guessed one.
#[derive(Debug, PartialEq)]
pub struct Found {
    pub email: String,
    /// The finder also verified it, so it needn't be checked again.
    pub verified: bool,
}

pub enum Lookup {
    Found(Found),
    /// No finder, or nothing it's confident in.
    Nothing,
    /// Hunter connected under Email verification failed, and its failure is shown there, so the verification that follows
    /// mustn't clear it.
    Failed,
}

/// Looks up the lead's real address at the guessed address's domain, with the project's email finder, or with Hunter
/// connected under Email verification when there's no finder.
pub async fn find(state: &AppState, project_id: Uuid, lead: &Lead, guessed: &str) -> Lookup {
    let row: Option<(String, String, String, String)> =
        sqlx::query_as("select finder, finder_key_enc, verifier, verifier_key_enc from project_settings where project_id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let Some((finder, finder_enc, verifier, verifier_enc)) = row else { return Lookup::Nothing };
    let (service, enc, alert) = match Finder::parse(&finder) {
        Some(service) => (service, finder_enc, Alert::Finder),
        None if verifier == "hunter" => (Finder::Hunter, verifier_enc, Alert::Verifier),
        None => return Lookup::Nothing,
    };
    let Some((_, domain)) = guessed.rsplit_once('@') else { return Lookup::Nothing };
    let domain = domain.to_lowercase();
    let (first, last) = if lead.first_name.is_empty() {
        lead.full_name.trim().split_once(' ').unwrap_or((lead.full_name.trim(), ""))
    } else {
        (lead.first_name.as_str(), lead.last_name.as_str())
    };
    let result = match crypto::decrypt(&state.cfg.app_secret, &enc) {
        Ok(key) => service.find(&key, (first.trim(), last.trim()), &domain).await,
        Err(e) => Err(e),
    };
    // Under Email verification the failure says it came from the finder, and only a failure is recorded here, since the
    // verification that follows records its own success.
    let under_verifier = matches!(alert, Alert::Verifier);
    let shown = result.as_ref().map(|_| ()).map_err(|e| if under_verifier { anyhow::anyhow!("Email finder: {e:#}") } else { anyhow::anyhow!("{e:#}") });
    if !under_verifier || shown.is_err() {
        hooks::record(&state.db, project_id, alert, &enc, &shown).await;
    }
    match result {
        Ok(found) => found.map_or(Lookup::Nothing, Lookup::Found),
        Err(e) => {
            tracing::warn!(error = %e, finder = service.label(), "email finder failed");
            if under_verifier { Lookup::Failed } else { Lookup::Nothing }
        }
    }
}

/// The address when it's at `domain`, so a lead is never pointed at someone at another company.
fn at_domain(email: Option<&str>, domain: &str) -> Option<String> {
    let email = email?.trim().to_lowercase();
    email.rsplit_once('@').is_some_and(|(_, d)| d == domain).then_some(email)
}

/// Hunter's answer, kept only when it's verified or Hunter is fairly sure (score 0 to 100).
fn hunter_found(body: &Value, domain: &str) -> Option<Found> {
    let data = &body["data"];
    let verified = data["verification"]["status"] == "valid";
    let sure = verified || data["score"].as_i64().unwrap_or(0) >= 80;
    at_domain(data["email"].as_str(), domain).filter(|_| sure).map(|email| Found { email, verified })
}

/// Findymail's errors come as `{"error": ...}`, like "Not enough credits", or `{"message": "Unauthenticated."}` for a bad
/// key. It doesn't document how it answers when it finds nothing, so a 404 counts as nothing found.
async fn findymail_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = req.bearer_auth(key).header("accept", "application/json").send().await?;
    let status = res.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(Value::Null);
    }
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["error"].as_str().or(body["message"].as_str()).unwrap_or(status.as_str()));
    Ok(body)
}

/// Findymail says it returns verified addresses, but its answer doesn't say so, so a connected verifier still checks it.
fn findymail_found(body: &Value, domain: &str) -> Option<Found> {
    at_domain(body["contact"]["email"].as_str(), domain).map(|email| Found { email, verified: false })
}

/// Prospeo answers every error with `{"error": true, "error_code": ...}`, like INVALID_API_KEY or INSUFFICIENT_CREDITS.
/// NO_MATCH means it found nobody, which isn't a failure.
async fn prospeo_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = req.header("X-KEY", key).send().await?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    let code = body["error_code"].as_str().map(str::to_string);
    match code.as_deref() {
        Some("NO_MATCH") => Ok(Value::Null),
        _ if status.is_success() && body["error"] != true => Ok(body),
        code => anyhow::bail!("{}", code.unwrap_or(status.as_str())),
    }
}

/// Prospeo's address, when it's revealed in full and verified.
fn prospeo_found(body: &Value, domain: &str) -> Option<Found> {
    let email = &body["person"]["email"];
    let usable = email["revealed"] != false && email["status"] == "VERIFIED";
    at_domain(email["email"].as_str().filter(|e| !e.contains('*')), domain).filter(|_| usable).map(|email| Found { email, verified: true })
}

/// Anymail Finder explains a failure in `message`, like "Invalid API key. Copy it again from ..." for a 401.
async fn anymailfinder_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = req.header("Authorization", key).send().await?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["message"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// Anymail Finder fills `valid_email` only when it verified the address, and charges a credit only then.
fn anymailfinder_found(body: &Value, domain: &str) -> Option<Found> {
    at_domain(body["valid_email"].as_str(), domain).map(|email| Found { email, verified: true })
}

const LEADMAGIC: &str = "https://api.leadmagic.io/v1";

/// LeadMagic explains a failure in `detail`, like "Not enough credits for this request." A wrong key's detail quotes
/// the key's first characters, so a 401 gets its own message.
async fn leadmagic_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = req.header("X-API-Key", key).send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["detail"].as_str().or(body["message"].as_str()).unwrap_or(status.as_str()));
    Ok(body)
}

/// LeadMagic's status is `valid` when it verified the address, and null with no address when it found nothing, which
/// costs no credit.
fn leadmagic_found(body: &Value, domain: &str) -> Option<Found> {
    at_domain(body["email"].as_str(), domain).filter(|_| body["status"] == "valid").map(|email| Found { email, verified: true })
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

/// A failure comes back as `{"debounce": {"error": ...}, "success": "0"}`. A wrong key's error is "Wrong API", so that
/// one gets the same message as NeverBounce's.
async fn debounce_get(url: Url) -> anyhow::Result<Value> {
    let res = client()?.get(url).send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success() && body["success"] != "0", "{}", body["debounce"]["error"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// DeBounce's result codes: 5 is deliverable, and syntax (1), spam trap (2), disposable (3) and invalid (6) aren't worth
/// sending to. Accept-all (4) and unknown (7) send like a catch-all.
fn debounce_verdict(code: &str) -> Verdict {
    match code {
        "5" => Verdict::Valid,
        "1" | "2" | "3" | "6" => Verdict::Invalid,
        _ => Verdict::Unsure,
    }
}

const ELV: &str = "https://api.emaillistverify.com/api";

/// The answer as text: a verification answers with its result alone, like `ok`. A failure is `{statusCode, message}`.
async fn elv_get(url: Url, key: &str) -> anyhow::Result<String> {
    let res = client()?.get(url).header("x-api-key", key).send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        let body: Value = serde_json::from_str(&text).unwrap_or_default();
        anyhow::bail!("{}", body["message"].as_str().unwrap_or(status.as_str()));
    }
    Ok(text.trim().to_string())
}

/// A dead server, a missing or disabled inbox, bad syntax, a throwaway inbox or a spam trap isn't worth sending to.
/// Accept-all (`ok_for_all`), a server that blocked the check or an unknown sends like a catch-all.
fn elv_verdict(result: &str) -> Verdict {
    match result {
        "ok" => Verdict::Valid,
        "dead_server" | "invalid_mx" | "email_disabled" | "invalid_syntax" | "disposable" | "spamtrap" => Verdict::Invalid,
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
        assert_eq!(debounce_verdict("5"), Verdict::Valid);
        assert_eq!(debounce_verdict("6"), Verdict::Invalid);
        assert_eq!(debounce_verdict("2"), Verdict::Invalid);
        assert_eq!(debounce_verdict("4"), Verdict::Unsure);
        assert_eq!(debounce_verdict("7"), Verdict::Unsure);
        // EmailListVerify's documented results.
        assert_eq!(elv_verdict("ok"), Verdict::Valid);
        assert_eq!(elv_verdict("email_disabled"), Verdict::Invalid);
        assert_eq!(elv_verdict("disposable"), Verdict::Invalid);
        assert_eq!(elv_verdict("ok_for_all"), Verdict::Unsure);
        assert_eq!(elv_verdict("antispam_system"), Verdict::Unsure);
        // Hunter's test key answers with this person whatever was asked.
        let found = json!({"data": {"email": "Richard@piedpiper.com", "score": 99, "verification": {"status": "valid"}}});
        assert_eq!(hunter_found(&found, "piedpiper.com"), Some(Found { email: "richard@piedpiper.com".into(), verified: true }));
        assert_eq!(hunter_found(&found, "stripe.com"), None);
        let unsure = json!({"data": {"email": "r@piedpiper.com", "score": 40, "verification": {"status": "accept_all"}}});
        assert_eq!(hunter_found(&unsure, "piedpiper.com"), None);
        assert_eq!(hunter_found(&json!({"data": {"email": null}}), "piedpiper.com"), None);
        // Findymail's documented answer, and the empty body a 404 becomes.
        let findymail = json!({"contact": {"name": "John Doe", "domain": "website.com", "email": "John@Website.com"}});
        assert_eq!(findymail_found(&findymail, "website.com"), Some(Found { email: "john@website.com".into(), verified: false }));
        assert_eq!(findymail_found(&findymail, "other.com"), None);
        assert_eq!(findymail_found(&Value::Null, "website.com"), None);
        // Prospeo's documented answer, a masked one, and one it couldn't verify.
        let prospeo = |email: &str, status: &str, revealed: bool| {
            let person = json!({"email": {"status": status, "revealed": revealed, "email": email}});
            json!({"error": false, "person": person, "company": {"domain": "intercom.com"}})
        };
        let eva = Some(Found { email: "eva@intercom.com".into(), verified: true });
        assert_eq!(prospeo_found(&prospeo("Eva@intercom.com", "VERIFIED", true), "intercom.com"), eva);
        assert_eq!(prospeo_found(&prospeo("eva.*****@intercom.com", "VERIFIED", true), "intercom.com"), None);
        assert_eq!(prospeo_found(&prospeo("eva@intercom.com", "VERIFIED", false), "intercom.com"), None);
        assert_eq!(prospeo_found(&prospeo("eva@intercom.com", "UNVERIFIED", true), "intercom.com"), None);
        assert_eq!(prospeo_found(&prospeo("eva@intercom.com", "VERIFIED", true), "stripe.com"), None);
        assert_eq!(prospeo_found(&Value::Null, "intercom.com"), None);
        // Anymail Finder's documented answers: found and verified, and not found.
        let found = json!({"email": "Satyan@microsoft.com", "email_status": "valid", "valid_email": "Satyan@microsoft.com", "credits_charged": 1});
        assert_eq!(anymailfinder_found(&found, "microsoft.com"), Some(Found { email: "satyan@microsoft.com".into(), verified: true }));
        assert_eq!(anymailfinder_found(&found, "github.com"), None);
        let missing = json!({"email": null, "email_status": "not_found", "valid_email": null, "credits_charged": 0});
        assert_eq!(anymailfinder_found(&missing, "microsoft.com"), None);
        // LeadMagic's documented answers: found and verified, and not found.
        let found = json!({"email": "Jesse@leadmagic.io", "status": "valid", "credits_consumed": 1, "message": "Valid email found."});
        assert_eq!(leadmagic_found(&found, "leadmagic.io"), Some(Found { email: "jesse@leadmagic.io".into(), verified: true }));
        assert_eq!(leadmagic_found(&found, "acme.com"), None);
        assert_eq!(leadmagic_found(&json!({"email": null, "status": null, "credits_consumed": 0}), "leadmagic.io"), None);
        // Real responses to a bad key, from both services.
        let nb = neverbounce_ok(&json!({"status": "auth_failure", "message": "Invalid API key 'secret123'"})).unwrap_err();
        assert!(!nb.to_string().contains("secret123"));
        let mv = millionverifier_ok(&json!({"result": "", "error": "Insufficient credits"})).unwrap_err();
        assert_eq!(mv.to_string(), "Insufficient credits");
        assert!(millionverifier_ok(&json!({"result": "error", "error": "apikey_not_found"})).is_err());
        assert!(millionverifier_ok(&json!({"result": "ok", "error": ""})).is_ok());
    }
}
