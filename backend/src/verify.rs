//! Checks a lead's address with an email verification service before its first cold email, so addresses that don't
//! exist fail in the app instead of bouncing and hurting the mailbox's reputation. NeverBounce, MillionVerifier, Kickbox,
//! Reoon, DeBounce, Bouncify, QuickEmailVerification, MailboxValidator and MyEmailVerifier take the key in the URL, so every error drops its URL before it's logged or shown. Hunter, Bouncer, Emailable,
//! Clearout, EmailListVerify and Abstract take it in a header. It also finds the real address behind a guessed one, with Hunter, Findymail, Prospeo,
//! Anymail Finder, LeadMagic, Apollo, Voila Norbert, Skrapp or Lusha.

use std::sync::LazyLock;
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
    Bouncify,
    QuickEmailVerification,
    Abstract,
    MailboxValidator,
    MyEmailVerifier,
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
            "bouncify" => Some(Verifier::Bouncify),
            "quickemailverification" => Some(Verifier::QuickEmailVerification),
            "abstract" => Some(Verifier::Abstract),
            "mailboxvalidator" => Some(Verifier::MailboxValidator),
            "myemailverifier" => Some(Verifier::MyEmailVerifier),
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
            Verifier::Bouncify => "Bouncify",
            Verifier::QuickEmailVerification => "QuickEmailVerification",
            Verifier::Abstract => "Abstract API",
            Verifier::MailboxValidator => "MailboxValidator",
            Verifier::MyEmailVerifier => "MyEmailVerifier",
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
            Verifier::Bouncify => bouncify_get(Url::parse_with_params("https://api.bouncify.io/v1/info", [("apikey", key)])?).await.map(|_| ()),
            // The sandbox costs no credit and still turns down a wrong key.
            Verifier::QuickEmailVerification => {
                qev_get(Url::parse_with_params(&format!("{QEV}/sandbox"), [("email", "valid@example.com"), ("apikey", key)])?).await.map(|_| ())
            }
            // Abstract has no free call, so this check uses one credit.
            Verifier::Abstract => abstract_get(Url::parse_with_params(ABSTRACT, [("email", "check@example.com")])?, key).await.map(|_| ()),
            // MailboxValidator has no free call either, and asks for an address before it looks at the key.
            Verifier::MailboxValidator => mbv_get(Url::parse_with_params(MBV, [("email", "check@example.com"), ("key", key)])?).await.map(|_| ()),
            // The balance costs nothing. The key goes in the path, as a segment of its own.
            Verifier::MyEmailVerifier => {
                let mut url = Url::parse("https://client.myemailverifier.com/verifier/getcredits")?;
                url.path_segments_mut().map_err(|_| anyhow::anyhow!("bad URL"))?.push(key);
                mev_get(url).await.map(|_| ())
            }
        }
    }

    pub(crate) async fn verify(self, key: &str, email: &str) -> anyhow::Result<Verdict> {
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
            Verifier::Bouncify => {
                let url = Url::parse_with_params("https://api.bouncify.io/v1/verify", [("apikey", key), ("email", email)])?;
                Ok(bouncify_verdict(&bouncify_get(url).await?))
            }
            Verifier::QuickEmailVerification => Ok(qev_verdict(&qev_get(Url::parse_with_params(QEV, [("email", email), ("apikey", key)])?).await?)),
            Verifier::Abstract => Ok(abstract_verdict(&abstract_get(Url::parse_with_params(ABSTRACT, [("email", email)])?, key).await?)),
            Verifier::MailboxValidator => Ok(mbv_verdict(&mbv_get(Url::parse_with_params(MBV, [("email", email), ("key", key)])?).await?)),
            Verifier::MyEmailVerifier => {
                let url = Url::parse_with_params("https://api.myemailverifier.com/api/validate_single.php", [("apikey", key), ("email", email)])?;
                Ok(mev_verdict(&mev_get(url).await?))
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
    Apollo,
    Norbert,
    Skrapp,
    Lusha,
}

impl Finder {
    pub fn parse(name: &str) -> Option<Finder> {
        match name {
            "hunter" => Some(Finder::Hunter),
            "findymail" => Some(Finder::Findymail),
            "prospeo" => Some(Finder::Prospeo),
            "anymailfinder" => Some(Finder::Anymail),
            "leadmagic" => Some(Finder::LeadMagic),
            "apollo" => Some(Finder::Apollo),
            "voilanorbert" => Some(Finder::Norbert),
            "skrapp" => Some(Finder::Skrapp),
            "lusha" => Some(Finder::Lusha),
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
            Finder::Apollo => "Apollo",
            Finder::Norbert => "Voila Norbert",
            Finder::Skrapp => "Skrapp",
            Finder::Lusha => "Lusha",
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
            // The health check is free and answers 200 to any key, saying whether it's logged in.
            Finder::Apollo => {
                let health = apollo_call(client()?.get(format!("{APOLLO}/auth/health")), key).await?;
                anyhow::ensure!(health["is_logged_in"] == true, "the API key was rejected");
                Ok(())
            }
            Finder::Norbert => norbert_call(client()?.get(format!("{NORBERT}/account/")), key).await.map(|_| ()),
            Finder::Skrapp => skrapp_call(client()?.get(format!("{SKRAPP}/v2/account")), key).await.map(|_| ()),
            Finder::Lusha => lusha_call(client()?.get(format!("{LUSHA}/account/usage")), key).await.map(|_| ()),
        }
    }

    /// The person's address at `domain`, or None when the service has nothing it's sure of there. Prospeo, Apollo and Lusha
    /// also match on the LinkedIn profile, when the lead has one.
    pub(crate) async fn find(self, key: &str, person: &Person<'_>, domain: &str) -> anyhow::Result<Search> {
        let Person { first, last, linkedin } = *person;
        let found = |found| Search { found, credits: None };
        match self {
            Finder::Hunter => {
                let query = [("domain", domain), ("first_name", first), ("last_name", last)];
                let url = Url::parse_with_params("https://api.hunter.io/v2/email-finder", query)?;
                Ok(found(hunter_found(&hunter_get(url, key).await?, domain)))
            }
            Finder::Findymail => {
                let name = format!("{first} {last}");
                let req = client()?.post("https://app.findymail.com/api/search/name").json(&json!({ "name": name.trim(), "domain": domain }));
                Ok(found(findymail_found(&findymail_call(req, key).await?, domain)))
            }
            Finder::Prospeo => {
                // Only a verified address costs a credit, and it's the only kind wanted here.
                let person = if last.is_empty() { json!({ "full_name": first }) } else { json!({ "first_name": first, "last_name": last }) };
                let mut body = json!({ "only_verified_email": true, "data": person });
                body["data"]["company_website"] = json!(domain);
                if !linkedin.is_empty() {
                    body["data"]["linkedin_url"] = json!(linkedin);
                }
                Ok(found(prospeo_found(&prospeo_call(client()?.post("https://api.prospeo.io/enrich-person").json(&body), key).await?, domain)))
            }
            Finder::Anymail => {
                let mut body = if last.is_empty() { json!({ "full_name": first }) } else { json!({ "first_name": first, "last_name": last }) };
                body["domain"] = json!(domain);
                let req = client()?.post("https://api.anymailfinder.com/v5.1/find-email/person").json(&body);
                // It searches live and suggests waiting up to 3 minutes. One that runs past the client's wait counts as
                // nothing found, and asking again within 30 days is free.
                match anymailfinder_call(req, key).await {
                    Err(e) if e.downcast_ref::<reqwest::Error>().is_some_and(reqwest::Error::is_timeout) => Ok(found(None)),
                    body => {
                        let body = body?;
                        Ok(Search { found: anymailfinder_found(&body, domain), credits: body["credits_charged"].as_i64() })
                    }
                }
            }
            Finder::LeadMagic => {
                let mut body = if last.is_empty() { json!({ "full_name": first }) } else { json!({ "first_name": first, "last_name": last }) };
                body["domain"] = json!(domain);
                let req = client()?.post(format!("{LEADMAGIC}/people/email-finder")).json(&body);
                let body = leadmagic_call(req, key).await?;
                Ok(Search { found: leadmagic_found(&body, domain), credits: body["credits_consumed"].as_i64() })
            }
            Finder::Apollo => {
                let mut query = if last.is_empty() { vec![("name", first)] } else { vec![("first_name", first), ("last_name", last)] };
                query.push(("domain", domain));
                if !linkedin.is_empty() {
                    query.push(("linkedin_url", linkedin));
                }
                let url = Url::parse_with_params(&format!("{APOLLO}/people/match"), query)?;
                Ok(found(apollo_found(&apollo_call(client()?.post(url).json(&json!({})), key).await?, domain)))
            }
            Finder::Norbert => {
                let name = format!("{first} {last}");
                let req = client()?.post(format!("{NORBERT}/search/name")).form(&[("name", name.trim()), ("domain", domain)]);
                let mut body = norbert_call(req, key).await?;
                // A search that isn't done yet is read again from its contact, for up to half a minute. One still going
                // then counts as nothing found, and its result waits in Norbert.
                for _ in 0..6 {
                    let Some(id) = body["id"].as_i64().filter(|_| body["searching"] == true) else { break };
                    tokio::time::sleep(NORBERT_WAIT).await;
                    body = norbert_call(client()?.get(format!("{NORBERT}/contacts/{id}")), key).await?;
                }
                Ok(found(norbert_found(&body, domain)))
            }
            Finder::Skrapp => {
                let name = if last.is_empty() { vec![("fullName", first)] } else { vec![("firstName", first), ("lastName", last)] };
                let url = Url::parse_with_params(&format!("{SKRAPP}/v3/people/email"), name.into_iter().chain([("domain", domain)]))?;
                Ok(found(skrapp_found(&skrapp_call(client()?.get(url), key).await?, domain)))
            }
            // Lusha needs a last name or a LinkedIn profile, and charges for every search, even one that finds no one.
            Finder::Lusha if last.is_empty() && linkedin.is_empty() => Ok(found(None)),
            Finder::Lusha => {
                let mut contact = json!({ "firstName": first, "lastName": last, "companyDomain": domain });
                if !linkedin.is_empty() {
                    contact["linkedinUrl"] = json!(linkedin);
                }
                let req = client()?.post(format!("{LUSHA}/contacts/search-and-enrich")).json(&json!({ "contacts": [contact], "reveal": ["emails"] }));
                let body = lusha_call(req, key).await?;
                Ok(Search { found: lusha_found(&body, domain), credits: body["billing"]["creditsCharged"].as_i64() })
            }
        }
    }
}

/// Who a finder looks for.
#[derive(Clone, Copy)]
pub struct Person<'a> {
    pub first: &'a str,
    pub last: &'a str,
    /// The LinkedIn profile, or "".
    pub linkedin: &'a str,
}

impl<'a> Person<'a> {
    /// A lead imported with only a full name has it split at the first space.
    pub fn of(lead: &'a Lead) -> Person<'a> {
        let (first, last) = if lead.first_name.trim().is_empty() {
            lead.full_name.trim().split_once(' ').unwrap_or((lead.full_name.trim(), ""))
        } else {
            (lead.first_name.as_str(), lead.last_name.as_str())
        };
        Person { first: first.trim(), last: last.trim(), linkedin: lead.linkedin_url.trim() }
    }
}

/// A finder's answer about one person.
pub struct Search {
    pub found: Option<Found>,
    /// The credits it said it charged, for the services whose answer says.
    pub credits: Option<i64>,
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
    let result = match crypto::decrypt(&state.cfg.app_secret, &enc) {
        Ok(key) => service.find(&key, &Person::of(lead), &domain).await.map(|search| search.found),
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
    let res = send(req.bearer_auth(key).header("accept", "application/json")).await?;
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
    let res = send(req.header("X-KEY", key)).await?;
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
    let res = send(req.header("Authorization", key)).await?;
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
    let res = send(req.header("X-API-Key", key)).await.map_err(reqwest::Error::without_url)?;
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

const APOLLO: &str = "https://api.apollo.io/api/v1";

/// Apollo explains a failure in `error_details.message`, where `error` adds a link to its docs.
async fn apollo_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = send(req.header("x-api-key", key)).await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    let said = body["error_details"]["message"].as_str().or(body["error"].as_str());
    anyhow::ensure!(status.is_success(), "{}", said.unwrap_or(status.as_str()));
    Ok(body)
}

/// Apollo's match is the person it thinks this is, with their work address when it has one. Only a `verified` one is
/// kept, and a match that found no one, or an address it hasn't unlocked (`email_not_unlocked@domain.com`), isn't at
/// the lead's domain.
fn apollo_found(body: &Value, domain: &str) -> Option<Found> {
    let person = &body["person"];
    at_domain(person["email"].as_str(), domain).filter(|_| person["email_status"] == "verified").map(|email| Found { email, verified: true })
}

const NORBERT: &str = "https://api.voilanorbert.com/2018-01-08";
const NORBERT_WAIT: Duration = Duration::from_secs(5);

/// Norbert takes the key as the password of basic auth, with any user. A failure is `{code, error}`, a 402 is no credits
/// left, and a 400 is a domain it can't place, which counts as nothing found.
async fn norbert_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = send(req.basic_auth("splee", Some(key))).await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    anyhow::ensure!(status != reqwest::StatusCode::PAYMENT_REQUIRED, "the account is out of credits");
    if status == reqwest::StatusCode::BAD_REQUEST {
        return Ok(Value::Null);
    }
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["error"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// Norbert's score is 100 for an address its mail server accepted, 80 for one it found but couldn't check, which the
/// verifier checks before sending, and 5 for one from outside services, which isn't kept. A search still going finds
/// nothing yet.
fn norbert_found(body: &Value, domain: &str) -> Option<Found> {
    if body["searching"] == true {
        return None;
    }
    let score = body["email"]["score"].as_i64().unwrap_or(0);
    at_domain(body["email"]["email"].as_str(), domain).filter(|_| score >= 80).map(|email| Found { email, verified: score >= 100 })
}

const SKRAPP: &str = "https://api.skrapp.io/api";

/// Skrapp answers a wrong key with a 403 "Not authorized", and the same 403 with `email_limit_exeeded` (sic) when the
/// credits ran out. A 404 is nothing found, unless its code is KNF, a revoked key, and a 408 is a search that ran past
/// Skrapp's 10 seconds, which costs nothing and counts as nothing found.
async fn skrapp_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = send(req.header("X-Access-Key", key)).await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(body["email_limit_exeeded"] != true, "the account is out of credits");
    anyhow::ensure!(!(status == reqwest::StatusCode::FORBIDDEN && body["message"] == "Not authorized"), "the API key was rejected");
    anyhow::ensure!(body["code"] != "KNF", "the API key was rejected");
    if matches!(status.as_u16(), 404 | 408) {
        return Ok(Value::Null);
    }
    anyhow::ensure!(status.is_success(), "{}", body["message"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// Skrapp checks what it finds: a valid address is verified, and a catch-all one is kept for the verifier to check
/// before sending. Invalid and unknown ones aren't kept.
fn skrapp_found(body: &Value, domain: &str) -> Option<Found> {
    let verified = match body["quality"]["status"].as_str()? {
        "valid" => true,
        "catch-all" => false,
        _ => return None,
    };
    at_domain(body["email"].as_str(), domain).map(|email| Found { email, verified })
}

const LUSHA: &str = "https://api.lusha.com/v3";

/// Lusha explains a failure in `message`: a 401 (or a 400 for a key that isn't shaped like one) is a wrong key, a 402
/// is no credits left, and a 403 an inactive account or one without V3 API access.
async fn lusha_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = send(req.header("api_key", key)).await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    let message = body["message"].as_str().unwrap_or(status.as_str());
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED && message != "Invalid API key format", "the API key was rejected");
    anyhow::ensure!(status != reqwest::StatusCode::PAYMENT_REQUIRED, "the account is out of credits");
    anyhow::ensure!(status.is_success(), "{message}");
    Ok(body)
}

/// Lusha grades each address A+ (very likely to deliver) or A (likely), and a result it couldn't find has an `error`
/// instead. An A+ address is verified, an A one is kept for the verifier to check before sending, and lower grades aren't
/// kept.
fn lusha_found(body: &Value, domain: &str) -> Option<Found> {
    let emails = body["results"][0]["emails"].as_array()?;
    let graded = |grade: &str| emails.iter().find_map(|e| at_domain(e["email"].as_str().filter(|_| e["confidence"] == grade), domain));
    graded("A+").map(|email| Found { email, verified: true }).or_else(|| graded("A").map(|email| Found { email, verified: false }))
}

async fn get(url: Url) -> anyhow::Result<Value> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let res = send(client.get(url)).await.map_err(reqwest::Error::without_url)?;
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
    let res = send(client.get(url).header("X-API-KEY", key)).await.map_err(reqwest::Error::without_url)?;
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
    let res = send(client.get(url)).await.map_err(reqwest::Error::without_url)?;
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
    let res = send(client.get(url).header("x-api-key", key)).await.map_err(reqwest::Error::without_url)?;
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
    let res = send(client.get(url).bearer_auth(key)).await.map_err(reqwest::Error::without_url)?;
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

/// A local sandbox sets EMAIL_API_URL to send every finder and verifier call to a fake server on its own path, so trying
/// the app spends no credits. Never set it in production.
static FAKE_API: LazyLock<Option<Url>> = LazyLock::new(|| std::env::var("EMAIL_API_URL").ok().and_then(|u| Url::parse(&u).ok()));

#[cfg(test)]
tokio::task_local! {
    /// The same for one test, so tests running side by side each talk to their own fake.
    pub static TEST_API: Url;
}

/// Every call to a finder or verifier goes through here.
async fn send(req: reqwest::RequestBuilder) -> reqwest::Result<reqwest::Response> {
    let (client, req) = req.build_split();
    let mut req = req?;
    #[cfg(test)]
    let fake = TEST_API.try_with(Url::clone).ok().or_else(|| FAKE_API.clone());
    #[cfg(not(test))]
    let fake = FAKE_API.clone();
    if let Some(fake) = fake {
        let url = req.url_mut();
        let _ = url.set_scheme(fake.scheme());
        let _ = url.set_host(fake.host_str());
        let _ = url.set_port(fake.port());
    }
    client.execute(req).await
}

/// Clearout's errors come as `{"status": "failed", "error": {"message": ...}}`, like "Invalid API Token, please generate new
/// token". A deep check that runs past the timeout answers 524 and goes on in the background, so it reads as unsure.
async fn clearout_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = send(req.bearer_auth(key)).await?;
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
    let res = send(client()?.get(url)).await.map_err(reqwest::Error::without_url)?;
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
    let res = send(client()?.get(url)).await.map_err(reqwest::Error::without_url)?;
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
    let res = send(client()?.get(url).header("x-api-key", key)).await.map_err(reqwest::Error::without_url)?;
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

/// A failure is `{"result": "Invalid API key", "success": false}`, so its `result` is the reason. A wrong key is a 401,
/// which gets the same message as the other services'.
async fn bouncify_get(url: Url) -> anyhow::Result<Value> {
    let res = send(client()?.get(url)).await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success() && body["success"] != false, "{}", body["result"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// An undeliverable address, a throwaway inbox or a spam trap isn't worth sending to, even when the inbox exists.
/// Accept-all and unknown send like a catch-all. Bouncify's docs show the flags as 0 or 1 in one place and as booleans in another.
fn bouncify_verdict(body: &Value) -> Verdict {
    if [&body["disposable"], &body["spamtrap"]].into_iter().any(|flag| *flag == 1 || *flag == true) {
        return Verdict::Invalid;
    }
    match body["result"].as_str().unwrap_or_default() {
        "deliverable" => Verdict::Valid,
        "undeliverable" => Verdict::Invalid,
        _ => Verdict::Unsure,
    }
}

const QEV: &str = "https://api.quickemailverification.com/v1/verify";

/// A failure is `{success: "false", message}`, like "Low credit. Payment required", and a wrong key a 401.
async fn qev_get(url: Url) -> anyhow::Result<Value> {
    let res = send(client()?.get(url)).await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success() && body["success"] == "true", "{}", body["message"].as_str().filter(|m| !m.is_empty()).unwrap_or(status.as_str()));
    Ok(body)
}

/// A throwaway inbox isn't worth sending to even when it exists, and a domain that accepts every address sends like a
/// catch-all. Its flags are the strings "true" and "false".
fn qev_verdict(body: &Value) -> Verdict {
    if body["disposable"] == "true" {
        return Verdict::Invalid;
    }
    match body["result"].as_str().unwrap_or_default() {
        "valid" if body["accept_all"] != "true" => Verdict::Valid,
        "invalid" => Verdict::Invalid,
        _ => Verdict::Unsure,
    }
}

const ABSTRACT: &str = "https://emailreputation.abstractapi.com/v1/";

/// Abstract's Email Reputation API. A failure is `{error: {message}}`, like a used-up quota's 422.
async fn abstract_get(url: Url, key: &str) -> anyhow::Result<Value> {
    let res = send(client()?.get(url).bearer_auth(key)).await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["error"]["message"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// A throwaway inbox isn't worth sending to, a catch-all domain sends as it would without a verifier, and so does a mail
/// server that didn't answer, which Abstract counts as undeliverable.
fn abstract_verdict(body: &Value) -> Verdict {
    if body["email_quality"]["is_disposable"] == true {
        return Verdict::Invalid;
    }
    let deliverability = &body["email_deliverability"];
    match (deliverability["status"].as_str().unwrap_or_default(), deliverability["status_detail"].as_str().unwrap_or_default()) {
        ("deliverable", _) if body["email_quality"]["is_catchall"] == true => Verdict::Unsure,
        ("deliverable", _) => Verdict::Valid,
        ("undeliverable", "unavailable_server") => Verdict::Unsure,
        ("undeliverable", _) => Verdict::Invalid,
        _ => Verdict::Unsure,
    }
}

const MBV: &str = "https://api.mailboxvalidator.com/v2/validation/single";

/// MailboxValidator answers every key problem with a 401 and says which in `{error: {error_code, error_message}}`, where
/// 10004 is a used-up balance.
async fn mbv_get(url: Url) -> anyhow::Result<Value> {
    let res = send(client()?.get(url)).await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    let error = &body["error"];
    anyhow::ensure!(error["error_code"] != 10004, "the account is out of credits");
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    anyhow::ensure!(status.is_success(), "{}", error["error_message"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

/// `status` is MailboxValidator's overall verdict, which also turns down role and risky addresses that may well exist,
/// so only what says the address can't get mail is Invalid. A catch-all domain, or a server that was down, greylisted or
/// didn't say, sends as it would without a verifier.
fn mbv_verdict(body: &Value) -> Verdict {
    if body["is_disposable"] == true || body["is_syntax"] == false || body["is_domain"] == false || body["is_verified"] == false {
        return Verdict::Invalid;
    }
    if body["is_catchall"] == true || body["is_server_down"] == true || body["is_greylisted"] == true || !body["is_verified"].is_boolean() {
        return Verdict::Unsure;
    }
    if body["status"] == true { Verdict::Valid } else { Verdict::Unsure }
}

/// MyEmailVerifier answers a wrong key with a 401 on the balance and a 200 on a verification, and either way puts the
/// failure in `{status: "error" or false, error, message}`. A result has `Status` instead.
async fn mev_get(url: Url) -> anyhow::Result<Value> {
    let res = send(client()?.get(url)).await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    let said = |key: &str| body[key].as_str().unwrap_or_default().to_ascii_lowercase();
    let failed = !status.is_success() || body["status"] == "error" || body["status"] == false;
    if failed {
        anyhow::ensure!(!said("error").contains("credit") && !said("message").contains("credit"), "the account is out of credits");
        let rejected = status == reqwest::StatusCode::UNAUTHORIZED || ["unauthorized", "invalid_api_key"].contains(&said("error").as_str());
        anyhow::ensure!(!rejected, "the API key was rejected");
        anyhow::bail!("{}", body["message"].as_str().unwrap_or(status.as_str()));
    }
    Ok(body)
}

/// MyEmailVerifier's flags are the strings "true" and "false". A catch-all or greylisting server, or an Unknown, sends as
/// it would without a verifier.
fn mev_verdict(body: &Value) -> Verdict {
    let flag = |key: &str| body[key] == "true" || body[key] == true;
    match body["Status"].as_str().unwrap_or_default() {
        _ if flag("Disposable_Domain") => Verdict::Invalid,
        "Invalid" => Verdict::Invalid,
        "Valid" if !flag("catch_all") && !flag("Greylisted") => Verdict::Valid,
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
        // Bouncify's results, with its flags as numbers or booleans.
        assert_eq!(bouncify_verdict(&json!({"result": "deliverable", "disposable": 0, "spamtrap": 0})), Verdict::Valid);
        assert_eq!(bouncify_verdict(&json!({"result": "undeliverable"})), Verdict::Invalid);
        assert_eq!(bouncify_verdict(&json!({"result": "deliverable", "disposable": 1})), Verdict::Invalid);
        assert_eq!(bouncify_verdict(&json!({"result": "deliverable", "spamtrap": true})), Verdict::Invalid);
        assert_eq!(bouncify_verdict(&json!({"result": "accept all", "accept_all": 1})), Verdict::Unsure);
        assert_eq!(bouncify_verdict(&json!({"result": "unknown"})), Verdict::Unsure);
        // QuickEmailVerification's sandbox answers, with its flags as strings.
        assert_eq!(qev_verdict(&json!({"result": "valid", "reason": "accepted_email", "disposable": "false", "accept_all": "false"})), Verdict::Valid);
        assert_eq!(qev_verdict(&json!({"result": "valid", "disposable": "true", "accept_all": "false"})), Verdict::Invalid);
        assert_eq!(qev_verdict(&json!({"result": "valid", "disposable": "false", "accept_all": "true"})), Verdict::Unsure);
        assert_eq!(qev_verdict(&json!({"result": "invalid", "reason": "rejected_email", "disposable": "false"})), Verdict::Invalid);
        assert_eq!(qev_verdict(&json!({"result": "unknown", "reason": "timeout"})), Verdict::Unsure);
        // Abstract's documented answers.
        let abstract_api = |status: &str, detail: &str, catchall: bool, disposable: bool| {
            json!({"email_deliverability": {"status": status, "status_detail": detail}, "email_quality": {"is_catchall": catchall, "is_disposable": disposable}})
        };
        assert_eq!(abstract_verdict(&abstract_api("deliverable", "valid_email", false, false)), Verdict::Valid);
        assert_eq!(abstract_verdict(&abstract_api("deliverable", "valid_email", true, false)), Verdict::Unsure);
        assert_eq!(abstract_verdict(&abstract_api("deliverable", "valid_email", false, true)), Verdict::Invalid);
        assert_eq!(abstract_verdict(&abstract_api("undeliverable", "invalid_mailbox", false, false)), Verdict::Invalid);
        assert_eq!(abstract_verdict(&abstract_api("undeliverable", "unavailable_server", false, false)), Verdict::Unsure);
        assert_eq!(abstract_verdict(&abstract_api("unknown", "", false, false)), Verdict::Unsure);
        let malformed = json!({"email_deliverability": {"status": "undeliverable", "status_detail": "invalid_format"}, "email_quality": {"is_disposable": null}});
        assert_eq!(abstract_verdict(&malformed), Verdict::Invalid);
        // MailboxValidator's documented fields.
        let mbv = |fields: Value| {
            let mut body = json!({"is_syntax": true, "is_domain": true, "is_verified": true, "is_catchall": false, "is_disposable": false,
                "is_server_down": false, "is_greylisted": false, "status": true});
            body.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
            mbv_verdict(&body)
        };
        assert_eq!(mbv(json!({})), Verdict::Valid);
        assert_eq!(mbv(json!({"is_verified": false, "status": false})), Verdict::Invalid);
        assert_eq!(mbv(json!({"is_domain": false, "is_verified": null, "status": false})), Verdict::Invalid);
        assert_eq!(mbv(json!({"is_disposable": true})), Verdict::Invalid);
        assert_eq!(mbv(json!({"is_catchall": true})), Verdict::Unsure);
        assert_eq!(mbv(json!({"is_verified": null, "is_server_down": true, "status": false})), Verdict::Unsure);
        // A role address that exists, which MailboxValidator's status turns down.
        assert_eq!(mbv(json!({"status": false})), Verdict::Unsure);
        // MyEmailVerifier's documented answers, with its flags as strings.
        let mev = |status: &str, flags: Value| {
            let mut body = json!({"Status": status, "catch_all": "false", "Disposable_Domain": "false", "Greylisted": "false"});
            body.as_object_mut().unwrap().extend(flags.as_object().unwrap().clone());
            mev_verdict(&body)
        };
        assert_eq!(mev("Valid", json!({})), Verdict::Valid);
        assert_eq!(mev("Invalid", json!({})), Verdict::Invalid);
        assert_eq!(mev("Valid", json!({"Disposable_Domain": "true"})), Verdict::Invalid);
        assert_eq!(mev("Catch-all", json!({"catch_all": "true"})), Verdict::Unsure);
        assert_eq!(mev("Valid", json!({"catch_all": "true"})), Verdict::Unsure);
        assert_eq!(mev("Unknown", json!({"Greylisted": "true"})), Verdict::Unsure);
        // Hunter's test key answers with this person whatever was asked.
        let found = json!({"data": {"email": "Richard@piedpiper.com", "score": 99, "verification": {"status": "valid"}}});
        assert_eq!(hunter_found(&found, "piedpiper.com"), Some(Found { email: "richard@piedpiper.com".into(), verified: true }));
        assert_eq!(hunter_found(&found, "stripe.com"), None);
        let unsure = json!({"data": {"email": "r@piedpiper.com", "score": 40, "verification": {"status": "accept_all"}}});
        assert_eq!(hunter_found(&unsure, "piedpiper.com"), None);
        assert_eq!(hunter_found(&json!({"data": {"email": null}}), "piedpiper.com"), None);
        // Voila Norbert's documented answer, by score.
        let norbert = |score: i64| json!({"id": 12345, "searching": false, "email": {"email": "Fake@Example.com", "score": score, "is_done": true}});
        assert_eq!(norbert_found(&norbert(100), "example.com"), Some(Found { email: "fake@example.com".into(), verified: true }));
        assert_eq!(norbert_found(&norbert(80), "example.com"), Some(Found { email: "fake@example.com".into(), verified: false }));
        assert_eq!(norbert_found(&norbert(5), "example.com"), None);
        assert_eq!(norbert_found(&norbert(100), "other.com"), None);
        assert_eq!(norbert_found(&json!({"id": 1, "searching": true, "email": null}), "example.com"), None);
        // Skrapp's documented answer, by quality.
        let skrapp = |status: &str| json!({"email": "Jane.Doe@Example.com", "pattern": "{first}.{last}", "quality": {"status": status}});
        assert_eq!(skrapp_found(&skrapp("valid"), "example.com"), Some(Found { email: "jane.doe@example.com".into(), verified: true }));
        assert_eq!(skrapp_found(&skrapp("catch-all"), "example.com"), Some(Found { email: "jane.doe@example.com".into(), verified: false }));
        assert_eq!(skrapp_found(&skrapp("unknown"), "example.com"), None);
        assert_eq!(skrapp_found(&skrapp("valid"), "other.com"), None);
        assert_eq!(skrapp_found(&serde_json::Value::Null, "example.com"), None);
        // Lusha's documented answer, by grade, and a contact it couldn't find.
        let lusha = |grade: &str| json!({"results": [{"id": "1", "emails": [
            {"email": "jane@gmail.com", "type": "private", "confidence": "A+"},
            {"email": "Jane.Doe@Example.com", "type": "work", "confidence": grade},
        ]}], "billing": {"creditsCharged": 2}});
        assert_eq!(lusha_found(&lusha("A+"), "example.com"), Some(Found { email: "jane.doe@example.com".into(), verified: true }));
        assert_eq!(lusha_found(&lusha("A"), "example.com"), Some(Found { email: "jane.doe@example.com".into(), verified: false }));
        assert_eq!(lusha_found(&lusha("B"), "example.com"), None);
        assert_eq!(lusha_found(&lusha("A+"), "other.com"), None);
        assert_eq!(lusha_found(&json!({"results": [{"error": {"code": "NOT_FOUND"}}]}), "example.com"), None);
        let unfinished = json!({"id": 1, "searching": true, "email": {"email": "fake@example.com", "score": 100}});
        assert_eq!(norbert_found(&unfinished, "example.com"), None);
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
        // Apollo's people match: a verified work address, a guessed one, a locked one and no match.
        let person = |email: &str, status: &str| json!({"person": {"first_name": "Jordan", "email": email, "email_status": status}});
        let found = Some(Found { email: "jordan.blake@northstar.io".into(), verified: true });
        assert_eq!(apollo_found(&person("Jordan.Blake@northstar.io", "verified"), "northstar.io"), found);
        assert_eq!(apollo_found(&person("jordan.blake@northstar.io", "guessed"), "northstar.io"), None);
        assert_eq!(apollo_found(&person("email_not_unlocked@domain.com", "verified"), "northstar.io"), None);
        assert_eq!(apollo_found(&json!({"person": null}), "northstar.io"), None);
        // Real responses to a bad key, from both services.
        let nb = neverbounce_ok(&json!({"status": "auth_failure", "message": "Invalid API key 'secret123'"})).unwrap_err();
        assert!(!nb.to_string().contains("secret123"));
        let mv = millionverifier_ok(&json!({"result": "", "error": "Insufficient credits"})).unwrap_err();
        assert_eq!(mv.to_string(), "Insufficient credits");
        assert!(millionverifier_ok(&json!({"result": "error", "error": "apikey_not_found"})).is_err());
        assert!(millionverifier_ok(&json!({"result": "ok", "error": ""})).is_ok());
    }
}
