//! The email waterfall: finds an address for a lead that came without one, like the leads LinkedIn people search adds, with
//! no Claude run. The project's email finders go first, then the common address patterns, each checked with its verifier.
//! A worker runs it on new leads by itself, and "Find emails" on the Leads page queues leads for it, ahead of the rest. With
//! "Find phone numbers too" on, finders that return mobile numbers also fill the lead's phone when it has none.

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
use crate::verify::{Finder, Paused, Person, Search, Verdict, Verifier};

/// Leads an organization's projects search an hour, together, so one big LinkedIn search can't spend a month's finder and
/// verifier credits in an afternoon.
pub const HOURLY: i64 = 200;
/// Leads searched at the same time. Each makes a few calls in a row, and the services limit calls a second.
const PARALLEL: usize = 4;

/// What the worker picks up, on `l` (leads): leads someone queued, and leads never searched that have a name and a company
/// domain and still want email, in a project with a verifier or a finder that isn't paused. One without either waits until
/// one is connected or back, rather than being marked not found.
const WAITING: &str = "l.email is null and (l.email_search = 'queued' or (l.email_search = '' and l.full_name <> '' and l.company_domain is not null
       and l.status not in ('bounced', 'unsubscribed', 'do_not_contact', 'not_interested')))
       and (exists (select 1 from project_finders pf where pf.project_id = l.project_id and (pf.paused_until is null or pf.paused_until <= now()))
         or exists (select 1 from project_settings ps where ps.project_id = l.project_id and ps.verifier <> ''))";

/// Whether the project has an email finder or verifier, so there's something to search with.
pub async fn connected(db: &PgPool, project_id: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "select exists (select 1 from project_finders where project_id = $1) or exists (select 1 from project_settings where project_id = $1 and verifier <> '')",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
}

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
    /// A mobile number in E.164 for a lead without a phone, with the finder that found it.
    pub phone: Option<(String, &'static str)>,
}

impl Outcome {
    fn ended(state: &'static str, note: impl Into<String>) -> Outcome {
        Outcome { state, source: String::new(), note: note.into(), email: None, lookups: 0, credits: None, phone: None }
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
        Err(e) => Err(e.into()),
        Ok(()) => match &outcome.email {
            Some((email, _)) if unset_if_suppressed(&state.db, lead, email).await? => {
                Ok(Outcome { email: None, state: "suppressed", note: format!("{email} is on the do-not-contact list."), ..outcome })
            }
            _ => Ok(outcome),
        },
    }
}

/// Checked again once the address is saved, since it may have gone on the do-not-contact list after the search checked it.
/// A suppression added later than this check came after the address was set, like one added after an import.
async fn unset_if_suppressed(db: &PgPool, lead: &Lead, email: &str) -> sqlx::Result<bool> {
    let domain = email.rsplit('@').next().unwrap_or_default();
    let unset = sqlx::query(
        "update leads l
         set email = null, email_status = default, email_search = 'suppressed', email_search_note = $3 || ' is on the do-not-contact list.'
         from projects p
         where l.id = $1 and p.id = l.project_id and lower(l.email::text) = $3
           and exists (select 1 from suppressions where org_id = p.org_id and value in ($3, $2))",
    )
    .bind(lead.id)
    .bind(domain.to_lowercase())
    .bind(email.to_lowercase())
    .execute(db)
    .await?;
    Ok(unset.rows_affected() == 1)
}

/// Leaves the lead alone when it got an address some other way while the search ran, and never replaces a phone it has. The
/// phone's do-not-contact check is in the same statement, since one added while later finders ran must still keep it off.
async fn save(db: &PgPool, lead: &Lead, o: &Outcome) -> sqlx::Result<()> {
    sqlx::query(
        "update leads l set email = coalesce($2, email), email_status = coalesce($3, email_status), email_search = $4, email_source = $5,
           email_search_note = $6, email_search_lookups = $7, email_search_credits = $8, email_searched_at = now(),
           phone_source = case when l.phone = '' and phone_ok.ok then $10 else l.phone_source end,
           phone = case when l.phone = '' and phone_ok.ok then $9 else l.phone end
         from (select $9::text is not null and not exists (
                 select 1 from suppressions s join projects p on p.org_id = s.org_id where p.id = $11 and s.value = $9) as ok) phone_ok
         where l.id = $1 and l.email is null",
    )
    .bind(lead.id)
    .bind(o.email.as_ref().map(|(email, _)| email))
    .bind(o.email.as_ref().map(|(_, status)| *status))
    .bind(o.state)
    .bind(&o.source)
    .bind(&o.note)
    .bind(o.lookups)
    .bind(o.credits)
    .bind(o.phone.as_ref().map(|(phone, _)| phone))
    .bind(o.phone.as_ref().map_or("", |(_, source)| *source))
    .bind(lead.project_id)
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
    let mut run = Run::new(state, lead, org_id).await?;
    if run.finders.is_empty() && run.verifier.is_none() {
        return Ok(Outcome::ended("not_found", "No email finder or verifier is connected."));
    }

    let outcome = match run.ask_finders(&person, &domain).await? {
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
    Ok(Outcome { lookups: run.lookups, credits: run.credits, phone: run.phone.take(), ..outcome })
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

pub(crate) struct Service<T> {
    pub service: T,
    key: String,
    /// The stored ciphertext, which alerts match on.
    enc: String,
    pub alert: Alert,
}

/// The project's finders that aren't paused, in its order, then Hunter when it's connected under Email verification and not
/// as a finder, since Hunter finds addresses too.
pub(crate) async fn finders(db: &PgPool, secret: &str, project_id: Uuid) -> anyhow::Result<Vec<Service<Finder>>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select provider, key_enc from project_finders where project_id = $1 and (paused_until is null or paused_until <= now()) order by position, created_at",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    let mut finders = Vec::new();
    for (provider, enc) in rows {
        if let Some(service) = Finder::parse(&provider) {
            finders.push(Service { service, key: crypto::decrypt(secret, &enc)?, enc, alert: Alert::Finder });
        }
    }
    let hunter: Option<String> = sqlx::query_scalar(
        "select verifier_key_enc from project_settings where project_id = $1 and verifier = 'hunter'
           and not exists (select 1 from project_finders where project_id = $1 and provider = 'hunter')",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await?;
    if let Some(enc) = hunter {
        finders.push(Service { service: Finder::Hunter, key: crypto::decrypt(secret, &enc)?, enc, alert: Alert::Verifier });
    }
    Ok(finders)
}

pub(crate) enum Asked {
    Answer(Search),
    /// Out of credits or past its rate limit, which is the finder's problem rather than the lead's.
    Paused(String),
    /// Paused by another search since this one loaded its finders, so it wasn't called.
    Skipped(String),
    Failed(anyhow::Error),
}

/// Asks one finder and shows how it went on the integrations page. A finder that says it's out of credits or past its rate
/// limit is paused on its row, so every search skips it until then. The pause is read again right before the call, since
/// searches running side by side load their finders before another one's call pauses it.
pub(crate) async fn ask(db: &PgPool, project_id: Uuid, finder: &Service<Finder>, person: &Person<'_>, domain: &str, phones: bool) -> Asked {
    let Service { service, key, enc, alert } = finder;
    let label = service.label();
    if matches!(alert, Alert::Finder) {
        let paused: sqlx::Result<Option<bool>> =
            sqlx::query_scalar("select paused_until > now() from project_finders where project_id = $1 and key_enc = $2")
                .bind(project_id)
                .bind(enc)
                .fetch_optional(db)
                .await
                .map(Option::flatten);
        match paused {
            Ok(Some(true)) => return Asked::Skipped(format!("{label}: it's out of credits or past its rate limit, so it's skipped for now")),
            Ok(_) => {}
            Err(e) => return Asked::Failed(e.into()),
        }
    }
    let result = service.find(key, person, domain, phones).await;
    // A Hunter key under Email verification shows only its failures there, like the sender's lookup.
    let shown = result.as_ref().map(|_| ()).map_err(|e| anyhow::anyhow!("{e:#}"));
    if matches!(alert, Alert::Finder) || shown.is_err() {
        hooks::record(db, project_id, *alert, enc, &shown).await;
    }
    let e = match result {
        Ok(search) => return Asked::Answer(search),
        Err(e) => e,
    };
    let Some(paused) = e.downcast_ref::<Paused>() else {
        tracing::warn!(error = %e, finder = label, "email search call failed");
        return Asked::Failed(e);
    };
    tracing::info!(finder = label, why = %paused, "email finder paused");
    if matches!(alert, Alert::Finder)
        && let Err(e) = pause(db, project_id, enc, paused.wait).await
    {
        tracing::warn!(error = %e, "couldn't pause the email finder");
    }
    Asked::Paused(format!("{label}: {e:#}"))
}

/// Never shortens a pause, so a rate limit answered by a search running alongside can't cut an hour without credits short.
async fn pause(db: &PgPool, project_id: Uuid, enc: &str, wait: Duration) -> sqlx::Result<()> {
    sqlx::query(
        "update project_finders set paused_until = greatest(paused_until, now() + make_interval(secs => $3)) where project_id = $1 and key_enc = $2",
    )
    .bind(project_id)
    .bind(enc)
    .bind(wait.as_secs_f64())
    .execute(db)
    .await?;
    Ok(())
}

/// One lead's search: the project's services and what they cost.
struct Run<'a> {
    db: &'a PgPool,
    project_id: Uuid,
    org_id: Option<Uuid>,
    finders: Vec<Service<Finder>>,
    verifier: Option<Service<Verifier>>,
    lookups: i32,
    credits: Option<i32>,
    /// The first service failure, which explains a search that found nothing.
    failure: Option<String>,
    /// Finders that paused during this search, which a search that found nothing mentions.
    paused: Vec<String>,
    /// Verdicts so far, so an address the finders' turn checked isn't paid for again when guessing.
    verdicts: HashMap<String, Verdict>,
    /// Set when the project finds phone numbers and the lead has none, to the country a local number is read by.
    phone_country: Option<String>,
    phone: Option<(String, &'static str)>,
}

impl<'a> Run<'a> {
    async fn new(state: &'a AppState, lead: &Lead, org_id: Option<Uuid>) -> anyhow::Result<Run<'a>> {
        let (db, project_id, secret) = (&state.db, lead.project_id, &state.cfg.app_secret);
        let row: Option<(String, String, bool)> = sqlx::query_as("select verifier, verifier_key_enc, find_phones from project_settings where project_id = $1")
            .bind(project_id)
            .fetch_optional(db)
            .await?;
        let (verifier, verifier_enc, find_phones) = row.unwrap_or_default();
        let verifier = match Verifier::parse(&verifier) {
            Some(service) => Some(Service { service, key: crypto::decrypt(secret, &verifier_enc)?, enc: verifier_enc, alert: Alert::Verifier }),
            None => None,
        };
        let phone_country = match find_phones && lead.phone.trim().is_empty() {
            true => Some(crate::routes::leads::phone_country(db, project_id, &lead.country_code).await?),
            false => None,
        };
        Ok(Run {
            db,
            project_id,
            org_id,
            finders: finders(db, secret, project_id).await?,
            verifier,
            lookups: 0,
            credits: None,
            failure: None,
            paused: Vec::new(),
            verdicts: HashMap::new(),
            phone_country,
            phone: None,
        })
    }

    fn wants_phone(&self) -> bool {
        self.phone_country.is_some() && self.phone.is_none()
    }

    /// A finder's mobile number, kept in E.164 unless it's on the do-not-contact list.
    async fn take_phone(&mut self, raw: &str, finder: &'static str) -> sqlx::Result<()> {
        let Some(phone) = self.phone_country.as_deref().and_then(|country| crate::phone::normalize(raw, country)) else { return Ok(()) };
        let suppressed: bool = sqlx::query_scalar("select exists (select 1 from suppressions where org_id = $1 and value = $2)")
            .bind(self.org_id)
            .bind(&phone)
            .fetch_one(self.db)
            .await?;
        if !suppressed {
            self.phone = Some((phone, finder));
        }
        Ok(())
    }

    fn failed(&mut self, service: &str, e: &anyhow::Error) {
        tracing::warn!(error = %e, service, "email verification call failed");
        self.failure.get_or_insert_with(|| format!("{service}: {e:#}"));
    }

    /// Each finder in turn, stopping at the first address it, or the verifier, says is deliverable. One that's found but
    /// unconfirmed is kept in case nothing better turns up, since a finder's answer beats a guess. While the lead still
    /// wants a phone, the finders after that one that return mobile numbers are asked too.
    async fn ask_finders(&mut self, person: &Person<'_>, domain: &str) -> anyhow::Result<Option<Outcome>> {
        let (mut chosen, mut fallback) = (None, None);
        for i in 0..self.finders.len() {
            let finder = &self.finders[i];
            let label = finder.service.label();
            let phones = self.wants_phone() && finder.service.finds_phones();
            if (chosen.is_some() && !phones) || !finder.service.can_search(person) {
                continue;
            }
            let asked = ask(self.db, self.project_id, finder, person, domain, phones).await;
            if !matches!(asked, Asked::Skipped(_)) {
                self.lookups += 1;
            }
            let search = match asked {
                Asked::Answer(search) => search,
                Asked::Paused(why) | Asked::Skipped(why) => {
                    self.paused.push(why);
                    continue;
                }
                Asked::Failed(e) => {
                    self.failure.get_or_insert_with(|| format!("{label}: {e:#}"));
                    continue;
                }
            };
            if let Some(charged) = search.credits {
                *self.credits.get_or_insert(0) += charged as i32;
            }
            if let Some(raw) = search.phone.as_deref().filter(|_| phones) {
                self.take_phone(raw, label).await?;
            }
            let Some(found) = search.found.filter(|_| chosen.is_none()) else { continue };
            if found.verified {
                chosen = Some(found_by(label, found.email, "verified", format!("Found by {label}.")));
                continue;
            }
            match self.check(&found.email).await {
                Some(Verdict::Valid) => {
                    let checker = self.verifier.as_ref().map_or("", |v| v.service.label());
                    chosen = Some(found_by(label, found.email, "verified", format!("Found by {label}, verified by {checker}.")));
                }
                Some(Verdict::Invalid) => {}
                // The sender checks it again before the first email, as it does any found address.
                Some(Verdict::Unsure) | None => {
                    fallback.get_or_insert_with(|| found_by(label, found.email, "found", format!("Found by {label}, not verified yet.")));
                }
            }
        }
        Ok(chosen.or(fallback))
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
        let finders = and_list(self.finders.iter().map(|f| f.service.label()).collect());
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

    /// Nothing found: a failure explains why better than the services' silence, and a paused finder is named.
    fn nothing(&self, note: String) -> Outcome {
        match &self.failure {
            Some(failure) => Outcome::ended("failed", failure.clone()),
            None if self.paused.is_empty() => Outcome::ended("not_found", note),
            None => Outcome::ended("not_found", format!("{note} Skipped {}.", self.paused.join("; "))),
        }
    }
}

/// "A", "A and B", or "A, B and C".
fn and_list(mut names: Vec<&str>) -> String {
    match names.pop() {
        Some(last) if !names.is_empty() => format!("{} and {last}", names.join(", ")),
        Some(last) => last.to_string(),
        None => String::new(),
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

    use axum::extract::{Path, Query, State};
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
        /// The six newer finders' people, by "{finder} {domain}", or "kaspr {LinkedIn id}" for Kaspr.
        people: HashMap<String, Found>,
        /// Finders answering that they're out of credits, or past their rate limit.
        broke: Vec<&'static str>,
        limited: Vec<&'static str>,
        /// Dropcontact's answers to a poll before a search is done.
        not_ready: usize,
    }

    /// A person a finder knows: their address, whether the finder vouches for it, and their mobile.
    #[derive(Clone, Default)]
    struct Found {
        email: Option<String>,
        verified: bool,
        mobile: Option<String>,
    }

    fn person(email: &str, verified: bool, mobile: Option<&str>) -> Found {
        Found { email: Some(email.to_string()), verified, mobile: mobile.map(str::to_string) }
    }

    impl Fake {
        fn book(&self) -> std::sync::MutexGuard<'_, Book> {
            self.0.lock().unwrap()
        }

        fn calls(&self) -> Vec<String> {
            std::mem::take(&mut self.book().calls)
        }

        /// Logs the call, and says whether the finder is out of credits or past its limit, else whom it knows there.
        fn ask(&self, finder: &'static str, at: &str, phones: bool) -> Result<Found, &'static str> {
            let mut book = self.book();
            book.calls.push(format!("{finder} {at}{}", if phones { " +phone" } else { "" }));
            if book.broke.contains(&finder) {
                return Err("broke");
            }
            if book.limited.contains(&finder) {
                return Err("limited");
            }
            Ok(book.people.get(&format!("{finder} {at}")).cloned().unwrap_or_default())
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

    fn reply(status: u16, body: Value) -> Response {
        (StatusCode::from_u16(status).unwrap(), Json(body)).into_response()
    }

    /// A 429 with Retry-After, as RocketReach and ContactOut send it.
    fn retry_in_two_minutes() -> Response {
        (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "120")], Json(json!({"message": "Rate limit reached"}))).into_response()
    }

    async fn pdl(State(fake): State<Fake>, Query(q): Query<HashMap<String, String>>) -> Response {
        match fake.ask("peopledatalabs", &q["company"], false) {
            Err("broke") => reply(402, json!({"status": 402, "error": {"type": "payment_required", "message": "You have hit your account maximum"}})),
            // People Data Labs gives no Retry-After, only when the window resets.
            Err(_) => (StatusCode::TOO_MANY_REQUESTS, [("x-ratelimit-reset", "2026-11-12 10:00:00")], Json(json!({"status": 429}))).into_response(),
            Ok(Found { email: None, .. }) => reply(404, json!({"status": 404, "error": {"type": "not_found", "message": "No records were found matching your request"}})),
            Ok(p) => Json(json!({"status": 200, "likelihood": 8, "data": {"work_email": p.email, "mobile_phone": p.mobile}})).into_response(),
        }
    }

    async fn rocketreach(State(fake): State<Fake>, Query(q): Query<HashMap<String, String>>) -> Response {
        match fake.ask("rocketreach", &q["current_employer"], false) {
            Err("broke") => reply(403, json!({"detail": "You have no lookup credits remaining."})),
            Err(_) => retry_in_two_minutes(),
            Ok(Found { email: None, .. }) => reply(404, json!({"detail": "Not found."})),
            // Still searching, which its status call finishes.
            Ok(_) => Json(json!({"id": 7, "status": "searching", "emails": []})).into_response(),
        }
    }

    async fn rocketreach_status(State(fake): State<Fake>) -> Json<Value> {
        let book = fake.book();
        let p = book.people.iter().find(|(k, _)| k.starts_with("rocketreach ")).map(|(_, p)| p.clone()).unwrap_or_default();
        let smtp = if p.verified { "valid" } else { "accept-all" };
        let phones = json!([{"number": "+1 212-555-0100", "e164": "+12125550100", "type": "direct dial"}, {"number": p.mobile, "e164": p.mobile, "type": "mobile"}]);
        Json(json!([{"id": 7, "status": "complete", "emails": [{"email": p.email, "type": "professional", "grade": "A", "smtp_valid": smtp}], "phones": phones}]))
    }

    async fn snov_sign_in(body: String) -> Response {
        match body.contains("client_secret=snov-secret") {
            true => Json(json!({"access_token": "snov-token", "token_type": "Bearer", "expires_in": 3600})).into_response(),
            false => reply(400, json!({"error": "invalid_client"})),
        }
    }

    async fn snov_start(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
        let domain = body["rows"][0]["domain"].as_str().unwrap_or_default().to_string();
        match fake.ask("snov", &domain, false) {
            Err("limited") => (StatusCode::TOO_MANY_REQUESTS, Json(json!({"message": "Too many requests"}))).into_response(),
            _ => Json(json!({"data": {"task_hash": domain}, "meta": {"rows": 1}})).into_response(),
        }
    }

    async fn snov_result(State(fake): State<Fake>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
        let book = fake.book();
        if book.broke.contains(&"snov") {
            return Json(json!({"status": "not_enough_credits", "data": []}));
        }
        let p = book.people.get(&format!("snov {}", q["task_hash"])).cloned().unwrap_or_default();
        let result: Vec<Value> = p.email.iter().map(|e| json!({"email": e, "smtp_status": if p.verified { "valid" } else { "unknown" }})).collect();
        Json(json!({"status": "completed", "data": [{"people": "Jane Doe", "result": result}]}))
    }

    async fn dropcontact_submit(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
        let domain = body["data"][0]["website"].as_str().unwrap_or_default().to_string();
        match fake.ask("dropcontact", &domain, false) {
            Err("broke") => reply(403, json!({"error": true, "reason": "You submitted 1 valid items but only have 0 credits left", "success": false})),
            Err(_) => reply(429, json!({"error": true, "reason": "please retry in a few seconds", "success": false})),
            Ok(_) => Json(json!({"error": false, "request_id": domain, "success": true, "credits_left": 99})).into_response(),
        }
    }

    async fn dropcontact_poll(State(fake): State<Fake>, Path(id): Path<String>) -> Json<Value> {
        let mut book = fake.book();
        book.calls.push("dropcontact poll".into());
        if book.not_ready > 0 {
            book.not_ready -= 1;
            return Json(json!({"error": false, "reason": "Request not ready yet, try again in 30 seconds", "success": false}));
        }
        let p = book.people.get(&format!("dropcontact {id}")).cloned().unwrap_or_default();
        let qualification = if p.verified { "nominative@pro" } else { "catch-all@pro" };
        let email: Vec<Value> = p.email.iter().map(|e| json!({"email": e, "qualification": qualification})).collect();
        Json(json!({"data": [{"first_name": "Jane", "email": email, "phone": "+33 1 23 45 67 89"}], "error": false, "success": true}))
    }

    async fn contactout(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
        let phones = body["include"].as_array().is_some_and(|i| i.contains(&json!("phone")));
        match fake.ask("contactout", body["company_domain"][0].as_str().unwrap_or_default(), phones) {
            Err("broke") => reply(403, json!({"status_code": 403, "message": "You're out of credits, please email your sales manager"})),
            Err(_) => retry_in_two_minutes(),
            Ok(Found { email: None, mobile: None, .. }) => reply(404, json!({"status_code": 404, "message": "Not Found"})),
            Ok(p) => {
                let status: serde_json::Map<String, Value> = p.email.iter().map(|e| (e.clone(), json!(if p.verified { "Verified" } else { "Unverified" }))).collect();
                let phone: Vec<&String> = p.mobile.iter().filter(|_| phones).collect();
                Json(json!({"status_code": 200, "profile": {"work_email": p.email.iter().collect::<Vec<_>>(), "work_email_status": status, "phone": phone}}))
                    .into_response()
            }
        }
    }

    async fn kaspr(State(fake): State<Fake>, headers: axum::http::HeaderMap, Json(body): Json<Value>) -> Response {
        assert_eq!(headers["accept-version"], "v2.0");
        assert!(body["name"].as_str().is_some_and(|n| !n.is_empty()), "Kaspr needs the name: {body}");
        let phones = body["dataToGet"].as_array().is_some_and(|d| d.contains(&json!("phone")));
        match fake.ask("kaspr", body["id"].as_str().unwrap_or_default(), phones) {
            Err("broke") => reply(402, json!({"message": "No credits left", "reason": "No work email credits left"})),
            Err(_) => {
                let headers = [("x-minutely-ratelimit-remaining", "0"), ("x-minutely-reset", "30000")];
                (StatusCode::TOO_MANY_REQUESTS, headers, Json(json!({"message": "Too many requests, please try again later."}))).into_response()
            }
            Ok(p) => {
                let phones: Vec<&String> = p.mobile.iter().filter(|_| phones).collect();
                Json(json!({"profile": {"professionalEmails": p.email.iter().collect::<Vec<_>>(), "starryProfessionalEmail": p.email, "phones": phones}})).into_response()
            }
        }
    }

    async fn serve() -> (Url, Fake) {
        let fake = Fake::default();
        let app = Router::new()
            .route("/v2/email-finder", get(hunter_find))
            .route("/v2/email-verifier", get(hunter_verify))
            .route("/api/search/name", post(findymail))
            .route("/api/v3/", get(millionverifier))
            .route("/v5/person/enrich", get(pdl))
            .route("/api/v2/person/lookup", get(rocketreach))
            .route("/api/v2/person/checkStatus", get(rocketreach_status))
            .route("/v1/oauth/access_token", post(snov_sign_in))
            .route("/v2/emails-by-domain-by-name/start", post(snov_start))
            .route("/v2/emails-by-domain-by-name/result", get(snov_result))
            .route("/v1/enrich/all", post(dropcontact_submit))
            .route("/v1/enrich/all/{id}", get(dropcontact_poll))
            .route("/v1/people/enrich", post(contactout))
            .route("/profile/linkedin", post(kaspr))
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

    /// `finders` is a comma-separated list, in the order they're asked.
    async fn add_project(db: &PgPool, org: Uuid, finders: &str, verifier: &str) -> Uuid {
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Fieldnote') returning id").bind(org).fetch_one(db).await.unwrap();
        // Snov.io's key is its client ID and secret.
        let key = |name: &str| match name {
            "" => String::new(),
            "snov" => crypto::encrypt("", "snov-id:snov-secret"),
            _ => crypto::encrypt("", &format!("{name}-key")),
        };
        sqlx::query("insert into project_settings (project_id, verifier, verifier_key_enc) values ($1, $2, $3)")
            .bind(project)
            .bind(verifier)
            .bind(key(verifier))
            .execute(db)
            .await
            .unwrap();
        for (position, finder) in finders.split(',').filter(|f| !f.is_empty()).enumerate() {
            sqlx::query("insert into project_finders (project_id, provider, key_enc, position) values ($1, $2, $3, $4)")
                .bind(project)
                .bind(finder)
                .bind(key(finder))
                .bind(position as i32)
                .execute(db)
                .await
                .unwrap();
        }
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

        // Suppressed while the search ran: the saved address is taken off again.
        let late = lead(&db, s.project, "Kim Ek", "late.io").await;
        sqlx::query("update leads set email = 'kim@late.io' where id = $1").bind(late.id).execute(&db).await.unwrap();
        assert!(!unset_if_suppressed(&db, &late, "kim@late.io").await.unwrap());
        crate::routes::suppressions::insert(&db, s.org, ["late.io"]).await.unwrap();
        assert!(unset_if_suppressed(&db, &late, "kim@late.io").await.unwrap());
        let late = reload(&db, &late).await;
        assert_eq!((late.email, late.email_search.as_str()), (None, "suppressed"));
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

    async fn linked(db: &PgPool, lead: Lead, slug: &str) -> Lead {
        sqlx::query_as("update leads set linkedin_url = $2 where id = $1 returning *")
            .bind(lead.id)
            .bind(format!("https://www.linkedin.com/in/{slug}/"))
            .fetch_one(db)
            .await
            .unwrap()
    }

    async fn find_phones(db: &PgPool, project: Uuid) {
        sqlx::query("update project_settings set find_phones = true where project_id = $1").bind(project).execute(db).await.unwrap();
    }

    /// Seconds until the finder is asked again, or None when it isn't paused.
    async fn paused_for(db: &PgPool, project: Uuid, finder: &str) -> Option<f64> {
        sqlx::query_scalar("select extract(epoch from paused_until - now())::float8 from project_finders where project_id = $1 and provider = $2")
            .bind(project)
            .bind(finder)
            .fetch_one(db)
            .await
            .unwrap()
    }

    const NEW_FINDERS: [(&str, &str); 6] = [
        ("peopledatalabs", "People Data Labs"),
        ("rocketreach", "RocketReach"),
        ("snov", "Snov.io"),
        ("dropcontact", "Dropcontact"),
        ("contactout", "ContactOut"),
        ("kaspr", "Kaspr"),
    ];

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_new_finder_finds_an_address_or_says_it_has_none(db: PgPool) {
        let (url, fake) = serve().await;
        let s = seed(&db, "", "").await;
        for (finder, label) in NEW_FINDERS {
            let project = add_project(&db, s.org, finder, "").await;
            // Kaspr knows people by their LinkedIn profile.
            let (hit, miss) = if finder == "kaspr" { ("jane-doe", "sam-lee") } else { ("acme.io", "beta.io") };
            fake.book().people.insert(format!("{finder} {hit}"), person("Jane.Doe@acme.io", true, None));

            let jane = linked(&db, lead(&db, project, "Jane Doe", "acme.io").await, "jane-doe").await;
            let jane = find(&s, &url, &jane).await;
            assert_eq!((jane.email.as_deref(), jane.email_source.as_str()), (Some("jane.doe@acme.io"), label), "{finder}");
            assert_eq!(jane.email_search, "found", "{finder}");
            // People Data Labs and Kaspr don't say whether they verified it, so the sender checks it first.
            let status = if matches!(finder, "peopledatalabs" | "kaspr") { "found" } else { "verified" };
            assert_eq!(jane.email_status, status, "{finder}");

            let sam = linked(&db, lead(&db, project, "Sam Lee", "beta.io").await, "sam-lee").await;
            let sam = find(&s, &url, &sam).await;
            assert_eq!((sam.email, sam.email_search.as_str(), sam.email_search_lookups), (None, "not_found", 1), "{finder}");
            assert_eq!(sam.email_search_note, format!("{label} found no address, and there's no verifier to check a guess."));
            let calls = fake.calls();
            assert!(calls.contains(&format!("{finder} {hit}")) && calls.contains(&format!("{finder} {miss}")), "{calls:?}");
        }
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_finder_out_of_credits_or_past_its_limit_is_skipped_without_failing_the_lead(db: PgPool) {
        let (url, fake) = serve().await;
        let s = seed(&db, "", "").await;
        for (finder, label) in NEW_FINDERS {
            for limited in [false, true] {
                let project = add_project(&db, s.org, &format!("{finder},findymail"), "").await;
                {
                    let mut book = fake.book();
                    book.broke.clear();
                    book.limited.clear();
                    if limited { book.limited.push(finder) } else { book.broke.push(finder) }
                    book.findymail.insert("acme.io".into(), "jane@acme.io".into());
                }
                let jane = find(&s, &url, &linked(&db, lead(&db, project, "Jane Doe", "acme.io").await, "jane-doe").await).await;
                assert_eq!((jane.email.as_deref(), jane.email_source.as_str()), (Some("jane@acme.io"), "Findymail"), "{finder} {limited}");

                // Out of credits waits an hour, a rate limit as long as the finder says.
                let wait = paused_for(&db, project, finder).await.unwrap_or_else(|| panic!("{finder} isn't paused"));
                let want = match (finder, limited) {
                    (_, false) => 3600.0,
                    ("rocketreach" | "contactout", true) => 120.0,
                    ("kaspr", true) => 30.0,
                    _ => 60.0,
                };
                assert!((want - 5.0..=want).contains(&wait), "{finder} {limited}: paused for {wait}s, not {want}s");
                let error: Option<String> =
                    sqlx::query_scalar("select error from project_finders where project_id = $1 and provider = $2").bind(project).bind(finder).fetch_one(&db).await.unwrap();
                assert!(error.is_some_and(|e| e.contains(if limited { "rate limit" } else { "out of credits" })), "{finder} {limited}");

                // While it's paused, nobody's search asks it, and one that finds nothing names it without failing.
                fake.calls();
                let sam = find(&s, &url, &linked(&db, lead(&db, project, "Sam Lee", "beta.io").await, "sam-lee").await).await;
                assert_eq!(fake.calls(), ["findymail beta.io"], "{finder} {limited}");
                assert_eq!(sam.email_search, "not_found");
                sqlx::query("update project_finders set paused_until = null where project_id = $1").bind(project).execute(&db).await.unwrap();
                let kim = find(&s, &url, &linked(&db, lead(&db, project, "Kim Ek", "gamma.io").await, "kim-ek").await).await;
                assert_eq!(kim.email_search, "not_found", "{finder} {limited}: {}", kim.email_search_note);
                assert!(kim.email_search_note.contains(&format!("Skipped {label}: ")), "{}", kim.email_search_note);
                fake.calls();
            }
        }
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn dropcontact_is_read_again_until_its_search_is_done(db: PgPool) {
        let (url, fake) = serve().await;
        let s = seed(&db, "dropcontact", "").await;
        {
            let mut book = fake.book();
            book.people.insert("dropcontact acme.io".into(), person("jane.doe@acme.io", false, None));
            book.not_ready = 2;
        }
        let jane = find(&s, &url, &lead(&db, s.project, "Jane Doe", "acme.io").await).await;
        assert_eq!(fake.calls(), ["dropcontact acme.io", "dropcontact poll", "dropcontact poll", "dropcontact poll"]);
        // On a catch-all domain, so it's kept unconfirmed.
        assert_eq!((jane.email.as_deref(), jane.email_status.as_str()), (Some("jane.doe@acme.io"), "found"));
        assert_eq!(jane.email_search_note, "Found by Dropcontact, not verified yet.");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn kaspr_is_only_asked_about_leads_with_a_linkedin_profile(db: PgPool) {
        let (url, fake) = serve().await;
        let s = seed(&db, "kaspr,snov", "").await;
        fake.book().people.insert("kaspr jane-doe".into(), person("jane@acme.io", false, None));
        fake.book().people.insert("snov beta.io".into(), person("sam@beta.io", true, None));

        let sam = find(&s, &url, &lead(&db, s.project, "Sam Lee", "beta.io").await).await;
        assert_eq!(fake.calls(), ["snov beta.io"]);
        assert_eq!((sam.email.as_deref(), sam.email_search_lookups), (Some("sam@beta.io"), 1));

        let jane = find(&s, &url, &linked(&db, lead(&db, s.project, "Jane Doe", "acme.io").await, "jane-doe").await).await;
        assert_eq!(fake.calls()[0], "kaspr jane-doe");
        assert_eq!(jane.email.as_deref(), Some("jane@acme.io"));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn finders_are_asked_in_the_projects_order(db: PgPool) {
        let (url, fake) = serve().await;
        let s = seed(&db, "kaspr,snov,peopledatalabs", "").await;
        {
            let mut book = fake.book();
            book.people.insert("kaspr jane-doe".into(), person("jane@acme.io", false, None));
            book.people.insert("kaspr sam-lee".into(), person("sam@acme.io", false, None));
            for who in ["jane", "sam"] {
                book.people.insert(format!("snov {who}.io"), person(&format!("{who}@{who}.io"), true, None));
                book.people.insert(format!("peopledatalabs {who}.io"), person(&format!("{who}@{who}.io"), false, None));
            }
        }
        // Kaspr's answer isn't verified, so the next finder is asked, and Snov.io's verified one wins. Nobody asks People Data Labs.
        let jane = find(&s, &url, &linked(&db, lead(&db, s.project, "Jane Doe", "jane.io").await, "jane-doe").await).await;
        assert_eq!(fake.calls(), ["kaspr jane-doe", "snov jane.io"]);
        assert_eq!(jane.email_source, "Snov.io");

        crate::routes::finders::set_order(&db, s.project, &["peopledatalabs".into(), "snov".into(), "kaspr".into()]).await.unwrap();
        let sam = find(&s, &url, &linked(&db, lead(&db, s.project, "Sam Lee", "sam.io").await, "sam-lee").await).await;
        assert_eq!(fake.calls(), ["peopledatalabs sam.io", "snov sam.io"]);
        assert_eq!((sam.email.as_deref(), sam.email_source.as_str()), (Some("sam@sam.io"), "Snov.io"));
    }

    async fn us_lead(db: &PgPool, project: Uuid, name: &str, domain: &str) -> Lead {
        let l = lead(db, project, name, domain).await;
        sqlx::query_as("update leads set country_code = 'US' where id = $1 returning *").bind(l.id).fetch_one(db).await.unwrap()
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn finders_fill_an_empty_phone_when_the_project_asks(db: PgPool) {
        let (url, fake) = serve().await;
        // Snov.io finds the address and has no phones, so People Data Labs and RocketReach are asked for one after it.
        let s = seed(&db, "snov,peopledatalabs,rocketreach", "").await;
        {
            let mut book = fake.book();
            for who in ["jane", "sam", "kim", "ola"] {
                book.people.insert(format!("snov {who}.io"), person(&format!("{who}@{who}.io"), true, None));
                book.people.insert(format!("peopledatalabs {who}.io"), person(&format!("{who}@{who}.io"), false, Some("415.555.0134")));
            }
            book.people.insert("rocketreach kim.io".into(), Found { email: Some("kim@kim.io".into()), mobile: Some("+1 202 555 0177".into()), ..Found::default() });
        }

        // Off by default: nobody is asked for a phone.
        let jane = find(&s, &url, &us_lead(&db, s.project, "Jane Doe", "jane.io").await).await;
        assert_eq!((jane.email.as_deref(), jane.phone.as_str()), (Some("jane@jane.io"), ""));
        assert_eq!(fake.calls(), ["snov jane.io"]);

        // On: People Data Labs' local number is read as the lead's country's, and RocketReach isn't asked once there's one.
        find_phones(&db, s.project).await;
        let sam = find(&s, &url, &us_lead(&db, s.project, "Sam Lee", "sam.io").await).await;
        assert_eq!((sam.email.as_deref(), sam.email_source.as_str()), (Some("sam@sam.io"), "Snov.io"));
        assert_eq!((sam.phone.as_str(), sam.phone_source.as_str()), ("+14155550134", "People Data Labs"));
        assert_eq!(fake.calls(), ["snov sam.io", "peopledatalabs sam.io"]);

        // A number on the do-not-contact list is skipped, and the next finder's mobile is used, not its direct dial.
        crate::routes::suppressions::insert(&db, s.org, ["+1 415 555 0134"]).await.unwrap();
        let kim = find(&s, &url, &us_lead(&db, s.project, "Kim Ek", "kim.io").await).await;
        assert_eq!((kim.phone.as_str(), kim.phone_source.as_str()), ("+12025550177", "RocketReach"));
        assert_eq!(fake.calls(), ["snov kim.io", "peopledatalabs kim.io", "rocketreach kim.io"]);

        // A lead's own phone is never replaced, so nobody is asked for one.
        let ola = us_lead(&db, s.project, "Ola Berg", "ola.io").await;
        sqlx::query("update leads set phone = '+447700900123' where id = $1").bind(ola.id).execute(&db).await.unwrap();
        let ola = find(&s, &url, &reload(&db, &ola).await).await;
        assert_eq!((ola.phone.as_str(), ola.phone_source.as_str()), ("+447700900123", ""));
        assert_eq!(fake.calls(), ["snov ola.io"]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn only_numbers_marked_as_mobiles_fill_a_phone(db: PgPool) {
        let (url, fake) = serve().await;
        // ContactOut and Kaspr list phones without saying which are mobiles, so they're never asked for one.
        let s = seed(&db, "contactout,kaspr", "").await;
        find_phones(&db, s.project).await;
        {
            let mut book = fake.book();
            book.people.insert("contactout acme.io".into(), person("jane@acme.io", true, Some("+1 415 555 0134")));
            book.people.insert("kaspr jane-doe".into(), person("jane@acme.io", false, Some("+1 202 555 0177")));
        }
        let jane = find(&s, &url, &linked(&db, us_lead(&db, s.project, "Jane Doe", "acme.io").await, "jane-doe").await).await;
        assert_eq!((jane.email.as_deref(), jane.phone.as_str()), (Some("jane@acme.io"), ""));
        assert_eq!(fake.calls(), ["contactout acme.io"]);

        // RocketReach's direct dial isn't a mobile, so it's left out.
        let other = add_project(&db, s.org, "rocketreach", "").await;
        find_phones(&db, other).await;
        fake.book().people.insert("rocketreach beta.io".into(), person("sam@beta.io", true, None));
        let sam = find(&s, &url, &us_lead(&db, other, "Sam Lee", "beta.io").await).await;
        assert_eq!((sam.email.as_deref(), sam.phone.as_str()), (Some("sam@beta.io"), ""));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_pause_is_never_shortened_and_is_read_again_before_each_call(db: PgPool) {
        let (url, fake) = serve().await;
        let s = seed(&db, "snov,findymail", "").await;
        let enc: String = sqlx::query_scalar("select key_enc from project_finders where project_id = $1 and provider = 'snov'")
            .bind(s.project)
            .fetch_one(&db)
            .await
            .unwrap();

        // A rate limit's minute answered after an hour without credits leaves the hour.
        pause(&db, s.project, &enc, Duration::from_secs(3600)).await.unwrap();
        pause(&db, s.project, &enc, Duration::from_secs(60)).await.unwrap();
        let wait = paused_for(&db, s.project, "snov").await.unwrap();
        assert!(wait > 3590.0, "paused for {wait}s");

        // A search that loaded its finders before the pause still doesn't call it.
        sqlx::query("update project_finders set paused_until = null where project_id = $1").bind(s.project).execute(&db).await.unwrap();
        let loaded = finders(&db, "", s.project).await.unwrap();
        pause(&db, s.project, &enc, Duration::from_secs(60)).await.unwrap();
        let jane = Person { first: "Jane", last: "Doe", linkedin: "", company: "" };
        let asked = TEST_API.scope(url.clone(), ask(&db, s.project, &loaded[0], &jane, "acme.io", false)).await;
        assert!(matches!(asked, Asked::Skipped(_)));
        assert!(fake.calls().is_empty());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_phone_suppressed_while_the_search_ran_is_not_saved(db: PgPool) {
        let s = seed(&db, "rocketreach", "").await;
        let with_phone = |phone: &str| Outcome { phone: Some((phone.to_string(), "RocketReach")), ..Outcome::ended("not_found", "") };
        let jane = lead(&db, s.project, "Jane Doe", "acme.io").await;
        let sam = lead(&db, s.project, "Sam Lee", "beta.io").await;
        crate::routes::suppressions::insert(&db, s.org, ["+1 415 555 0134"]).await.unwrap();

        save(&db, &jane, &with_phone("+14155550134")).await.unwrap();
        save(&db, &sam, &with_phone("+12025550177")).await.unwrap();
        let (jane, sam) = (reload(&db, &jane).await, reload(&db, &sam).await);
        assert_eq!((jane.phone.as_str(), jane.phone_source.as_str(), jane.email_search.as_str()), ("", "", "not_found"));
        assert_eq!((sam.phone.as_str(), sam.phone_source.as_str()), ("+12025550177", "RocketReach"));
    }
}
