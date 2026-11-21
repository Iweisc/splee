//! Finding a person's email by name for the public API and MCP, without making them a lead: one at a time, or in batches the
//! worker here searches (migration 0185). Both run the project's email waterfall (waterfall.rs) and count against the same
//! hourly limit as the leads it searches, so a big batch waits for room rather than spending a month's credits at once. A
//! person's mobile by their LinkedIn profile is found here too, with the same finders and limit.

use std::time::Duration;

use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::routes::util_input;
use crate::state::AppState;
use crate::verify::Finder;
use crate::waterfall::{self, Asked, Outcome};

/// People one batch takes.
pub const MAX_BATCH: usize = 500;
/// Searched at the same time, like the waterfall's leads.
const PARALLEL: usize = 4;

static WAKE: Notify = Notify::const_new();

#[derive(Deserialize, FromRow)]
pub struct Person {
    pub first_name: String,
    #[serde(default)]
    pub last_name: String,
    /// A domain or a website address.
    pub company_domain: String,
    #[serde(default)]
    pub linkedin_url: String,
}

impl Person {
    /// The person with their names trimmed and the company's bare domain, or why they can't be searched.
    pub fn checked(self) -> Result<Person, String> {
        let first_name = self.first_name.trim().to_string();
        if first_name.is_empty() {
            return Err("first_name is empty".into());
        }
        let Some(company_domain) = util_input::domain(&self.company_domain) else {
            return Err(format!("company_domain {:?} isn't a domain", self.company_domain));
        };
        Ok(Person { first_name, last_name: self.last_name.trim().to_string(), company_domain, linkedin_url: self.linkedin_url.trim().to_string() })
    }
}

#[derive(Deserialize)]
pub struct BatchPerson {
    #[serde(flatten)]
    pub person: Person,
    /// The caller's own id for the person, given back with their result.
    #[serde(default)]
    pub client_ref: String,
}

/// What a search found, as the API returns it.
#[derive(Serialize, FromRow)]
pub struct Found {
    /// found, guessed, catch_all, not_found, suppressed, duplicate or failed, as on a lead's email_search. A batch's person is
    /// pending until searched.
    pub status: String,
    pub email: Option<String>,
    /// verified, or found when a finder had it and no verifier confirmed it.
    pub email_status: Option<String>,
    /// The finder that found it, or the pattern a guess followed.
    pub source: String,
    pub note: String,
    /// A mobile in E.164, only when the project's "Find phone numbers too" is on.
    pub phone: Option<String>,
}

impl From<Outcome> for Found {
    fn from(o: Outcome) -> Found {
        Found {
            status: o.state.to_string(),
            email: o.email.as_ref().map(|(email, _)| email.clone()),
            email_status: o.email.map(|(_, status)| status.to_string()),
            source: o.source,
            note: o.note,
            phone: o.phone.map(|(phone, _)| phone),
        }
    }
}

/// A 409 when the project has nothing to search with, which connecting a finder or verifier fixes.
pub async fn check_connected(db: &PgPool, project_id: Uuid) -> AppResult<()> {
    if !waterfall::connected(db, project_id).await? {
        return Err(AppError::Conflict("Connect an email finder or verifier to the project under Integrations first".into()));
    }
    Ok(())
}

/// Counts one search against the organization's hour, or a 429 saying when there's room again.
pub async fn claim_one(db: &PgPool, org_id: Uuid) -> AppResult<()> {
    match waterfall::claim(db, org_id, waterfall::HOURLY, 1).await? {
        Ok(_) => Ok(()),
        Err(secs) => Err(AppError::RetryAfter(
            format!("This organization used its {} email searches for the last hour. Try again in {secs} seconds", waterfall::HOURLY),
            secs,
        )),
    }
}

/// Searches `person`, checked, with the project's waterfall. The caller has claimed the search.
pub async fn find(state: &AppState, project_id: Uuid, person: &Person) -> Found {
    let lead = waterfall::unsaved_lead(project_id, &person.first_name, &person.last_name, &person.company_domain, &person.linkedin_url);
    match waterfall::search(state, &lead).await {
        Ok(outcome) => outcome.into(),
        Err(e) => Outcome::ended("failed", format!("{e:#}")).into(),
    }
}

/// Someone to find a mobile for, by their LinkedIn profile.
#[derive(Deserialize)]
pub struct PhoneQuery {
    pub linkedin_url: String,
    /// An address they're known by, which helps the finders match.
    #[serde(default)]
    pub email: Option<String>,
}

impl PhoneQuery {
    /// The query trimmed, with a lowercased address, or why it can't be searched.
    pub fn checked(self) -> Result<PhoneQuery, String> {
        let linkedin_url = self.linkedin_url.trim().to_string();
        if crate::unipile::profile_id(&linkedin_url).is_none() {
            return Err(format!("linkedin_url {:?} isn't a LinkedIn profile, like https://www.linkedin.com/in/jane-doe", self.linkedin_url));
        }
        let email = match self.email.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
            Some(email) => Some(util_input::email(email).ok_or_else(|| format!("email {email:?} isn't an address"))?),
            None => None,
        };
        Ok(PhoneQuery { linkedin_url, email })
    }
}

/// What looking up a mobile found, as the API returns it.
#[derive(Serialize, Debug)]
pub struct PhoneFound {
    /// found, not_found, suppressed or failed.
    pub status: &'static str,
    /// In E.164.
    pub phone: Option<String>,
    /// The finder that found it.
    pub source: String,
    pub note: String,
}

impl PhoneFound {
    fn ended(status: &'static str, source: &str, note: String) -> PhoneFound {
        PhoneFound { status, phone: None, source: source.to_string(), note }
    }
}

/// A 409 when none of the project's email finders return mobiles. Keep the names in sync with Finder::finds_phones.
pub async fn check_phone_finder(db: &PgPool, project_id: Uuid) -> AppResult<()> {
    let providers: Vec<String> = sqlx::query_scalar("select provider from project_finders where project_id = $1").bind(project_id).fetch_all(db).await?;
    if !providers.iter().filter_map(|p| Finder::parse(p)).any(Finder::finds_phones) {
        let connect = "Connect People Data Labs, RocketReach, Lusha or Prospeo to the project under Integrations first. They're the email finders that return mobiles";
        return Err(AppError::Conflict(connect.into()));
    }
    Ok(())
}

/// Asks the project's finders that return mobiles, in its order, for the mobile of the person behind the profile, stopping
/// at the first that has one. Like a lead's, a number without a + is read in the project's country, and one on the
/// do-not-contact list is passed over for the next finder's. The caller has claimed the search.
pub async fn find_phone(state: &AppState, org_id: Uuid, project_id: Uuid, query: &PhoneQuery) -> PhoneFound {
    search_phone(state, org_id, project_id, query).await.unwrap_or_else(|e| PhoneFound::ended("failed", "", format!("{e:#}")))
}

async fn search_phone(state: &AppState, org_id: Uuid, project_id: Uuid, query: &PhoneQuery) -> anyhow::Result<PhoneFound> {
    let db = &state.db;
    let country = crate::routes::leads::phone_country(db, project_id, "").await?;
    let finders = waterfall::finders(db, &state.cfg.app_secret, project_id).await?;
    let (linkedin, email) = (query.linkedin_url.as_str(), query.email.as_deref().unwrap_or_default());
    let (mut asked, mut paused, mut failure, mut suppressed) = (Vec::new(), paused_before(db, project_id).await?, None, None);
    for finder in finders.iter().filter(|f| f.service.finds_phones()) {
        let label = finder.service.label();
        let raw = match waterfall::ask_with(db, project_id, finder, |service, key| service.find_phone(key, linkedin, email)).await {
            Asked::Answer(raw) => raw,
            Asked::Paused(why) | Asked::Skipped(why) => {
                paused.push(why);
                continue;
            }
            Asked::Failed(e) => {
                failure.get_or_insert_with(|| format!("{label}: {e:#}"));
                continue;
            }
        };
        asked.push(label);
        let Some(phone) = raw.and_then(|raw| crate::phone::normalize(&raw, &country)) else { continue };
        let listed: bool = sqlx::query_scalar("select exists (select 1 from suppressions where org_id = $1 and value = $2)")
            .bind(org_id)
            .bind(&phone)
            .fetch_one(db)
            .await?;
        if !listed {
            return Ok(PhoneFound { status: "found", phone: Some(phone), source: label.to_string(), note: String::new() });
        }
        suppressed.get_or_insert((phone, label));
    }
    if let Some((phone, label)) = suppressed {
        return Ok(PhoneFound::ended("suppressed", label, format!("{phone} is on the do-not-contact list.")));
    }
    // Like the waterfall: a failure explains why better than the finders' silence, and a paused finder is named.
    if let Some(failure) = failure {
        return Ok(PhoneFound::ended("failed", "", failure));
    }
    let tried = (!asked.is_empty()).then(|| format!("{} had no mobile for this profile.", waterfall::and_list(asked)));
    let skipped = (!paused.is_empty()).then(|| format!("Skipped {}.", paused.join("; ")));
    let note = match (tried, skipped) {
        (Some(tried), Some(skipped)) => format!("{tried} {skipped}"),
        (Some(note), None) | (None, Some(note)) => note,
        // check_phone_finder found one, so it was removed since.
        (None, None) => "The project has no finder that returns mobiles.".into(),
    };
    Ok(PhoneFound::ended("not_found", "", note))
}

/// The finders that return mobiles and were paused before this search, which finders() leaves out and the note still names.
async fn paused_before(db: &PgPool, project_id: Uuid) -> sqlx::Result<Vec<String>> {
    let providers: Vec<String> =
        sqlx::query_scalar("select provider from project_finders where project_id = $1 and paused_until > now() order by position, created_at")
            .bind(project_id)
            .fetch_all(db)
            .await?;
    let paused = providers.iter().filter_map(|p| Finder::parse(p)).filter(|f| f.finds_phones());
    Ok(paused.map(|f| waterfall::skipped(f.label())).collect())
}

/// Saves checked people as a batch for the worker, and starts it.
pub async fn create(db: &PgPool, org_id: Uuid, project_id: Uuid, people: &[BatchPerson]) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar("insert into email_lookup_batches (org_id, project_id) values ($1, $2) returning id")
        .bind(org_id)
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
    let column = |f: &dyn Fn(&BatchPerson) -> String| people.iter().map(f).collect::<Vec<String>>();
    sqlx::query(
        "insert into email_lookups (batch_id, position, client_ref, first_name, last_name, company_domain, linkedin_url)
         select $1, n::int - 1, r, f, l, d, li from unnest($2::text[], $3::text[], $4::text[], $5::text[], $6::text[]) with ordinality as t(r, f, l, d, li, n)",
    )
    .bind(id)
    .bind(column(&|p| p.client_ref.clone()))
    .bind(column(&|p| p.person.first_name.clone()))
    .bind(column(&|p| p.person.last_name.clone()))
    .bind(column(&|p| p.person.company_domain.clone()))
    .bind(column(&|p| p.person.linkedin_url.clone()))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    WAKE.notify_one();
    Ok(id)
}

#[derive(Serialize)]
pub struct Batch {
    pub task_id: Uuid,
    pub project_id: Uuid,
    /// pending until every person is searched, then completed.
    pub status: &'static str,
    pub total: usize,
    pub searched: usize,
    pub created_at: Ts,
    pub finished_at: Option<Ts>,
    pub results: Vec<Row>,
}

#[derive(Serialize, FromRow)]
pub struct Row {
    pub client_ref: String,
    pub first_name: String,
    pub last_name: String,
    pub company_domain: String,
    pub linkedin_url: String,
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub found: Found,
}

/// The organization's batch with every person in order. Another organization's is a 404.
pub async fn load(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<Batch> {
    let (project_id, created_at, finished_at): (Uuid, Ts, Option<Ts>) =
        sqlx::query_as("select project_id, created_at, finished_at from email_lookup_batches where id = $1 and org_id = $2")
            .bind(id)
            .bind(org_id)
            .fetch_optional(db)
            .await?
            .ok_or(AppError::NotFound)?;
    let results: Vec<Row> = sqlx::query_as(
        "select client_ref, first_name, last_name, company_domain, linkedin_url, coalesce(result, 'pending') as status, email, email_status, source,
           note, phone from email_lookups where batch_id = $1 order by position",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    Ok(Batch {
        task_id: id,
        project_id,
        status: if finished_at.is_some() { "completed" } else { "pending" },
        total: results.len(),
        searched: results.iter().filter(|r| r.found.status != "pending").count(),
        created_at,
        finished_at,
        results,
    })
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = tick(&state).await {
                tracing::error!(error = ?e, "email lookups failed");
            }
            let _ = tokio::time::timeout(Duration::from_secs(60), WAKE.notified()).await;
        }
    });
}

/// Searches the waiting people of every organization, oldest batch first, as many as its hour has room for.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    sqlx::query("delete from email_lookup_batches where created_at < now() - interval '30 days'").execute(&state.db).await?;
    let orgs: Vec<Uuid> = sqlx::query_scalar("select distinct org_id from email_lookup_batches where finished_at is null").fetch_all(&state.db).await?;
    for org in orgs {
        if let Err(e) = search_org(state, org).await {
            tracing::error!(error = ?e, %org, "email lookups failed for an organization");
        }
    }
    Ok(())
}

#[derive(FromRow)]
struct Waiting {
    batch_id: Uuid,
    position: i32,
    project_id: Uuid,
    #[sqlx(flatten)]
    person: Person,
}

async fn search_org(state: &AppState, org_id: Uuid) -> anyhow::Result<()> {
    // Held for the round, so a second backend on the database skips this organization instead of searching its people twice.
    let mut lock = state.db.begin().await?;
    let owned: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended('email-lookups:' || $1::text, 0))")
        .bind(org_id)
        .fetch_one(&mut *lock)
        .await?;
    if !owned {
        return Ok(());
    }
    let mut waiting: Vec<Waiting> = sqlx::query_as(
        "select l.batch_id, l.position, b.project_id, l.first_name, l.last_name, l.company_domain, l.linkedin_url
         from email_lookups l join email_lookup_batches b on b.id = l.batch_id
         where b.org_id = $1 and b.finished_at is null and l.result is null order by b.created_at, l.batch_id, l.position limit $2",
    )
    .bind(org_id)
    .bind(waterfall::HOURLY)
    .fetch_all(&state.db)
    .await?;
    if !waiting.is_empty() {
        // Spent for the hour: the next tick tries again.
        let Ok(room) = waterfall::claim(&state.db, org_id, waterfall::HOURLY, waiting.len() as i64).await? else { return Ok(()) };
        waiting.truncate(room as usize);
        let search = |w: Waiting| async move {
            let found = find(state, w.project_id, &w.person).await;
            (found, w)
        };
        let mut done = stream::iter(waiting).map(search).buffer_unordered(PARALLEL);
        while let Some((found, w)) = done.next().await {
            if let Err(e) = save(&state.db, &w, &found).await {
                tracing::error!(error = ?e, batch = %w.batch_id, "could not save an email lookup");
            }
        }
    }
    sqlx::query(
        "update email_lookup_batches b set finished_at = now() where b.org_id = $1 and b.finished_at is null
           and not exists (select 1 from email_lookups l where l.batch_id = b.id and l.result is null)",
    )
    .bind(org_id)
    .execute(&state.db)
    .await?;
    Ok(())
}

async fn save(db: &PgPool, w: &Waiting, found: &Found) -> sqlx::Result<()> {
    sqlx::query(
        "update email_lookups set result = $3, email = $4, email_status = $5, source = $6, note = $7, phone = $8, searched_at = now()
         where batch_id = $1 and position = $2",
    )
    .bind(w.batch_id)
    .bind(w.position)
    .bind(&found.status)
    .bind(&found.email)
    .bind(&found.email_status)
    .bind(&found.source)
    .bind(&found.note)
    .bind(&found.phone)
    .execute(db)
    .await?;
    Ok(())
}
