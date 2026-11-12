//! Checks a lead's address with an email verification service before its first cold email, so addresses that don't
//! exist fail in the app instead of bouncing and hurting the mailbox's reputation. NeverBounce, MillionVerifier, Kickbox,
//! Reoon, DeBounce, Bouncify, QuickEmailVerification, MailboxValidator and MyEmailVerifier take the key in the URL, so every error drops its URL before it's logged or shown. Hunter, Bouncer, Emailable,
//! Clearout, EmailListVerify and Abstract take it in a header. It also finds the real address behind a guessed one, with Hunter, Findymail, Prospeo,
//! Anymail Finder, LeadMagic, Apollo, Voila Norbert, Skrapp, Lusha, People Data Labs, RocketReach, Snov.io, Dropcontact,
//! ContactOut or Kaspr, some of which also return mobile numbers.

use std::sync::LazyLock;
use std::time::Duration;

use reqwest::Url;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::crypto;
use crate::hooks::{self, Alert};
use crate::models::Lead;
use crate::state::AppState;
use crate::waterfall::{self, Asked};

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

/// The project's Hunter API key, from Email finder or Email verification, or None when neither has Hunter.
pub async fn hunter_key(state: &AppState, project_id: Uuid) -> anyhow::Result<Option<String>> {
    let enc: Option<String> = sqlx::query_scalar(
        "select key_enc from project_finders where project_id = $1 and provider = 'hunter'
         union all select verifier_key_enc from project_settings where project_id = $1 and verifier = 'hunter' limit 1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?;
    enc.map(|enc| crypto::decrypt(&state.cfg.app_secret, &enc)).transpose()
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
    PeopleDataLabs,
    RocketReach,
    Snov,
    Dropcontact,
    ContactOut,
    Kaspr,
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
            "peopledatalabs" => Some(Finder::PeopleDataLabs),
            "rocketreach" => Some(Finder::RocketReach),
            "snov" => Some(Finder::Snov),
            "dropcontact" => Some(Finder::Dropcontact),
            "contactout" => Some(Finder::ContactOut),
            "kaspr" => Some(Finder::Kaspr),
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
            Finder::PeopleDataLabs => "People Data Labs",
            Finder::RocketReach => "RocketReach",
            Finder::Snov => "Snov.io",
            Finder::Dropcontact => "Dropcontact",
            Finder::ContactOut => "ContactOut",
            Finder::Kaspr => "Kaspr",
        }
    }

    /// The finders whose answer marks a number as a mobile: People Data Labs' `mobile_phone`, RocketReach's and Lusha's
    /// phones of type `mobile`, and Prospeo's `mobile`. ContactOut and Kaspr list phones without saying which are mobiles,
    /// so a landline could come back, and their phones aren't used. Keep in sync with PHONE_FINDERS on the integrations page.
    pub fn finds_phones(self) -> bool {
        matches!(self, Finder::PeopleDataLabs | Finder::RocketReach | Finder::Lusha | Finder::Prospeo)
    }

    /// Kaspr only looks people up by their LinkedIn profile, and Lusha needs a last name or one, since it charges for every
    /// search, even one that finds no one.
    pub fn can_search(self, person: &Person<'_>) -> bool {
        match self {
            Finder::Kaspr => !person.linkedin.is_empty(),
            Finder::Lusha => !person.last.is_empty() || !person.linkedin.is_empty(),
            _ => true,
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
            // People Data Labs has no free account call, and only charges for a match, so this asks for nobody.
            Finder::PeopleDataLabs => {
                let url = Url::parse_with_params(&format!("{PDL}/person/enrich"), [("email", "nobody@example.com"), ("min_likelihood", "10")])?;
                pdl_call(client()?.get(url), key).await.map(|_| ())
            }
            Finder::RocketReach => rocketreach_call(client()?.get(format!("{ROCKETREACH}/account")), key).await.map(|_| ()),
            Finder::Snov => snov_call(client()?.get(format!("{SNOV}/v1/get-balance")), &snov_token(key).await?).await.map(|_| ()),
            Finder::Dropcontact => dropcontact_call(client()?.get(format!("{DROPCONTACT}/v1/account/credits")), key).await.map(|_| ()),
            Finder::ContactOut => {
                let url = Url::parse_with_params(&format!("{CONTACTOUT}/v1/stats"), [("period", chrono::Utc::now().format("%Y-%m").to_string())])?;
                contactout_call(client()?.get(url), key).await.map(|_| ())
            }
            Finder::Kaspr => kaspr_call(client()?.get(format!("{KASPR}/keys/verifyKey")), key).await.map(|_| ()),
        }
    }

    /// The person's address at `domain`, or None when the service has nothing it's sure of there. Prospeo, Apollo and Lusha
    /// also match on the LinkedIn profile, when the lead has one.
    pub(crate) async fn find(self, key: &str, person: &Person<'_>, domain: &str, phones: bool) -> anyhow::Result<Search> {
        let Person { first, last, linkedin, .. } = *person;
        let found = |found| Search { found, credits: None, phone: None };
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
                // A mobile costs 10 credits, with the email included.
                if phones {
                    body["enrich_mobile"] = json!(true);
                }
                let body = prospeo_call(client()?.post("https://api.prospeo.io/enrich-person").json(&body), key).await?;
                Ok(Search { found: prospeo_found(&body, domain), credits: None, phone: prospeo_mobile(&body) })
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
                        Ok(Search { found: anymailfinder_found(&body, domain), credits: body["credits_charged"].as_i64(), phone: None })
                    }
                }
            }
            Finder::LeadMagic => {
                let mut body = if last.is_empty() { json!({ "full_name": first }) } else { json!({ "first_name": first, "last_name": last }) };
                body["domain"] = json!(domain);
                let req = client()?.post(format!("{LEADMAGIC}/people/email-finder")).json(&body);
                let body = leadmagic_call(req, key).await?;
                Ok(Search { found: leadmagic_found(&body, domain), credits: body["credits_consumed"].as_i64(), phone: None })
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
                let reveal = if phones { json!(["emails", "phones"]) } else { json!(["emails"]) };
                let req = client()?.post(format!("{LUSHA}/contacts/search-and-enrich")).json(&json!({ "contacts": [contact], "reveal": reveal }));
                let body = lusha_call(req, key).await?;
                let phone = mobile(&body["results"][0]["phones"], "number");
                Ok(Search { found: lusha_found(&body, domain), credits: body["billing"]["creditsCharged"].as_i64(), phone })
            }
            Finder::PeopleDataLabs => {
                let mut query = vec![("first_name", first), ("last_name", last), ("company", domain), ("min_likelihood", "6")];
                if !linkedin.is_empty() {
                    query.push(("profile", linkedin));
                }
                let body = pdl_call(client()?.get(Url::parse_with_params(&format!("{PDL}/person/enrich"), query)?), key).await?;
                // The person's mobile, unlike `phone_numbers`, which lists every kind. A plan without contact data answers `true`.
                let phone = body["data"]["mobile_phone"].as_str().map(str::to_string);
                Ok(Search { found: pdl_found(&body, domain), credits: None, phone })
            }
            Finder::RocketReach => {
                let name = format!("{first} {last}");
                let employer = if person.company.is_empty() { domain } else { person.company };
                let mut query = vec![("name", name.trim()), ("current_employer", employer)];
                if !linkedin.is_empty() {
                    query.push(("linkedin_url", linkedin));
                }
                let url = Url::parse_with_params(&format!("{ROCKETREACH}/person/lookup"), query)?;
                let mut body = rocketreach_call(client()?.get(url), key).await?;
                // RocketReach matches on the employer's name and has no domain field, so a lead without a company name is looked
                // up at its domain. A lookup still searching is read again from its status, for up to half a minute.
                for _ in 0..6 {
                    let Some(id) = body["id"].as_i64().filter(|_| rocketreach_searching(&body)) else { break };
                    tokio::time::sleep(POLL_WAIT).await;
                    let url = Url::parse_with_params(&format!("{ROCKETREACH}/person/checkStatus"), [("ids", id.to_string())])?;
                    let list = rocketreach_call(client()?.get(url), key).await?;
                    body = list.as_array().and_then(|l| l.first().cloned()).unwrap_or(Value::Null);
                }
                let phones = &body["phones"];
                let phone = mobile(phones, "e164").or_else(|| mobile(phones, "number"));
                Ok(Search { found: rocketreach_found(&body, domain), credits: None, phone })
            }
            Finder::Snov => {
                let token = snov_token(key).await?;
                let row = json!({ "first_name": first, "last_name": last, "domain": domain });
                let start = client()?.post(format!("{SNOV}/v2/emails-by-domain-by-name/start")).json(&json!({ "rows": [row] }));
                let started = snov_call(start, &token).await?;
                let hash = started["data"]["task_hash"].as_str().ok_or_else(|| anyhow::anyhow!("Snov.io didn't start the search"))?.to_string();
                // Its result is read until it's done, for up to half a minute. One still going counts as nothing found.
                let mut body = Value::Null;
                for _ in 0..6 {
                    tokio::time::sleep(POLL_WAIT).await;
                    let url = Url::parse_with_params(&format!("{SNOV}/v2/emails-by-domain-by-name/result"), [("task_hash", hash.as_str())])?;
                    body = snov_call(client()?.get(url), &token).await?;
                    if body["status"] == "not_enough_credits" {
                        return Err(out_of_credits());
                    }
                    if body["status"] != "in_progress" {
                        break;
                    }
                }
                Ok(found(snov_found(&body, domain)))
            }
            Finder::Dropcontact => {
                let mut contact = json!({ "first_name": first, "last_name": last, "website": domain });
                if !person.company.is_empty() {
                    contact["company"] = json!(person.company);
                }
                if !linkedin.is_empty() {
                    contact["linkedin"] = json!(linkedin);
                }
                let req = client()?.post(format!("{DROPCONTACT}/v1/enrich/all")).json(&json!({ "data": [contact], "siren": false, "language": "en" }));
                let submitted = dropcontact_call(req, key).await?;
                let id = submitted["request_id"].as_str().ok_or_else(|| anyhow::anyhow!("Dropcontact didn't take the search"))?.to_string();
                // It enriches in the background. One still going after three minutes counts as nothing found, and a credit is
                // only spent on a verified address.
                for _ in 0..DROPCONTACT_POLLS {
                    tokio::time::sleep(DROPCONTACT_WAIT).await;
                    let body = dropcontact_call(client()?.get(format!("{DROPCONTACT}/v1/enrich/all/{id}")), key).await?;
                    if body["success"] == true {
                        return Ok(found(dropcontact_found(&body, domain)));
                    }
                }
                Ok(found(None))
            }
            Finder::ContactOut => {
                let mut body = json!({ "first_name": first, "last_name": last, "company_domain": [domain], "include": ["work_email"] });
                if !person.company.is_empty() {
                    body["company"] = json!([person.company]);
                }
                if !linkedin.is_empty() {
                    body["linkedin_url"] = json!(linkedin);
                }
                let body = contactout_call(client()?.post(format!("{CONTACTOUT}/v1/people/enrich")).json(&body), key).await?;
                Ok(found(contactout_found(&body, domain)))
            }
            Finder::Kaspr if linkedin.is_empty() => Ok(found(None)),
            Finder::Kaspr => {
                // It takes the profile's public id or its link.
                let id = linkedin_id(linkedin).unwrap_or(linkedin);
                let name = format!("{first} {last}");
                let req = client()?.post(format!("{KASPR}/profile/linkedin")).json(&json!({ "id": id, "name": name.trim(), "dataToGet": ["workEmail"] }));
                Ok(found(kaspr_found(&kaspr_call(req, key).await?, domain)))
            }
        }
    }
}

/// A finder that can't take calls for a while: out of credits, or past its rate limit. The waterfall skips it for `wait`
/// rather than failing the lead.
#[derive(Debug)]
pub struct Paused {
    why: &'static str,
    pub wait: Duration,
}

impl std::fmt::Display for Paused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.why)
    }
}

impl std::error::Error for Paused {}

const HOUR: Duration = Duration::from_secs(3600);
const RATE_LIMITED: &str = "it's past its rate limit, so it's skipped until the limit resets";

fn out_of_credits() -> anyhow::Error {
    Paused { why: "the account is out of credits, so it's skipped for an hour", wait: HOUR }.into()
}

/// A finder's error answer, which a message about credits turns into a pause.
fn failed(status: reqwest::StatusCode, said: Option<&str>) -> anyhow::Result<()> {
    if status.is_success() {
        return Ok(());
    }
    let said = said.unwrap_or(status.as_str());
    let lower = said.to_lowercase();
    // Only failures get here, and one that mentions credits, like "Not enough credits" or "no lookup credits remaining", means
    // there are none left.
    if lower.contains("credit") {
        return Err(out_of_credits());
    }
    anyhow::bail!("{said}")
}

/// Every finder's answer goes through here. A 402 is out of credits. A 429 waits as long as Retry-After, or a rate limit
/// reset header, says, or a minute when neither does, and never more than an hour.
fn limits(res: &reqwest::Response) -> anyhow::Result<()> {
    match res.status().as_u16() {
        402 => Err(out_of_credits()),
        429 => {
            let header = |name: &str| res.headers().get(name).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<f64>().ok());
            let reset = ["x-ratelimit-reset", "ratelimit-reset", "x-rate-limit-reset"].into_iter().find_map(header);
            // A reset header is either seconds from now or a Unix time.
            let now = chrono::Utc::now().timestamp() as f64;
            let secs = header("retry-after").or(reset.map(|r| if r > 1e9 { r - now } else { r })).unwrap_or(60.0);
            Err(Paused { why: RATE_LIMITED, wait: Duration::from_secs_f64(secs.clamp(1.0, 3600.0)) }.into())
        }
        _ => Ok(()),
    }
}

/// Who a finder looks for.
#[derive(Clone, Copy)]
pub struct Person<'a> {
    pub first: &'a str,
    pub last: &'a str,
    /// The LinkedIn profile, or "".
    pub linkedin: &'a str,
    /// The company's name, or "", for the finders that match on it.
    pub company: &'a str,
}

impl<'a> Person<'a> {
    /// A lead imported with only a full name has it split at the first space.
    pub fn of(lead: &'a Lead) -> Person<'a> {
        let (first, last) = if lead.first_name.trim().is_empty() {
            lead.full_name.trim().split_once(' ').unwrap_or((lead.full_name.trim(), ""))
        } else {
            (lead.first_name.as_str(), lead.last_name.as_str())
        };
        Person { first: first.trim(), last: last.trim(), linkedin: lead.linkedin_url.trim(), company: lead.company_name.trim() }
    }
}

/// A finder's answer about one person.
pub struct Search {
    pub found: Option<Found>,
    /// The credits it said it charged, for the services whose answer says.
    pub credits: Option<i64>,
    /// A mobile number as the finder wrote it, when it was asked for phones and had one.
    pub phone: Option<String>,
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

/// Looks up the lead's real address at the guessed address's domain with the project's email finders in order, then Hunter
/// connected under Email verification, like the waterfall does. The first address one is sure of wins.
pub async fn find(state: &AppState, project_id: Uuid, lead: &Lead, guessed: &str) -> Lookup {
    let Some((_, domain)) = guessed.rsplit_once('@') else { return Lookup::Nothing };
    let domain = domain.to_lowercase();
    let finders = match waterfall::finders(&state.db, &state.cfg.app_secret, project_id).await {
        Ok(finders) => finders,
        Err(e) => {
            tracing::warn!(error = %e, "couldn't load the email finders");
            return Lookup::Nothing;
        }
    };
    let mut under_verifier_failed = false;
    for finder in &finders {
        match waterfall::ask(&state.db, project_id, finder, &Person::of(lead), &domain, false).await {
            Asked::Answer(Search { found: Some(found), .. }) => return Lookup::Found(found),
            Asked::Failed(_) => under_verifier_failed |= matches!(finder.alert, Alert::Verifier),
            Asked::Answer(_) | Asked::Paused(_) | Asked::Skipped(_) => {}
        }
    }
    if under_verifier_failed { Lookup::Failed } else { Lookup::Nothing }
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
    limits(&res)?;
    let status = res.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(Value::Null);
    }
    let body: Value = res.json().await.unwrap_or_default();
    failed(status, body["error"].as_str().or(body["message"].as_str()))?;
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
    limits(&res)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    let code = body["error_code"].as_str().map(str::to_string);
    match code.as_deref() {
        Some("NO_MATCH") => Ok(Value::Null),
        Some("INSUFFICIENT_CREDITS") => Err(out_of_credits()),
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

/// Prospeo's mobile, when it's verified and revealed in full.
fn prospeo_mobile(body: &Value) -> Option<String> {
    let mobile = &body["person"]["mobile"];
    let usable = mobile["revealed"] == true && mobile["status"] == "VERIFIED";
    mobile["mobile"].as_str().filter(|m| usable && !m.contains('*')).map(str::to_string)
}

/// Anymail Finder explains a failure in `message`, like "Invalid API key. Copy it again from ..." for a 401.
async fn anymailfinder_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = send(req.header("Authorization", key)).await?;
    limits(&res)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    failed(status, body["message"].as_str())?;
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
    limits(&res)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    failed(status, body["detail"].as_str().or(body["message"].as_str()))?;
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
    limits(&res)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    failed(status, body["error_details"]["message"].as_str().or(body["error"].as_str()))?;
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
    limits(&res)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
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
    limits(&res)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    if body["email_limit_exeeded"] == true {
        return Err(out_of_credits());
    }
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
    limits(&res)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    let message = body["message"].as_str().unwrap_or(status.as_str());
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED && message != "Invalid API key format", "the API key was rejected");
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

/// How long a finder that answers later is left before it's asked again. Tests don't wait.
#[cfg(not(test))]
const POLL_WAIT: Duration = Duration::from_secs(5);
#[cfg(test)]
const POLL_WAIT: Duration = Duration::from_millis(10);

/// The first number at `field` in a finder's list of phones whose `type` is `mobile`.
fn mobile(phones: &Value, field: &str) -> Option<String> {
    phones.as_array()?.iter().filter(|p| p["type"] == "mobile").find_map(|p| p[field].as_str().map(str::to_string))
}

/// The public id in a LinkedIn profile link, like "jane-doe" in linkedin.com/in/jane-doe/.
fn linkedin_id(url: &str) -> Option<&str> {
    let rest = url.split_once("/in/")?.1;
    let id = rest.split(['/', '?', '#']).next()?.trim();
    (!id.is_empty()).then_some(id)
}

const PDL: &str = "https://api.peopledatalabs.com/v5";

/// People Data Labs answers a match with `{status: 200, likelihood, data}`, and a failure with `{error: {type, message}}`:
/// a 404 is nobody found, which costs nothing, a 401 a wrong key and a 402 a used-up plan.
async fn pdl_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = send(req.header("X-Api-Key", key)).await.map_err(reqwest::Error::without_url)?;
    limits(&res)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(Value::Null);
    }
    let body: Value = res.json().await.unwrap_or_default();
    failed(status, body["error"]["message"].as_str())?;
    Ok(body)
}

/// People Data Labs doesn't verify addresses, so its work address is kept for the verifier to check before sending. On a
/// plan without contact fields the field is `true` instead of the address, which isn't one.
fn pdl_found(body: &Value, domain: &str) -> Option<Found> {
    at_domain(body["data"]["work_email"].as_str(), domain).map(|email| Found { email, verified: false })
}

const ROCKETREACH: &str = "https://api.rocketreach.co/api/v2";

/// RocketReach explains a failure in `detail` or `message`. A 401 is a wrong key. Running out of lookups is documented as a
/// 402 in one place and a 403 in another, and both mention credits.
async fn rocketreach_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = send(req.header("Api-Key", key)).await.map_err(reqwest::Error::without_url)?;
    limits(&res)?;
    let status = res.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(Value::Null);
    }
    let body: Value = res.json().await.unwrap_or_default();
    let said = body["detail"].as_str().or(body["message"].as_str());
    failed(status, said).map_err(|e| if e.is::<Paused>() || status != reqwest::StatusCode::UNAUTHORIZED { e } else { anyhow::anyhow!("the API key was rejected") })?;
    Ok(body)
}

fn rocketreach_searching(body: &Value) -> bool {
    matches!(body["status"].as_str(), Some("searching" | "progress" | "waiting" | "queued"))
}

/// RocketReach grades each address and says whether its mail server took it (`smtp_valid`). A professional one at the
/// domain that's valid is verified, and one graded A or A- is kept for the verifier to check before sending.
fn rocketreach_found(body: &Value, domain: &str) -> Option<Found> {
    if rocketreach_searching(body) {
        return None;
    }
    let emails = body["emails"].as_array()?;
    let work = |e: &&Value| e["type"] != "personal";
    let valid = emails.iter().filter(work).find_map(|e| at_domain(e["email"].as_str().filter(|_| e["smtp_valid"] == "valid"), domain));
    let graded = || emails.iter().filter(work).find_map(|e| at_domain(e["email"].as_str().filter(|_| matches!(e["grade"].as_str(), Some("A" | "A-"))), domain));
    valid.map(|email| Found { email, verified: true }).or_else(|| graded().map(|email| Found { email, verified: false }))
}

const SNOV: &str = "https://api.snov.io";

/// Snov.io's tokens last an hour, so each key's is kept until a minute before it runs out.
static SNOV_TOKENS: LazyLock<std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>> = LazyLock::new(Default::default);

/// Signs in with the key's client ID and secret, which it holds as "id:secret".
async fn snov_token(key: &str) -> anyhow::Result<String> {
    if let Some((token, until)) = SNOV_TOKENS.lock().unwrap().get(key)
        && *until > std::time::Instant::now()
    {
        return Ok(token.clone());
    }
    let (id, secret) = key.split_once(':').ok_or_else(|| anyhow::anyhow!("the API user ID and secret were rejected"))?;
    let form = [("grant_type", "client_credentials"), ("client_id", id), ("client_secret", secret)];
    let res = send(client()?.post(format!("{SNOV}/v1/oauth/access_token")).form(&form)).await.map_err(reqwest::Error::without_url)?;
    limits(&res)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    let token = body["access_token"].as_str().filter(|_| status.is_success());
    let token = token.ok_or_else(|| anyhow::anyhow!("the API user ID and secret were rejected"))?.to_string();
    let lasts = Duration::from_secs(body["expires_in"].as_u64().unwrap_or(3600).saturating_sub(60));
    SNOV_TOKENS.lock().unwrap().insert(key.to_string(), (token.clone(), std::time::Instant::now() + lasts));
    Ok(token)
}

/// Snov.io answers a failure with `{success: false, message}` or `{errors}`. Its limit is 60 calls a minute.
async fn snov_call(req: reqwest::RequestBuilder, token: &str) -> anyhow::Result<Value> {
    let res = send(req.bearer_auth(token)).await.map_err(reqwest::Error::without_url)?;
    limits(&res)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API user ID and secret were rejected");
    let body: Value = res.json().await.unwrap_or_default();
    let said = body["message"].as_str().or(body["errors"].as_str()).or(body["error"].as_str());
    failed(if body["success"] == false && status.is_success() { reqwest::StatusCode::BAD_REQUEST } else { status }, said)?;
    Ok(body)
}

/// Snov.io checks what it finds: `valid` is verified, and `unknown`, a catch-all domain or a server that blocks checks, is
/// kept for the verifier to check before sending. A search that found nothing has an empty `result`.
fn snov_found(body: &Value, domain: &str) -> Option<Found> {
    let results = body["data"][0]["result"].as_array()?;
    let with = |status: &str| results.iter().find_map(|r| at_domain(r["email"].as_str().filter(|_| r["smtp_status"] == status), domain));
    with("valid").map(|email| Found { email, verified: true }).or_else(|| with("unknown").map(|email| Found { email, verified: false }))
}

const DROPCONTACT: &str = "https://api.dropcontact.com";
/// Dropcontact asks to be read again every 30 seconds, so a search is read for up to 3 minutes.
#[cfg(not(test))]
const DROPCONTACT_WAIT: Duration = Duration::from_secs(30);
#[cfg(test)]
const DROPCONTACT_WAIT: Duration = Duration::from_millis(10);
const DROPCONTACT_POLLS: usize = 6;

/// Dropcontact answers a failure with `{error: true, reason}`, like a 403 "You submitted 1 valid items but only have 0
/// credits left", and a search still going with a 200 `{error: false, success: false, reason: "Request not ready yet..."}`.
/// A wrong key is a 401 with its reason in `detail`.
async fn dropcontact_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = send(req.header("X-Access-Token", key)).await.map_err(reqwest::Error::without_url)?;
    limits(&res)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    let status = if body["error"] == true && status.is_success() { reqwest::StatusCode::BAD_REQUEST } else { status };
    failed(status, body["reason"].as_str().or(body["detail"].as_str()))?;
    Ok(body)
}

/// Dropcontact qualifies each address as kind@domain type: `nominative@pro` is the person's own work address, which it
/// verified, and `catch-all@pro` one on a catch-all domain, kept for the verifier to check before sending. Generic,
/// personal and unverified ones aren't kept.
fn dropcontact_found(body: &Value, domain: &str) -> Option<Found> {
    let emails = body["data"][0]["email"].as_array()?;
    let with = |q: &str| emails.iter().find_map(|e| at_domain(e["email"].as_str().filter(|_| e["qualification"] == q), domain));
    with("nominative@pro").map(|email| Found { email, verified: true }).or_else(|| with("catch-all@pro").map(|email| Found { email, verified: false }))
}

const CONTACTOUT: &str = "https://api.contactout.com";

/// ContactOut takes the key in a `token` header, with the `authorization: basic` its examples send, and explains a failure
/// in `message`. A 401 is a wrong key, a 404 nobody found, a 403 "You're out of credits..." a used-up plan, and a 429 comes
/// with retry-after.
async fn contactout_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = send(req.header("token", key).header("authorization", "basic")).await.map_err(reqwest::Error::without_url)?;
    limits(&res)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(Value::Null);
    }
    let body: Value = res.json().await.unwrap_or_default();
    failed(status, body["message"].as_str().or(body["error"].as_str()))?;
    Ok(body)
}

/// ContactOut lists work addresses, and may say which it verified in `work_email_status`. A Verified one is kept as
/// verified, and another for the verifier to check before sending. Its phones are plain numbers, mostly mobiles.
fn contactout_found(body: &Value, domain: &str) -> Option<Found> {
    let profile = &body["profile"];
    let emails = profile["work_email"].as_array()?;
    let status = |e: &str| profile["work_email_status"][e].as_str().unwrap_or_default().to_lowercase();
    let verified = emails.iter().filter_map(Value::as_str).find(|e| status(e) == "verified");
    match verified {
        Some(email) => at_domain(Some(email), domain).map(|email| Found { email, verified: true }),
        None => emails.iter().filter_map(Value::as_str).find_map(|e| at_domain(Some(e), domain)).map(|email| Found { email, verified: false }),
    }
}

const KASPR: &str = "https://api.developers.kaspr.io";

/// Kaspr takes a bearer key and the API version, without which it answers as v1. A failure explains itself in `message`,
/// like a 402 "No credits left". A 429 blocks the key until the minute's, hour's or day's limit resets, which its headers
/// give in milliseconds.
async fn kaspr_call(req: reqwest::RequestBuilder, key: &str) -> anyhow::Result<Value> {
    let res = send(req.bearer_auth(key).header("accept-version", "v2.0")).await.map_err(reqwest::Error::without_url)?;
    if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let header = |name: &str| res.headers().get(name).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<f64>().ok());
        let spent = |window: &str| header(&format!("x-{window}-ratelimit-remaining")) == Some(0.0);
        let reset = match () {
            _ if spent("daily") => header("x-daily-ratelimit-reset"),
            _ if spent("hourly") => header("x-hourly-ratelimit-reset"),
            _ => header("x-minutely-reset"),
        };
        // Either milliseconds from now or a Unix time in milliseconds.
        let now = chrono::Utc::now().timestamp_millis() as f64;
        let wait = reset.map_or(60.0, |ms| if ms > 1e12 { (ms - now) / 1000.0 } else { ms / 1000.0 });
        return Err(Paused { why: RATE_LIMITED, wait: Duration::from_secs_f64(wait.clamp(1.0, 3600.0)) }.into());
    }
    limits(&res)?;
    let status = res.status();
    anyhow::ensure!(!matches!(status.as_u16(), 401 | 403), "the API key was rejected");
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(Value::Null);
    }
    let body: Value = res.json().await.unwrap_or_default();
    failed(status, body["message"].as_str())?;
    Ok(body)
}

/// Kaspr's best work address is `starryProfessionalEmail`, among `professionalEmails`. It doesn't say whether it's verified,
/// so the verifier checks it before sending.
fn kaspr_found(body: &Value, domain: &str) -> Option<Found> {
    let profile = &body["profile"];
    let listed = profile["professionalEmails"].as_array().into_iter().flatten().filter_map(Value::as_str);
    let mut emails = profile["starryProfessionalEmail"].as_str().into_iter().chain(listed);
    emails.find_map(|e| at_domain(Some(e), domain)).map(|email| Found { email, verified: false })
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
        // Only a number marked as a mobile is used, never a direct dial or an office line.
        let phones = json!([{"number": "+1 212-555-0100", "type": "direct dial"}, {"number": "+1 415-555-0134", "type": "mobile"}]);
        assert_eq!(mobile(&phones, "number").as_deref(), Some("+1 415-555-0134"));
        assert_eq!(mobile(&json!([{"number": "+1 212-555-0100", "type": "work"}, {"number": "+1 212-555-0101"}]), "number"), None);
        let prospeo = |revealed: bool, number: &str| json!({"person": {"mobile": {"status": "VERIFIED", "revealed": revealed, "mobile": number}}});
        assert_eq!(prospeo_mobile(&prospeo(true, "+14155550134")).as_deref(), Some("+14155550134"));
        assert_eq!(prospeo_mobile(&prospeo(false, "+1 415-3**-****")), None);
        assert!([Finder::ContactOut, Finder::Kaspr].iter().all(|f| !f.finds_phones()));
        // Real responses to a bad key, from both services.
        let nb = neverbounce_ok(&json!({"status": "auth_failure", "message": "Invalid API key 'secret123'"})).unwrap_err();
        assert!(!nb.to_string().contains("secret123"));
        let mv = millionverifier_ok(&json!({"result": "", "error": "Insufficient credits"})).unwrap_err();
        assert_eq!(mv.to_string(), "Insufficient credits");
        assert!(millionverifier_ok(&json!({"result": "error", "error": "apikey_not_found"})).is_err());
        assert!(millionverifier_ok(&json!({"result": "ok", "error": ""})).is_ok());
    }
}
