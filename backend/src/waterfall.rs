//! The email waterfall: finds an address for a lead that came without one, like the leads LinkedIn people search adds, with
//! no Claude run. The project's email finders go first, then the common address patterns, each checked with its verifier.
//! A worker runs it on new leads by itself, and "Find emails" on the Leads page queues leads for it, ahead of the rest.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures::{StreamExt, stream};
use sqlx::PgPool;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::crypto;
use crate::hooks::{self, Alert};
use crate::models::Lead;
use crate::state::AppState;
use crate::verify::{Finder, Person, Verdict, Verifier};

/// Leads an organization's projects search an hour, together, so one big LinkedIn search can't spend a month's finder and
/// verifier credits in an afternoon.
pub const HOURLY: i64 = 200;
/// Leads searched at the same time. Each makes a few calls in a row, and the services limit calls a second.
const PARALLEL: usize = 4;

/// What the worker picks up, on `l` (leads): leads someone queued, and leads never searched that have a name and a company
/// domain, still want email and are in a project with a finder or verifier. One without either waits until one is
/// connected, rather than being marked not found.
const WAITING: &str = "l.email is null and (l.email_search = 'queued' or (l.email_search = '' and l.full_name <> '' and l.company_domain is not null
       and l.status not in ('bounced', 'unsubscribed', 'do_not_contact', 'not_interested')
       and exists (select 1 from project_settings ps where ps.project_id = l.project_id and (ps.finder <> '' or ps.verifier <> ''))))";

/// The patterns tried, in this order, after any the domain's verified leads already follow.
pub const PATTERNS: [&str; 7] = ["first", "first.last", "flast", "firstlast", "first_last", "f.last", "last"];

static WAKE: Notify = Notify::const_new();

/// Starts the next round now, for leads someone just queued.
pub fn wake() {
    WAKE.notify_one();
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = tick(&state, HOURLY).await {
                tracing::error!(error = ?e, "email finding failed");
            }
            let _ = tokio::time::timeout(Duration::from_secs(60), WAKE.notified()).await;
        }
    });
}

/// Searches the waiting leads of every organization, up to `hourly` an organization over the last hour, queued ones first
/// and then oldest first. Demo projects have no organization, so they're left out.
pub async fn tick(state: &AppState, hourly: i64) -> anyhow::Result<()> {
    let orgs: Vec<Uuid> = sqlx::query_scalar(&format!(
        "select distinct p.org_id from leads l join projects p on p.id = l.project_id where p.org_id is not null and {WAITING}"
    ))
    .fetch_all(&state.db)
    .await?;
    for org in orgs {
        if let Err(e) = search_org(state, org, hourly).await {
            tracing::error!(error = ?e, %org, "email finding failed for an organization");
        }
    }
    Ok(())
}

/// Searches the organization started in the last hour.
pub async fn searched_last_hour(db: &PgPool, org_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar("select count(*) from email_search_attempts where org_id = $1 and at > now() - interval '1 hour'")
        .bind(org_id)
        .fetch_one(db)
        .await
}

async fn search_org(state: &AppState, org_id: Uuid, hourly: i64) -> anyhow::Result<()> {
    // Held for the round, so a second backend on the database skips this organization instead of searching its leads twice.
    let mut lock = state.db.begin().await?;
    let owned: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended('email-search:' || $1::text, 0))")
        .bind(org_id)
        .fetch_one(&mut *lock)
        .await?;
    let room = hourly - searched_last_hour(&state.db, org_id).await?;
    if !owned || room <= 0 {
        return Ok(());
    }
    let leads: Vec<Lead> = sqlx::query_as(&format!(
        "select l.* from leads l join projects p on p.id = l.project_id where p.org_id = $1 and {WAITING}
         order by l.email_search = 'queued' desc, l.created_at, l.id limit $2"
    ))
    .bind(org_id)
    .bind(room)
    .fetch_all(&state.db)
    .await?;
    // Counted before any call goes out, so the limit holds even when a search fails partway.
    sqlx::query("delete from email_search_attempts where org_id = $1 and at < now() - interval '1 day'").bind(org_id).execute(&state.db).await?;
    sqlx::query("insert into email_search_attempts (org_id) select $1 from generate_series(1, $2)")
        .bind(org_id)
        .bind(leads.len() as i32)
        .execute(&state.db)
        .await?;
    let mut done = stream::iter(leads).map(|lead| async move { (lead.id, search_and_save(state, &lead).await) }).buffer_unordered(PARALLEL);
    while let Some((lead, result)) = done.next().await {
        if let Err(e) = result {
            tracing::error!(error = ?e, %lead, "could not save an email search");
        }
    }
    Ok(())
}

/// How a search ended, as it's saved on the lead.
#[derive(Debug, PartialEq)]
pub struct Outcome {
    /// One of migration 0103's email_search values.
    pub state: &'static str,
    /// The finder's name, or the pattern a guess followed.
    pub source: String,
    pub note: String,
    /// The address to give the lead, with its email_status.
    pub email: Option<(String, &'static str)>,
    pub lookups: i32,
    pub credits: Option<i32>,
}

impl Outcome {
    fn ended(state: &'static str, note: impl Into<String>) -> Outcome {
        Outcome { state, source: String::new(), note: note.into(), email: None, lookups: 0, credits: None }
    }
}

pub async fn search_and_save(state: &AppState, lead: &Lead) -> anyhow::Result<Outcome> {
    let outcome = match search(state, lead).await {
        Ok(outcome) => outcome,
        Err(e) => Outcome::ended("failed", format!("{e:#}")),
    };
    match save(&state.db, lead, &outcome).await {
        // Another lead got the address while this search ran.
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23505") => {
            let email = outcome.email.as_ref().map(|(email, _)| email.as_str()).unwrap_or_default();
            let taken = Outcome { email: None, state: "duplicate", note: format!("{email} is already another lead's address."), ..outcome };
            save(&state.db, lead, &taken).await?;
            Ok(taken)
        }
        saved => saved.map(|()| outcome).map_err(Into::into),
    }
}

/// Leaves the lead alone when it got an address some other way while the search ran.
async fn save(db: &PgPool, lead: &Lead, o: &Outcome) -> sqlx::Result<()> {
    sqlx::query(
        "update leads set email = coalesce($2, email), email_status = coalesce($3, email_status), email_search = $4, email_source = $5,
           email_search_note = $6, email_search_lookups = $7, email_search_credits = $8, email_searched_at = now()
         where id = $1 and email is null",
    )
    .bind(lead.id)
    .bind(o.email.as_ref().map(|(email, _)| email))
    .bind(o.email.as_ref().map(|(_, status)| *status))
    .bind(o.state)
    .bind(&o.source)
    .bind(&o.note)
    .bind(o.lookups)
    .bind(o.credits)
    .execute(db)
    .await?;
    Ok(())
}

/// Runs the waterfall for one lead, and says what it found without saving it.
pub async fn search(state: &AppState, lead: &Lead) -> anyhow::Result<Outcome> {
    let person = Person::of(lead);
    if person.first.is_empty() {
        return Ok(Outcome::ended("not_found", "There's no name to search for."));
    }
    let Some(domain) = lead.company_domain.as_deref().and_then(crate::routes::util_input::domain) else {
        return Ok(Outcome::ended("not_found", "There's no company website to search at."));
    };
    let org_id: Option<Uuid> = sqlx::query_scalar("select org_id from projects where id = $1").bind(lead.project_id).fetch_one(&state.db).await?;
    if suppressed(&state.db, org_id, &domain).await? {
        return Ok(Outcome::ended("suppressed", format!("{domain} is on the do-not-contact list.")));
    }
    let mut run = Run::new(state, lead.project_id).await?;
    if run.finders.is_empty() && run.verifier.is_none() {
        return Ok(Outcome::ended("not_found", "No email finder or verifier is connected."));
    }

    let outcome = match run.ask_finders(&person, &domain).await {
        Some(found) if run.verifier.is_none() || found.email.as_ref().is_some_and(|(_, status)| *status == "verified") => found,
        // A finder's unconfirmed address is kept only when no guess verifies.
        Some(found) => match run.guess(org_id, lead, &person, &domain).await? {
            guessed if guessed.email.is_some() => guessed,
            _ => found,
        },
        None => run.guess(org_id, lead, &person, &domain).await?,
    };
    let outcome = match outcome.email.as_ref().map(|(email, _)| email.clone()) {
        Some(email) => settle(&state.db, org_id, lead, &email, outcome).await?,
        None => outcome,
    };
    Ok(Outcome { lookups: run.lookups, credits: run.credits, ..outcome })
}

/// The do-not-contact list holds addresses and domains alike.
async fn suppressed(db: &PgPool, org_id: Option<Uuid>, value: &str) -> sqlx::Result<bool> {
    let domain = value.rsplit('@').next().unwrap_or_default();
    sqlx::query_scalar("select exists (select 1 from suppressions where org_id = $1 and value in ($2, $3))")
        .bind(org_id)
        .bind(value.to_lowercase())
        .bind(domain.to_lowercase())
        .fetch_one(db)
        .await
}

/// A found address is only given to the lead when it's not on the do-not-contact list and no other lead in the project has
/// it, so nobody gets two first emails.
async fn settle(db: &PgPool, org_id: Option<Uuid>, lead: &Lead, email: &str, outcome: Outcome) -> sqlx::Result<Outcome> {
    if suppressed(db, org_id, email).await? {
        return Ok(Outcome { email: None, state: "suppressed", note: format!("{email} is on the do-not-contact list."), ..outcome });
    }
    let owner: Option<String> = sqlx::query_scalar("select full_name from leads where project_id = $1 and id <> $2 and lower(email::text) = $3")
        .bind(lead.project_id)
        .bind(lead.id)
        .bind(email)
        .fetch_optional(db)
        .await?;
    Ok(match owner {
        Some(name) => {
            let whose = if name.trim().is_empty() { "another lead's".to_string() } else { format!("{}'s", name.trim()) };
            Outcome { email: None, state: "duplicate", note: format!("{email} is already {whose} address."), ..outcome }
        }
        None => outcome,
    })
}

/// Whether another lead in the project has the address.
async fn taken(db: &PgPool, lead: &Lead, email: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar("select exists (select 1 from leads where project_id = $1 and id <> $2 and lower(email::text) = $3)")
        .bind(lead.project_id)
        .bind(lead.id)
        .bind(email.to_lowercase())
        .fetch_one(db)
        .await
}

struct Service<T> {
    service: T,
    key: String,
    /// The stored ciphertext, which alerts match on.
    enc: String,
    alert: Alert,
}

/// One lead's search: the project's services and what they cost.
struct Run<'a> {
    db: &'a PgPool,
    project_id: Uuid,
    finders: Vec<Service<Finder>>,
    verifier: Option<Service<Verifier>>,
    lookups: i32,
    credits: Option<i32>,
    /// The first service failure, which explains a search that found nothing.
    failure: Option<String>,
    /// Verdicts so far, so an address the finders' turn checked isn't paid for again when guessing.
    verdicts: HashMap<String, Verdict>,
}

impl<'a> Run<'a> {
    /// The project's finder, then Hunter when it's connected under Email verification, since Hunter finds addresses too.
    async fn new(state: &'a AppState, project_id: Uuid) -> anyhow::Result<Run<'a>> {
        let row: Option<(String, String, String, String)> =
            sqlx::query_as("select finder, finder_key_enc, verifier, verifier_key_enc from project_settings where project_id = $1")
                .bind(project_id)
                .fetch_optional(&state.db)
                .await?;
        let (finder, finder_enc, verifier, verifier_enc) = row.unwrap_or_default();
        let secret = &state.cfg.app_secret;
        let mut finders = Vec::new();
        if let Some(service) = Finder::parse(&finder) {
            finders.push(Service { service, key: crypto::decrypt(secret, &finder_enc)?, enc: finder_enc, alert: Alert::Finder });
        }
        if verifier == "hunter" && finder != "hunter" {
            finders.push(Service { service: Finder::Hunter, key: crypto::decrypt(secret, &verifier_enc)?, enc: verifier_enc.clone(), alert: Alert::Verifier });
        }
        let verifier = match Verifier::parse(&verifier) {
            Some(service) => Some(Service { service, key: crypto::decrypt(secret, &verifier_enc)?, enc: verifier_enc, alert: Alert::Verifier }),
            None => None,
        };
        Ok(Run { db: &state.db, project_id, finders, verifier, lookups: 0, credits: None, failure: None, verdicts: HashMap::new() })
    }

    fn failed(&mut self, service: &str, e: &anyhow::Error) {
        tracing::warn!(error = %e, service, "email search call failed");
        self.failure.get_or_insert_with(|| format!("{service}: {e:#}"));
    }

    /// Each finder in turn, stopping at the first address it, or the verifier, says is deliverable. One that's found but
    /// unconfirmed is kept in case nothing better turns up, since a finder's answer beats a guess.
    async fn ask_finders(&mut self, person: &Person<'_>, domain: &str) -> Option<Outcome> {
        let mut fallback = None;
        for i in 0..self.finders.len() {
            let Service { service, key, enc, alert } = &self.finders[i];
            let (label, enc, alert) = (service.label(), enc.clone(), *alert);
            self.lookups += 1;
            let result = service.find(key, person, domain).await;
            // A Hunter key under Email verification shows only its failures there, like the sender's lookup.
            let shown = result.as_ref().map(|_| ()).map_err(|e| anyhow::anyhow!("{e:#}"));
            if matches!(alert, Alert::Finder) || shown.is_err() {
                hooks::record(self.db, self.project_id, alert, &enc, &shown).await;
            }
            let search = match result {
                Ok(search) => search,
                Err(e) => {
                    self.failed(label, &e);
                    continue;
                }
            };
            if let Some(charged) = search.credits {
                *self.credits.get_or_insert(0) += charged as i32;
            }
            let Some(found) = search.found else { continue };
            if found.verified {
                return Some(found_by(label, found.email, "verified", format!("Found by {label}.")));
            }
            match self.check(&found.email).await {
                Some(Verdict::Valid) => {
                    let checker = self.verifier.as_ref().map_or("", |v| v.service.label());
                    return Some(found_by(label, found.email, "verified", format!("Found by {label}, verified by {checker}.")));
                }
                Some(Verdict::Invalid) => {}
                // The sender checks it again before the first email, as it does any found address.
                Some(Verdict::Unsure) | None => {
                    fallback.get_or_insert_with(|| found_by(label, found.email, "found", format!("Found by {label}, not verified yet.")));
                }
            }
        }
        fallback
    }

    /// The verifier's verdict, or None when there's no verifier or it failed. Every call is shown on the integrations page
    /// like a check before sending.
    async fn check(&mut self, email: &str) -> Option<Verdict> {
        if let Some(verdict) = self.verdicts.get(email) {
            return Some(*verdict);
        }
        let Service { service, key, enc, alert } = self.verifier.as_ref()?;
        self.lookups += 1;
        let result = service.verify(key, email).await;
        let shown = result.as_ref().map(|_| ()).map_err(|e| anyhow::anyhow!("{e:#}"));
        hooks::record(self.db, self.project_id, *alert, enc, &shown).await;
        let label = service.label();
        let verdict = result.map_err(|e| self.failed(label, &e)).ok()?;
        self.verdicts.insert(email.to_string(), verdict);
        Some(verdict)
    }

    /// The common patterns at `domain`, checked with the verifier, the pattern of the domain's verified leads first. A
    /// catch-all domain says yes to every address, so it's skipped: known from an earlier search, or found when an
    /// unsure answer comes back and a made-up address gets the same.
    async fn guess(&mut self, org_id: Option<Uuid>, lead: &Lead, person: &Person<'_>, domain: &str) -> anyhow::Result<Outcome> {
        let finders: Vec<&str> = self.finders.iter().map(|f| f.service.label()).collect();
        let finders = finders.join(" and ");
        let Some(checker) = self.verifier.as_ref().map(|v| v.service.label()) else {
            return Ok(self.nothing(format!("{finders} found no address, and there's no verifier to check a guess.")));
        };
        let catch_all = || Outcome::ended("catch_all", format!("{domain} accepts mail for any address, so a guess can't be checked."));
        let known_catch_all: bool = sqlx::query_scalar(
            "select exists (select 1 from leads where project_id = $1 and id <> $2 and company_domain = $3 and email_search = 'catch_all')",
        )
        .bind(lead.project_id)
        .bind(lead.id)
        .bind(domain)
        .fetch_one(self.db)
        .await?;
        if known_catch_all {
            return Ok(catch_all());
        }
        let known = domain_pattern(self.db, lead.project_id, domain).await?;
        let mut tried = HashSet::new();
        let mut probed = false;
        for pattern in known.into_iter().chain(PATTERNS) {
            let Some(local) = local_part(pattern, person.first, person.last) else { continue };
            if !tried.insert(local.clone()) {
                continue;
            }
            let email = format!("{local}@{domain}");
            // Not worth a verification when it couldn't be used anyway.
            if suppressed(self.db, org_id, &email).await? || taken(self.db, lead, &email).await? {
                continue;
            }
            match self.check(&email).await {
                Some(Verdict::Valid) => {
                    let whose = if known == Some(pattern) { " colleagues use" } else { "" };
                    let note = format!("Guessed from the {pattern}@ pattern{whose}, verified by {checker}.");
                    return Ok(Outcome { state: "guessed", source: pattern.to_string(), email: Some((email, "verified")), ..Outcome::ended("", note) });
                }
                Some(Verdict::Invalid) => {}
                Some(Verdict::Unsure) if !probed => {
                    probed = true;
                    let made_up = format!("no-such-person-{}@{domain}", &Uuid::new_v4().simple().to_string()[..10]);
                    match self.check(&made_up).await {
                        Some(Verdict::Invalid) => {}
                        Some(_) => return Ok(catch_all()),
                        None => break,
                    }
                }
                Some(Verdict::Unsure) => {}
                // Out of credits or a rejected key fails every call after it too, so stop spending on it.
                None => break,
            }
        }
        let tried = if finders.is_empty() { String::new() } else { format!("{finders} found no address, and ") };
        Ok(self.nothing(format!("{tried}{checker} turned down every common pattern.")))
    }

    /// Nothing found: a failure explains why better than the services' silence.
    fn nothing(&self, note: String) -> Outcome {
        match &self.failure {
            Some(failure) => Outcome::ended("failed", failure.clone()),
            None => Outcome::ended("not_found", note),
        }
    }
}

fn found_by(finder: &str, email: String, status: &'static str, note: String) -> Outcome {
    Outcome { state: "found", source: finder.to_string(), email: Some((email, status)), ..Outcome::ended("", note) }
}

/// The pattern most of the project's verified leads at `domain` follow, if any does.
async fn domain_pattern(db: &PgPool, project_id: Uuid, domain: &str) -> sqlx::Result<Option<&'static str>> {
    let colleagues: Vec<Lead> = sqlx::query_as(
        "select * from leads where project_id = $1 and email_status = 'verified' and split_part(lower(email::text), '@', 2) = $2 limit 50",
    )
    .bind(project_id)
    .bind(domain)
    .fetch_all(db)
    .await?;
    Ok(common_pattern(colleagues.iter().filter_map(|l| {
        let local = l.email.as_deref()?.rsplit_once('@')?.0.to_lowercase();
        let person = Person::of(l);
        Some((person.first.to_string(), person.last.to_string(), local))
    })))
}

/// The pattern that makes the most of these (first, last, local part) addresses, earlier patterns winning a tie.
pub fn common_pattern(addresses: impl Iterator<Item = (String, String, String)>) -> Option<&'static str> {
    let mut counts = [0; PATTERNS.len()];
    for (first, last, local) in addresses {
        for (i, pattern) in PATTERNS.iter().enumerate() {
            if local_part(pattern, &first, &last).as_deref() == Some(local.as_str()) {
                counts[i] += 1;
            }
        }
    }
    let best = (0..PATTERNS.len()).filter(|&i| counts[i] > 0).max_by_key(|&i| (counts[i], std::cmp::Reverse(i)))?;
    Some(PATTERNS[best])
}

/// The part before the @ for a pattern, or None when the name lacks a part it needs.
pub fn local_part(pattern: &str, first: &str, last: &str) -> Option<String> {
    let (f, l) = (plain(first), plain(last));
    if f.is_empty() || (pattern != "first" && l.is_empty()) {
        return None;
    }
    let initial = &f[..1];
    Some(match pattern {
        "first" => f,
        "first.last" => format!("{f}.{l}"),
        "flast" => format!("{initial}{l}"),
        "firstlast" => format!("{f}{l}"),
        "first_last" => format!("{f}_{l}"),
        "f.last" => format!("{initial}.{l}"),
        "last" => l,
        _ => return None,
    })
}

/// A name as addresses spell it: lowercase ASCII letters and digits, accents dropped, and spaces, hyphens and apostrophes
/// left out, so "Zoë O'Brien-Smith" gives "zoe" and "obriensmith".
fn plain(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => out.push('a'),
            'ç' | 'ć' | 'č' => out.push('c'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' => out.push('i'),
            'ł' => out.push('l'),
            'ñ' | 'ń' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => out.push('o'),
            'ś' | 'š' => out.push('s'),
            'ù' | 'ú' | 'û' | 'ü' | 'ū' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'ź' | 'ż' | 'ž' => out.push('z'),
            'ß' => out.push_str("ss"),
            c if c.is_ascii_alphanumeric() => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use reqwest::Url;
    use serde_json::{Value, json};

    use super::*;
    use crate::verify::TEST_API;

    /// Hunter (finder and verifier), Findymail and MillionVerifier, answering from what a test set.
    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<Book>>);

    #[derive(Default)]
    struct Book {
        /// Each finder's address for a domain.
        hunter: HashMap<String, String>,
        findymail: HashMap<String, String>,
        /// The verifiers' answer for an address, in MillionVerifier's words. Others are invalid, or catch_all on a domain
        /// in `catch_all`.
        mailboxes: HashMap<String, &'static str>,
        catch_all: Vec<String>,
        calls: Vec<String>,
    }

    impl Fake {
        fn book(&self) -> std::sync::MutexGuard<'_, Book> {
            self.0.lock().unwrap()
        }

        fn calls(&self) -> Vec<String> {
            std::mem::take(&mut self.book().calls)
        }

        fn verdict(&self, email: &str) -> &'static str {
            let mut book = self.book();
            book.calls.push(format!("verify {email}"));
            let domain = email.rsplit('@').next().unwrap_or_default().to_string();
            book.mailboxes.get(email).copied().unwrap_or(if book.catch_all.contains(&domain) { "catch_all" } else { "invalid" })
        }
    }

    async fn hunter_find(State(fake): State<Fake>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
        let mut book = fake.book();
        book.calls.push(format!("hunter {}", q["domain"]));
        let email = book.hunter.get(&q["domain"]).cloned();
        Json(json!({"data": {"email": email, "score": 99, "verification": {"status": "valid"}}}))
    }

    async fn hunter_verify(State(fake): State<Fake>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
        let status = match fake.verdict(&q["email"]) {
            "ok" => "valid",
            "catch_all" => "accept_all",
            other => other,
        };
        Json(json!({"data": {"status": status}}))
    }

    async fn findymail(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
        let domain = body["domain"].as_str().unwrap_or_default();
        let mut book = fake.book();
        book.calls.push(format!("findymail {domain}"));
        match book.findymail.get(domain) {
            Some(email) => Json(json!({"contact": {"email": email}})).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn millionverifier(State(fake): State<Fake>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
        Json(json!({"result": fake.verdict(&q["email"]), "error": ""}))
    }

    async fn serve() -> (Url, Fake) {
        let fake = Fake::default();
        let app = Router::new()
            .route("/v2/email-finder", get(hunter_find))
            .route("/v2/email-verifier", get(hunter_verify))
            .route("/api/search/name", post(findymail))
            .route("/api/v3/", get(millionverifier))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, fake)
    }

    struct Seeded {
        state: AppState,
        org: Uuid,
        project: Uuid,
    }

    async fn seed(db: &PgPool, finder: &str, verifier: &str) -> Seeded {
        let state = crate::autopilot::tests::state(db.clone());
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let project = add_project(db, org, finder, verifier).await;
        Seeded { state, org, project }
    }

    async fn add_project(db: &PgPool, org: Uuid, finder: &str, verifier: &str) -> Uuid {
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Fieldnote') returning id").bind(org).fetch_one(db).await.unwrap();
        let key = |name: &str| if name.is_empty() { String::new() } else { crypto::encrypt("", &format!("{name}-key")) };
        sqlx::query("insert into project_settings (project_id, finder, finder_key_enc, verifier, verifier_key_enc) values ($1, $2, $3, $4, $5)")
            .bind(project)
            .bind(finder)
            .bind(key(finder))
            .bind(verifier)
            .bind(key(verifier))
            .execute(db)
            .await
            .unwrap();
        project
    }

    async fn lead(db: &PgPool, project: Uuid, name: &str, domain: &str) -> Lead {
        let (first, last) = name.split_once(' ').unwrap_or((name, ""));
        sqlx::query_as(
            "insert into leads (project_id, source, full_name, first_name, last_name, company_domain) values ($1, 'linkedin', $2, $3, $4, $5) returning *",
        )
        .bind(project)
        .bind(name)
        .bind(first)
        .bind(last)
        .bind(domain)
        .fetch_one(db)
        .await
        .unwrap()
    }

    async fn reload(db: &PgPool, lead: &Lead) -> Lead {
        sqlx::query_as("select * from leads where id = $1").bind(lead.id).fetch_one(db).await.unwrap()
    }

    async fn find(s: &Seeded, url: &Url, lead: &Lead) -> Lead {
        TEST_API.scope(url.clone(), search_and_save(&s.state, lead)).await.unwrap();
        reload(&s.state.db, lead).await
    }

    #[test]
    fn patterns_spell_names_like_addresses_do() {
        let all: Vec<_> = PATTERNS.iter().map(|p| local_part(p, "Zoë", "O'Brien-Smith").unwrap()).collect();
        assert_eq!(all, ["zoe", "zoe.obriensmith", "zobriensmith", "zoeobriensmith", "zoe_obriensmith", "z.obriensmith", "obriensmith"]);
        assert_eq!(local_part("first.last", "Cher", ""), None);
        assert_eq!(local_part("first", "Cher", "").as_deref(), Some("cher"));
        let colleagues = [("Jane", "Doe", "jdoe"), ("Sam", "Lee", "slee"), ("Ola", "Berg", "ola.berg")];
        let found = common_pattern(colleagues.iter().map(|(f, l, local)| (f.to_string(), l.to_string(), local.to_string())));
        assert_eq!(found, Some("flast"));
        assert_eq!(common_pattern(std::iter::empty()), None);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn finders_go_in_order_and_stop_at_a_deliverable_address(db: PgPool) {
        let (url, fake) = serve().await;
        // Findymail is the finder and Hunter the verifier, so Hunter also finds, after Findymail.
        let s = seed(&db, "findymail", "hunter").await;
        {
            let mut book = fake.book();
            book.findymail.insert("acme.io".into(), "jane@acme.io".into());
            book.mailboxes.insert("jane@acme.io".into(), "ok");
            book.hunter.insert("beta.io".into(), "sam.lee@beta.io".into());
            book.findymail.insert("gamma.io".into(), "ola@gamma.io".into());
            book.mailboxes.insert("ola@gamma.io".into(), "unknown");
        }

        // Findymail doesn't vouch for its answers, so the verifier checks it, and Hunter isn't asked.
        let jane = find(&s, &url, &lead(&db, s.project, "Jane Doe", "acme.io").await).await;
        assert_eq!((jane.email.as_deref(), jane.email_status.as_str()), (Some("jane@acme.io"), "verified"));
        assert_eq!((jane.email_search.as_str(), jane.email_source.as_str(), jane.email_search_lookups), ("found", "Findymail", 2));
        assert_eq!(jane.email_search_note, "Found by Findymail, verified by Hunter.");
        assert_eq!(fake.calls(), ["findymail acme.io", "verify jane@acme.io"]);

        // Findymail has nobody, Hunter has a verified address.
        let sam = find(&s, &url, &lead(&db, s.project, "Sam Lee", "beta.io").await).await;
        assert_eq!((sam.email.as_deref(), sam.email_source.as_str()), (Some("sam.lee@beta.io"), "Hunter"));
        assert_eq!(fake.calls(), ["findymail beta.io", "hunter beta.io"]);

        // An address the verifier can't confirm is kept when no guess verifies either. Guessing doesn't check it again.
        let ola = find(&s, &url, &lead(&db, s.project, "Ola Berg", "gamma.io").await).await;
        assert_eq!((ola.email.as_deref(), ola.email_status.as_str(), ola.email_search.as_str()), (Some("ola@gamma.io"), "found", "found"));
        let calls = fake.calls();
        assert_eq!(calls[..3], ["findymail gamma.io", "verify ola@gamma.io", "hunter gamma.io"]);
        assert_eq!(calls.iter().filter(|c| *c == "verify ola@gamma.io").count(), 1, "{calls:?}");

        // A guess the verifier confirms beats a finder's unconfirmed address.
        {
            let mut book = fake.book();
            book.findymail.insert("delta.io".into(), "kim@delta.io".into());
            book.mailboxes.insert("kim@delta.io".into(), "unknown");
            book.mailboxes.insert("kim.ek@delta.io".into(), "ok");
        }
        let kim = find(&s, &url, &lead(&db, s.project, "Kim Ek", "delta.io").await).await;
        assert_eq!((kim.email.as_deref(), kim.email_status.as_str(), kim.email_search.as_str()), (Some("kim.ek@delta.io"), "verified", "guessed"));
        fake.calls();
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn guesses_follow_the_domains_pattern_first(db: PgPool) {
        let (url, fake) = serve().await;
        let s = seed(&db, "", "millionverifier").await;
        fake.book().mailboxes.extend([("jane.doe@acme.io".to_string(), "ok"), ("sam.lee@acme.io".to_string(), "ok")]);

        let jane = find(&s, &url, &lead(&db, s.project, "Jane Doe", "acme.io").await).await;
        assert_eq!((jane.email.as_deref(), jane.email_status.as_str()), (Some("jane.doe@acme.io"), "verified"));
        assert_eq!((jane.email_search.as_str(), jane.email_source.as_str()), ("guessed", "first.last"));
        assert_eq!(fake.calls(), ["verify jane@acme.io", "verify jane.doe@acme.io"]);

        // Jane's verified address shows acme.io's pattern, so Sam's guess starts there.
        let sam = find(&s, &url, &lead(&db, s.project, "Sam Lee", "acme.io").await).await;
        assert_eq!((sam.email.as_deref(), sam.email_search_lookups), (Some("sam.lee@acme.io"), 1));
        assert_eq!(sam.email_search_note, "Guessed from the first.last@ pattern colleagues use, verified by MillionVerifier.");
        assert_eq!(fake.calls(), ["verify sam.lee@acme.io"]);

        let kim = find(&s, &url, &lead(&db, s.project, "Kim Park", "nowhere.io").await).await;
        assert_eq!((kim.email, kim.email_search.as_str(), kim.email_search_lookups), (None, "not_found", 7));
        assert_eq!(kim.email_search_note, "MillionVerifier turned down every common pattern.");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_catch_all_domain_is_skipped(db: PgPool) {
        let (url, fake) = serve().await;
        let s = seed(&db, "", "millionverifier").await;
        {
            let mut book = fake.book();
            book.catch_all.push("open.io".into());
            // An unsure answer on a domain that turns down a made-up address isn't a catch-all, so the guessing goes on.
            book.mailboxes.insert("ada@lovelace.io".into(), "unknown");
            book.mailboxes.insert("ada.byron@lovelace.io".into(), "ok");
        }

        let jane = find(&s, &url, &lead(&db, s.project, "Jane Doe", "open.io").await).await;
        assert_eq!((jane.email, jane.email_search.as_str(), jane.email_search_lookups), (None, "catch_all", 2));
        let calls = fake.calls();
        assert_eq!(calls[0], "verify jane@open.io");
        assert!(calls[1].starts_with("verify no-such-person-"), "{calls:?}");

        // The next lead there is known to be on a catch-all domain, so no credit is spent.
        let sam = find(&s, &url, &lead(&db, s.project, "Sam Lee", "open.io").await).await;
        assert_eq!((sam.email_search.as_str(), sam.email_search_lookups), ("catch_all", 0));
        assert!(fake.calls().is_empty());

        let ada = find(&s, &url, &lead(&db, s.project, "Ada Byron", "lovelace.io").await).await;
        assert_eq!((ada.email.as_deref(), ada.email_search_lookups), (Some("ada.byron@lovelace.io"), 3));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn suppressed_and_taken_addresses_are_never_set(db: PgPool) {
        let (url, fake) = serve().await;
        let s = seed(&db, "findymail", "").await;
        crate::routes::suppressions::insert(&db, s.org, ["blocked.io", "jane@acme.io"]).await.unwrap();
        {
            let mut book = fake.book();
            book.findymail.insert("acme.io".into(), "jane@acme.io".into());
            book.findymail.insert("beta.io".into(), "sam@beta.io".into());
        }
        sqlx::query("insert into leads (project_id, full_name, email) values ($1, 'Samuel Lee', 'sam@beta.io')").bind(s.project).execute(&db).await.unwrap();

        // A suppressed domain costs no lookup at all.
        let blocked = find(&s, &url, &lead(&db, s.project, "Ola Berg", "blocked.io").await).await;
        assert_eq!((blocked.email_search.as_str(), blocked.email_search_note.as_str()), ("suppressed", "blocked.io is on the do-not-contact list."));
        assert!(fake.calls().is_empty());

        let jane = find(&s, &url, &lead(&db, s.project, "Jane Doe", "acme.io").await).await;
        assert_eq!((jane.email, jane.email_search.as_str()), (None, "suppressed"));
        assert_eq!(jane.email_search_note, "jane@acme.io is on the do-not-contact list.");

        let sam = find(&s, &url, &lead(&db, s.project, "Sam Lee", "beta.io").await).await;
        assert_eq!((sam.email, sam.email_search.as_str()), (None, "duplicate"));
        assert_eq!(sam.email_search_note, "sam@beta.io is already Samuel Lee's address.");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn guesses_skip_addresses_that_could_not_be_used(db: PgPool) {
        let (url, fake) = serve().await;
        let s = seed(&db, "", "millionverifier").await;
        crate::routes::suppressions::insert(&db, s.org, ["jane@acme.io"]).await.unwrap();
        sqlx::query("insert into leads (project_id, full_name, email) values ($1, 'Jane Dee', 'jane.doe@acme.io')")
            .bind(s.project)
            .execute(&db)
            .await
            .unwrap();
        let ok = ["jane@acme.io", "jane.doe@acme.io", "jdoe@acme.io"].map(|e| (e.to_string(), "ok"));
        fake.book().mailboxes.extend(ok);

        // The suppressed and the taken pattern aren't paid for, and the search goes on to the next.
        let jane = find(&s, &url, &lead(&db, s.project, "Jane Doe", "acme.io").await).await;
        assert_eq!((jane.email.as_deref(), jane.email_search.as_str()), (Some("jdoe@acme.io"), "guessed"));
        assert_eq!(fake.calls(), ["verify jdoe@acme.io"]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_worker_keeps_to_the_hourly_limit_and_tries_each_lead_once(db: PgPool) {
        let (url, fake) = serve().await;
        let s = seed(&db, "findymail", "").await;
        let other = add_project(&db, s.org, "findymail", "").await;
        let unconnected = add_project(&db, s.org, "", "").await;
        let oldest = lead(&db, s.project, "Ana Diaz", "one.io").await;
        let middle = lead(&db, other, "Ben Cho", "two.io").await;
        let newest = lead(&db, s.project, "Cy Ong", "three.io").await;
        let waiting = lead(&db, unconnected, "Di Fox", "four.io").await;
        sqlx::query("update leads set email_search = 'queued' where id = $1").bind(newest.id).execute(&db).await.unwrap();
        let tick = |hourly| TEST_API.scope(url.clone(), tick(&s.state, hourly));

        // Two an hour across the organization's projects: the queued lead first, then the oldest.
        tick(2).await.unwrap();
        let mut calls = fake.calls();
        calls.sort();
        assert_eq!(calls, ["findymail one.io", "findymail three.io"]);
        assert_eq!(reload(&db, &middle).await.email_search, "");
        tick(2).await.unwrap();
        assert!(fake.calls().is_empty());

        // With room, the rest goes, and a lead searched once isn't searched again unless it's queued.
        tick(10).await.unwrap();
        assert_eq!(fake.calls(), ["findymail two.io"]);
        tick(10).await.unwrap();
        assert!(fake.calls().is_empty());
        assert_eq!(reload(&db, &waiting).await.email_search, "", "a project without a finder or verifier waits for one");
        sqlx::query("update leads set email_search = 'queued' where id = $1").bind(oldest.id).execute(&db).await.unwrap();
        tick(10).await.unwrap();
        assert_eq!(fake.calls(), ["findymail one.io"]);
        assert_eq!(reload(&db, &oldest).await.email_search, "not_found");
        // A lead searched again counts again, and a deleted one still counts.
        sqlx::query("delete from leads where id = $1").bind(middle.id).execute(&db).await.unwrap();
        assert_eq!(searched_last_hour(&db, s.org).await.unwrap(), 4);
    }
}
