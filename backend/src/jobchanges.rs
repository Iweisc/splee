//! Job change alerts: someone who bought, booked a meeting or replied hot is the warmest lead there is once they move to a new
//! company. Each project watches those people on LinkedIn through its Unipile account, reading each profile at most every
//! RECHECK_DAYS within the account's DAILY_READS, and a move becomes a lead at the new company: an email from the finders, a
//! score reason, an alert to the old lead's owner, and the campaign the project picked for job changes, if any. The email
//! writer gets the history, so the first email can mention it.

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::hooks::{self, Alert, ChatAlert};
use crate::models::Ts;
use crate::routes::util_input;
use crate::state::AppState;
use crate::unipile::{self, Unipile};

/// Profile and company pages read per LinkedIn account a day, counted in linkedin_profile_reads. LinkedIn restricts accounts
/// that view a few hundred profiles a day, and Unipile advises staying near 100. The same account also searches and sends,
/// so the watch keeps well under that.
pub const DAILY_READS: i32 = 40;
/// Days a profile rests between reads. People stay in a job for years, so a month still catches a move while it's news.
pub const RECHECK_DAYS: i32 = 30;
/// A list or CSV added at once. A team's past customers fit many times over.
const MAX_ADD: usize = 5000;
const MAX_ERRORS: usize = 200;

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(3600), || tick(&state), "job changes").await });
}

/// What they did with the project at their old company, for the score reason: "Moved from Acme, where they bought".
pub fn did(relationship: &str) -> &'static str {
    match relationship {
        "won" => "bought",
        "booked" => "booked a meeting",
        _ => "replied",
    }
}

/// The same for alerts and the email writer, which speak to and for the team.
fn did_with(relationship: &str, us: &str) -> String {
    match relationship {
        "won" => format!("bought from {us}"),
        "booked" => format!("booked a meeting with {us}"),
        _ => format!("replied to {us} with interest"),
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Watched {
    pub id: Uuid,
    #[serde(skip)]
    pub project_id: Uuid,
    pub lead_id: Option<Uuid>,
    /// won, booked or hot.
    pub relationship: String,
    pub full_name: String,
    pub email: Option<String>,
    pub linkedin_url: String,
    #[serde(skip)]
    pub linkedin_key: String,
    pub company_name: String,
    pub company_domain: Option<String>,
    #[serde(skip)]
    pub company_linkedin_id: String,
    pub title: String,
    #[serde(skip)]
    pub moves: i32,
    #[serde(skip)]
    pub pending_profile: Option<Value>,
    pub checked_at: Option<Ts>,
    pub check_error: Option<String>,
    pub created_at: Ts,
}

/// Adds the project's leads that bought, booked a meeting or replied hot, and moves someone already watched up to the
/// strongest thing they did. Someone already on the list by email or LinkedIn profile isn't added twice.
pub async fn enroll(db: &PgPool, project_id: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "with candidates as (
           select d.lead_id as id, 1 as rank from deals d where d.project_id = $1 and d.status = 'won'
           union all select l.id, 2 from leads l where l.project_id = $1 and (l.status = 'meeting_booked' or l.meeting_at is not null)
           union all select l.id, 3 from leads l where l.project_id = $1 and l.status = 'hot'
           union all select e.lead_id, 3 from email_messages e where e.project_id = $1 and e.status = 'received' and e.classification = 'hot'
         ), best as (
           select id, min(rank) as rank from candidates where id is not null group by id
         ), upgraded as (
           update watched_people w set relationship = case b.rank when 1 then 'won' else 'booked' end
           from best b where w.project_id = $1 and w.lead_id = b.id and b.rank < case w.relationship when 'won' then 1 when 'booked' then 2 else 3 end
         )
         insert into watched_people (project_id, lead_id, relationship, full_name, email, linkedin_url, linkedin_key, company_name, company_domain, title)
         select l.project_id, l.id, case b.rank when 1 then 'won' when 2 then 'booked' else 'hot' end, l.full_name, l.email, l.linkedin_url,
                coalesce(lower(substring(l.linkedin_url from 'linkedin\\.com/in/([^/?#]+)')), ''), l.company_name, l.company_domain, l.title
         from best b join leads l on l.id = b.id
         where l.project_id = $1 and not exists (select 1 from watched_people w where w.project_id = $1 and w.lead_id = l.id)
         on conflict do nothing",
    )
    .bind(project_id)
    .execute(db)
    .await?;
    Ok(())
}

#[derive(FromRow)]
struct Account {
    project_id: Uuid,
    unipile_dsn: String,
    unipile_token_enc: String,
    unipile_account_id: String,
}

/// Unipile failed or the LinkedIn account did, not the person: the project's checks stop until the next tick.
#[derive(Debug)]
struct AccountFailed(anyhow::Error);

/// Every hour: the watch lists take in new customers, booked meetings and hot replies, and each project with LinkedIn
/// connected reads its due profiles. A Unipile failure holds every project on that account for an hour, as it does sending.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    let db = &state.db;
    let projects: Vec<Uuid> = sqlx::query_scalar("select id from projects where org_id is not null").fetch_all(db).await?;
    for project_id in projects {
        enroll(db, project_id).await?;
    }
    let accounts: Vec<Account> = sqlx::query_as(
        "select s.project_id, s.unipile_dsn, s.unipile_token_enc, s.unipile_account_id
         from project_settings s join projects p on p.id = s.project_id
         where s.unipile_token_enc <> '' and p.org_id is not null
           and not exists (select 1 from project_settings o where o.unipile_account_id = s.unipile_account_id
                           and o.unipile_failed_at > now() - interval '1 hour')
         order by p.created_at",
    )
    .fetch_all(db)
    .await?;
    let mut failed = HashSet::new();
    for a in accounts {
        if failed.contains(&a.unipile_account_id) {
            continue;
        }
        let token = match crate::crypto::decrypt(&state.cfg.app_secret, &a.unipile_token_enc) {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!(project_id = %a.project_id, error = %e, "couldn't decrypt the Unipile token");
                continue;
            }
        };
        let unipile = Unipile::new(&a.unipile_dsn, &token, &a.unipile_account_id)?;
        // Held until this project's checks are done, so a second backend on the database skips it instead of reading twice.
        let mut lock = db.begin().await?;
        let owned: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended('job-changes:' || $1::text, 0))")
            .bind(a.project_id)
            .fetch_one(&mut *lock)
            .await?;
        if !owned {
            continue;
        }
        let checked = check_project(state, &unipile, &a.unipile_account_id, a.project_id).await;
        lock.commit().await?;
        match checked {
            Ok(Ok(())) => {}
            Ok(Err(AccountFailed(e))) => {
                failed.insert(a.unipile_account_id.clone());
                hooks::record(db, a.project_id, Alert::Unipile, &a.unipile_token_enc, &Err(e)).await;
            }
            Err(e) => tracing::error!(project_id = %a.project_id, error = ?e, "job change checks failed"),
        }
    }
    Ok(())
}

/// Counts a profile or company page read against the account's day, or says no once it has had DAILY_READS.
async fn take_read(db: &PgPool, account: &str) -> sqlx::Result<bool> {
    let reads: Option<i32> = sqlx::query_scalar(
        "insert into linkedin_profile_reads (account_id, day, reads) values ($1, (now() at time zone 'utc')::date, 1)
         on conflict (account_id, day) do update set reads = linkedin_profile_reads.reads + 1 where linkedin_profile_reads.reads < $2
         returning reads",
    )
    .bind(account)
    .bind(DAILY_READS)
    .fetch_optional(db)
    .await?;
    Ok(reads.is_some())
}

/// Reads the project's people not read in RECHECK_DAYS, never-read first, until the account's day runs out. Someone whose
/// move still waits on the new company's page goes first, from the profile already read.
async fn check_project(state: &AppState, unipile: &Unipile, account: &str, project_id: Uuid) -> anyhow::Result<Result<(), AccountFailed>> {
    let due: Vec<Watched> = sqlx::query_as(
        "select * from watched_people where project_id = $1 and linkedin_key <> ''
           and (pending_profile is not null or checked_at is null or checked_at < now() - make_interval(days => $2))
         order by pending_profile is null, checked_at nulls first, created_at limit $3",
    )
    .bind(project_id)
    .bind(RECHECK_DAYS)
    .bind(i64::from(DAILY_READS))
    .fetch_all(&state.db)
    .await?;
    for w in due {
        let profile = match &w.pending_profile {
            Some(profile) => profile.clone(),
            None => {
                if !take_read(&state.db, account).await? {
                    break;
                }
                match read_profile(state, unipile, &w).await? {
                    Ok(Some(profile)) => profile,
                    Ok(None) => continue,
                    Err(e) => return Ok(Err(e)),
                }
            }
        };
        if let Err(e) = check(state, unipile, account, &w, &profile).await? {
            return Ok(Err(e));
        }
    }
    Ok(Ok(()))
}

/// Reads the profile and marks it read. It's kept as pending_profile until check settles it, so a move whose company page
/// has to wait for tomorrow's reads or for Unipile doesn't read the profile again. None when LinkedIn couldn't show it.
async fn read_profile(state: &AppState, unipile: &Unipile, w: &Watched) -> anyhow::Result<Result<Option<Value>, AccountFailed>> {
    let (profile, error) = match unipile.work_profile(&w.linkedin_url).await {
        Ok(Ok(profile)) => (Some(profile), None),
        Ok(Err(reason)) => (None, Some(reason)),
        Err(e) => return Ok(Err(AccountFailed(e))),
    };
    sqlx::query("update watched_people set checked_at = now(), check_error = $2, pending_profile = $3 where id = $1")
        .bind(w.id)
        .bind(error)
        .bind(&profile)
        .execute(&state.db)
        .await?;
    Ok(Ok(profile))
}

/// Someone's current job on LinkedIn.
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub company: String,
    /// LinkedIn's id for the company page, or "" for a company typed in without one.
    pub company_id: String,
    pub title: String,
}

/// The first position on the profile that hasn't ended. None for someone between jobs.
pub fn current_position(profile: &Value) -> Option<Position> {
    let s = |v: &Value| v.as_str().unwrap_or_default().trim().to_string();
    profile["work_experience"].as_array()?.iter().find_map(|p| {
        let ended = !s(&p["end"]).is_empty() && !s(&p["end"]).eq_ignore_ascii_case("present");
        let company = s(&p["company"]);
        (!ended && !company.is_empty()).then(|| Position { company, company_id: s(&p["company_id"]), title: s(&p["position"]) })
    })
}

/// Lowercase letters and digits without a legal suffix, so "Acme, Inc." and "ACME" are one company and "Acme Labs" isn't.
fn company_key(name: &str) -> String {
    const LEGAL: [&str; 14] = ["inc", "llc", "ltd", "limited", "co", "corp", "corporation", "gmbh", "plc", "sa", "sas", "bv", "ag", "pty"];
    let lower = name.to_lowercase();
    let mut words: Vec<String> =
        lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect();
    while words.len() > 1 && words.last().is_some_and(|w| LEGAL.contains(&w.as_str())) {
        words.pop();
    }
    words.concat()
}

/// Compares the current position with what was last seen. The same LinkedIn id, or else the same name, is the same company.
/// A different one is a move unless the new company's page has the old website, since a company that renamed itself or
/// made a new page is still the same company.
async fn check(state: &AppState, unipile: &Unipile, account: &str, w: &Watched, profile: &Value) -> anyhow::Result<Result<(), AccountFailed>> {
    let db = &state.db;
    let Some(now) = current_position(profile) else {
        sqlx::query("update watched_people set pending_profile = null where id = $1").bind(w.id).execute(db).await?;
        return Ok(Ok(()));
    };
    let known = !w.company_name.trim().is_empty() || w.company_domain.is_some() || !w.company_linkedin_id.is_empty();
    let same = if !w.company_linkedin_id.is_empty() && !now.company_id.is_empty() {
        Some(w.company_linkedin_id == now.company_id)
    } else if !known || company_key(&w.company_name) == company_key(&now.company) {
        Some(true)
    } else {
        None
    };
    if same == Some(true) {
        seen(db, w.id, &now, None).await?;
        return Ok(Ok(()));
    }
    let mut domain = None;
    if !now.company_id.is_empty() {
        // Out of reads for today: the move waits in pending_profile for tomorrow, rather than being judged without the website.
        if !take_read(db, account).await? {
            return Ok(Ok(()));
        }
        match unipile.company_page(&now.company_id).await {
            Ok(Ok(page)) => domain = page["website"].as_str().and_then(util_input::domain),
            Ok(Err(_)) => {}
            Err(e) => return Ok(Err(AccountFailed(e))),
        }
    }
    let renamed = domain.as_deref().is_some_and(|d| w.company_domain.as_deref().is_some_and(|old| old.eq_ignore_ascii_case(d)));
    if renamed {
        seen(db, w.id, &now, domain.as_deref()).await?;
        return Ok(Ok(()));
    }
    if let Some(change) = record_move(db, w, &now, domain.as_deref(), profile).await? {
        after_move(state, change).await;
    }
    Ok(Ok(()))
}

/// Keeps the company and title last seen at the same company, and settles the profile read.
async fn seen<'e>(db: impl sqlx::PgExecutor<'e>, id: Uuid, now: &Position, domain: Option<&str>) -> sqlx::Result<()> {
    sqlx::query(
        "update watched_people set company_name = $2, company_linkedin_id = $3, title = $4, company_domain = coalesce($5, company_domain),
           pending_profile = null
         where id = $1",
    )
    .bind(id)
    .bind(&now.company)
    .bind(&now.company_id)
    .bind(&now.title)
    .bind(domain)
    .execute(db)
    .await?;
    Ok(())
}

/// Records the move, and makes the new company and a lead there, or finds the lead someone already added. Each move is
/// numbered from the person's moves as `w` saw them, so two backends that read the same profile record it once, while
/// going back to an earlier company later is a move again. Returns the new job change, or None when it was already recorded.
pub async fn record_move(db: &PgPool, w: &Watched, now: &Position, domain: Option<&str>, profile: &Value) -> anyhow::Result<Option<Uuid>> {
    let number = w.moves + 1;
    let mut tx = db.begin().await?;
    let change: Option<Uuid> = sqlx::query_scalar(
        "insert into job_changes (project_id, watched_id, old_company, old_domain, new_company, new_domain, move_number, new_title)
         values ($1, $2, $3, $4, $5, $6, $7, $8) on conflict (watched_id, move_number) do nothing returning id",
    )
    .bind(w.project_id)
    .bind(w.id)
    .bind(&w.company_name)
    .bind(&w.company_domain)
    .bind(&now.company)
    .bind(domain)
    .bind(number)
    .bind(&now.title)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(change) = change else {
        tx.commit().await?;
        return Ok(None);
    };
    // The old company's domain goes too, even with no new one known, or a later move back there would look like a rename.
    sqlx::query(
        "update watched_people set company_name = $2, company_linkedin_id = $3, title = $4, company_domain = $5, moves = $6,
           pending_profile = null
         where id = $1",
    )
    .bind(w.id)
    .bind(&now.company)
    .bind(&now.company_id)
    .bind(&now.title)
    .bind(domain)
    .bind(number)
    .execute(&mut *tx)
    .await?;

    let company_id: Option<Uuid> = match domain {
        Some(domain) => Some(
            sqlx::query_scalar(
                "insert into companies (project_id, domain, name) values ($1, $2, $3)
                 on conflict (project_id, domain) do update set name = case when companies.name = '' then excluded.name else companies.name end
                 returning id",
            )
            .bind(w.project_id)
            .bind(domain)
            .bind(&now.company)
            .fetch_one(&mut *tx)
            .await?,
        ),
        None => None,
    };
    // Someone may have added them there already, by hand or from a search.
    let existing: Option<Uuid> = sqlx::query_scalar(
        "select id from leads where project_id = $1 and lower(substring(linkedin_url from 'linkedin\\.com/in/([^/?#]+)')) = $2
           and (company_domain = $3 or (company_domain is null and lower(company_name) = lower($4)))
         order by created_at limit 1",
    )
    .bind(w.project_id)
    .bind(&w.linkedin_key)
    .bind(domain)
    .bind(&now.company)
    .fetch_optional(&mut *tx)
    .await?;
    let lead = match existing {
        Some(id) => id,
        None => {
            let s = |k: &str| profile[k].as_str().unwrap_or_default().trim().to_string();
            let full = Some(w.full_name.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("{} {}", s("first_name"), s("last_name")).trim().to_string());
            let (first, last) = match (s("first_name"), s("last_name")) {
                (f, l) if !f.is_empty() => (f, l),
                _ => full.split_once(' ').map_or((full.clone(), String::new()), |(f, l)| (f.to_string(), l.trim().to_string())),
            };
            // No email: the old one is at the company they left. The waterfall finds the new one from the name and domain.
            sqlx::query_scalar(
                "insert into leads (project_id, company_id, full_name, first_name, last_name, title, linkedin_url, linkedin_provider_id,
                   company_name, company_domain, source, owner_id)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'linkedin', (select owner_id from leads where id = $11))
                 returning id",
            )
            .bind(w.project_id)
            .bind(company_id)
            .bind(&full)
            .bind(&first)
            .bind(&last)
            .bind(&now.title)
            .bind(&w.linkedin_url)
            .bind(s("provider_id"))
            .bind(&now.company)
            .bind(domain)
            .bind(w.lead_id)
            .fetch_one(&mut *tx)
            .await?
        }
    };
    // The project's job change campaign takes them, unless they're already in one.
    let segment: Option<Uuid> = sqlx::query_scalar(
        "update leads set segment_id = s.job_change_segment_id from project_settings s
         where leads.id = $1 and s.project_id = $2 and s.job_change_segment_id is not null and leads.segment_id is null
         returning leads.segment_id",
    )
    .bind(lead)
    .bind(w.project_id)
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query("update job_changes set new_lead_id = $2, segment_id = $3 where id = $1").bind(change).bind(lead).bind(segment).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(change))
}

/// Scores the new lead, wakes the email finders and tells the team. Failures are logged, the move is already kept.
async fn after_move(state: &AppState, change: Uuid) {
    let lead: Option<Uuid> =
        sqlx::query_scalar("select new_lead_id from job_changes where id = $1").bind(change).fetch_one(&state.db).await.ok().flatten();
    if let Some(lead) = lead
        && let Err(e) = crate::score::refresh_lead(&state.db, lead).await
    {
        tracing::warn!(lead_id = %lead, error = ?e, "couldn't score the lead from a job change");
    }
    crate::waterfall::wake();
    match announcement(&state.db, &state.cfg.public_url, change).await {
        Ok(Some(a)) => announce(state, &a).await,
        Ok(None) => {}
        Err(e) => tracing::warn!(%change, error = ?e, "couldn't load the job change alert"),
    }
}

/// A job change alert: to chat, and by email to the old lead's owner, or to everyone when it had none.
#[derive(Debug)]
pub struct Announcement {
    pub org_id: Uuid,
    pub project_id: Uuid,
    pub recipients: Vec<String>,
    pub subject: String,
    pub summary: String,
    pub detail: String,
    pub link: String,
}

#[derive(FromRow)]
struct AlertRow {
    org_id: Uuid,
    project_id: Uuid,
    full_name: String,
    relationship: String,
    old_company: String,
    new_company: String,
    new_title: String,
    new_lead_id: Option<Uuid>,
    owner_id: Option<Uuid>,
    segment_name: Option<String>,
}

pub async fn announcement(db: &PgPool, public_url: &str, change: Uuid) -> anyhow::Result<Option<Announcement>> {
    let row: Option<AlertRow> = sqlx::query_as(
        "select p.org_id, j.project_id, w.full_name, w.relationship, j.old_company, j.new_company, j.new_title, j.new_lead_id,
                old.owner_id, s.name as segment_name
         from job_changes j join watched_people w on w.id = j.watched_id join projects p on p.id = j.project_id
         left join leads old on old.id = w.lead_id left join segments s on s.id = j.segment_id
         where j.id = $1 and p.org_id is not null",
    )
    .bind(change)
    .fetch_optional(db)
    .await?;
    let Some(r) = row else { return Ok(None) };
    let old = if r.old_company.is_empty() { "their last company".to_string() } else { r.old_company.clone() };
    let role = if r.new_title.is_empty() { String::new() } else { format!(" as {}", r.new_title) };
    let summary = format!("{} moved from {old} to {}{role}", r.full_name, r.new_company);
    let campaign = r.segment_name.map(|s| format!(" and added them to {s}")).unwrap_or_default();
    let detail = format!("They {} at {old}. Splee made them a lead at {}{campaign}.", did_with(&r.relationship, "you"), r.new_company);
    let link = match r.new_lead_id {
        Some(lead) => format!("{public_url}/app/p/{}/leads?lead={lead}", r.project_id),
        None => format!("{public_url}/app/p/{}/leads?tab=job-changes", r.project_id),
    };
    let recipients = crate::ownership::hot_lead_recipients(db, r.org_id, r.owner_id).await?;
    let subject = format!("Job change: {} is now at {}", r.full_name, r.new_company);
    Ok(Some(Announcement { org_id: r.org_id, project_id: r.project_id, recipients, subject, summary, detail, link }))
}

async fn announce(state: &AppState, a: &Announcement) {
    let alert = ChatAlert { label: "Job change", summary: &a.summary, quote: Some(&a.detail), link: &a.link, link_text: "Open the lead" };
    hooks::post_chat(&state.db, a.project_id, &alert).await;
    let body = format!("{}.\n\n{}\n\n{}", a.summary, a.detail, a.link);
    for to in &a.recipients {
        if let Err(e) = crate::mail::send_system_email(state, a.org_id, to, &a.subject, &body).await {
            tracing::warn!(error = ?e, "job change email failed");
        }
    }
}

/// A move for the Job changes tab and the API, newest first.
#[derive(Debug, Serialize, FromRow)]
pub struct Change {
    pub id: Uuid,
    pub full_name: String,
    pub linkedin_url: String,
    pub relationship: String,
    pub old_company: String,
    pub old_domain: Option<String>,
    pub old_lead_id: Option<Uuid>,
    pub new_company: String,
    pub new_domain: Option<String>,
    pub new_title: String,
    pub new_lead_id: Option<Uuid>,
    pub new_lead_email: Option<String>,
    pub segment_id: Option<Uuid>,
    pub segment_name: Option<String>,
    pub seen_at: Ts,
}

/// The tab shows this many moves at most. A watch list moves a few people a month.
const MAX_CHANGES: i64 = 500;

#[derive(Debug, Serialize)]
pub struct Overview {
    /// Whether the project has LinkedIn through Unipile, which the checks need.
    pub connected: bool,
    /// The campaign new leads from moves join, if the project picked one.
    pub segment_id: Option<Uuid>,
    pub watching: i64,
    pub last_checked_at: Option<Ts>,
    /// Moves nobody dismissed, newest first.
    pub changes: Vec<Change>,
}

/// The caller checks the project belongs to the organization.
pub async fn overview(db: &PgPool, project_id: Uuid) -> sqlx::Result<Overview> {
    let (connected, segment_id): (bool, Option<Uuid>) = sqlx::query_as(
        "select coalesce(bool_or(unipile_token_enc <> ''), false), (array_agg(job_change_segment_id))[1] from project_settings where project_id = $1",
    )
    .bind(project_id)
    .fetch_one(db)
    .await?;
    let (watching, last_checked_at): (i64, Option<Ts>) =
        sqlx::query_as("select count(*), max(checked_at) from watched_people where project_id = $1").bind(project_id).fetch_one(db).await?;
    let changes = sqlx::query_as(
        "select j.id, w.full_name, w.linkedin_url, w.relationship, j.old_company, j.old_domain::text, w.lead_id as old_lead_id, j.new_company,
                j.new_domain::text, j.new_title, j.new_lead_id, l.email::text as new_lead_email, l.segment_id, s.name as segment_name, j.seen_at
         from job_changes j join watched_people w on w.id = j.watched_id
         left join leads l on l.id = j.new_lead_id left join segments s on s.id = l.segment_id
         where j.project_id = $1 and j.dismissed_at is null order by j.seen_at desc limit $2",
    )
    .bind(project_id)
    .bind(MAX_CHANGES)
    .fetch_all(db)
    .await?;
    Ok(Overview { connected, segment_id, watching, last_checked_at, changes })
}

/// The tab lists this many people at most, the most recently added first.
const MAX_LISTED: i64 = 1000;

pub async fn watch_list(db: &PgPool, project_id: Uuid) -> sqlx::Result<Vec<Watched>> {
    sqlx::query_as("select * from watched_people where project_id = $1 order by created_at desc, full_name limit $2")
        .bind(project_id)
        .bind(MAX_LISTED)
        .fetch_all(db)
        .await
}

/// A past customer or champion added by hand or from a CSV.
#[derive(Debug, Default, Deserialize)]
pub struct Person {
    #[serde(default, alias = "full_name")]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub linkedin_url: String,
    #[serde(default, alias = "company_name")]
    pub company: String,
    #[serde(default)]
    pub company_domain: String,
    #[serde(default)]
    pub title: String,
}

#[derive(Debug, Deserialize)]
pub struct AddBody {
    /// What they did with you: won (bought), booked (a meeting) or hot (replied with interest). Bought when left out.
    #[serde(default = "bought")]
    pub relationship: String,
    #[serde(default)]
    pub people: Vec<Person>,
    /// A CSV with a header row: name, email, LinkedIn URL and company, in any order. Title and company domain are read too.
    #[serde(default)]
    pub csv: Option<String>,
}

fn bought() -> String {
    "won".into()
}

#[derive(Debug, Serialize)]
pub struct RowError {
    /// 1 for the first person, or the first row under a CSV's header.
    pub row: usize,
    pub error: String,
}

#[derive(Debug, Serialize)]
pub struct Added {
    pub added: i64,
    /// Already watched, with something filled in that was missing, like their LinkedIn link.
    pub updated: i64,
    pub skipped: i64,
    pub errors: Vec<RowError>,
}

/// People from a CSV's rows, matched to fields by their header.
fn read_csv(text: &str) -> AppResult<Vec<Person>> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).trim(csv::Trim::All).from_reader(text.trim_start_matches('\u{feff}').as_bytes());
    let headers = reader.headers().map_err(|e| AppError::BadRequest(format!("Couldn't read the CSV: {e}")))?.clone();
    let field = |h: &str| -> Option<&'static str> {
        let h: String = h.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
        Some(match h.as_str() {
            "name" | "fullname" | "contact" | "contactname" | "person" => "name",
            "firstname" | "first" => "first_name",
            "lastname" | "last" | "surname" => "last_name",
            "email" | "emailaddress" | "workemail" | "mail" => "email",
            "linkedin" | "linkedinurl" | "linkedinprofile" | "linkedinprofileurl" | "profileurl" | "linkedinlink" => "linkedin_url",
            "company" | "companyname" | "organization" | "organisation" | "account" | "accountname" | "employer" => "company",
            "companydomain" | "domain" | "website" | "companywebsite" => "company_domain",
            "title" | "jobtitle" | "position" | "role" => "title",
            _ => return None,
        })
    };
    let columns: Vec<Option<&str>> = headers.iter().map(field).collect();
    if !columns.iter().any(|c| matches!(c, Some("email" | "linkedin_url"))) {
        return Err(AppError::BadRequest("The CSV needs an email or a LinkedIn URL column".into()));
    }
    let mut people = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| AppError::BadRequest(format!("Couldn't read the CSV: {e}")))?;
        let (mut p, mut first, mut last) = (Person::default(), String::new(), String::new());
        for (column, value) in columns.iter().zip(record.iter()) {
            let value = value.to_string();
            match column {
                Some("name") => p.name = value,
                Some("first_name") => first = value,
                Some("last_name") => last = value,
                Some("email") => p.email = value,
                Some("linkedin_url") => p.linkedin_url = value,
                Some("company") => p.company = value,
                Some("company_domain") => p.company_domain = value,
                Some("title") => p.title = value,
                _ => {}
            }
        }
        if p.name.is_empty() {
            p.name = format!("{first} {last}").trim().to_string();
        }
        people.push(p);
    }
    Ok(people)
}

fn error(errors: &mut Vec<RowError>, row: usize, text: String) {
    if errors.len() < MAX_ERRORS {
        errors.push(RowError { row, error: text });
    }
}

/// Adds people to the project's watch list. Someone already on it, by email or LinkedIn profile, isn't added twice: what's
/// missing on their entry is filled in, and they move up to a stronger relationship.
pub async fn add(db: &PgPool, project_id: Uuid, body: AddBody) -> AppResult<Added> {
    util_input::one_of("relationship", &body.relationship, &["won", "booked", "hot"])?;
    let mut people = body.people;
    if let Some(csv) = body.csv.as_deref().filter(|c| !c.trim().is_empty()) {
        people.extend(read_csv(csv)?);
    }
    if people.is_empty() {
        return Err(AppError::BadRequest("Add at least one person".into()));
    }
    if people.len() > MAX_ADD {
        return Err(AppError::BadRequest(format!("Add up to {MAX_ADD} people at a time")));
    }
    let mut tx = db.begin().await?;
    // Two adds at once would both miss each other's people, so they take turns.
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('watch-list:' || $1::text, 0))").bind(project_id).execute(&mut *tx).await?;
    let (mut added, mut updated, mut skipped, mut errors) = (0, 0, 0, Vec::new());
    let (mut emails, mut profiles) = (HashSet::new(), HashSet::new());
    for (i, p) in people.iter().enumerate() {
        let row = i + 1;
        let email = util_input::email(&p.email);
        if email.is_none() && !p.email.trim().is_empty() {
            error(&mut errors, row, format!("Invalid email: {}", p.email.trim()));
            continue;
        }
        let key = unipile::profile_id(&p.linkedin_url).map(str::to_lowercase);
        if key.is_none() && !p.linkedin_url.trim().is_empty() {
            error(&mut errors, row, "The LinkedIn link isn't a profile link".into());
            continue;
        }
        if email.is_none() && key.is_none() {
            error(&mut errors, row, "Needs an email or a LinkedIn link".into());
            continue;
        }
        if email.as_ref().is_some_and(|e| !emails.insert(e.clone())) || key.as_ref().is_some_and(|k| !profiles.insert(k.clone())) {
            skipped += 1;
            error(&mut errors, row, "Listed twice".into());
            continue;
        }
        let linkedin_url = if key.is_some() { p.linkedin_url.trim().split(['?', '#']).next().unwrap_or_default().to_string() } else { String::new() };
        let domain = util_input::domain(&p.company_domain);
        let existing: Option<Uuid> = sqlx::query_scalar(
            "select id from watched_people where project_id = $1 and ((email = $2 and $2 is not null) or (linkedin_key = $3 and $3 <> ''))
             order by created_at limit 1",
        )
        .bind(project_id)
        .bind(&email)
        .bind(key.as_deref().unwrap_or_default())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(id) = existing {
            let changed = sqlx::query(
                "update watched_people set
                   full_name = case when full_name = '' then $2 else full_name end,
                   email = coalesce(email, $3),
                   linkedin_url = case when linkedin_key = '' and $5 <> '' then $4 else linkedin_url end,
                   linkedin_key = case when linkedin_key = '' then $5 else linkedin_key end,
                   company_name = case when company_name = '' then $6 else company_name end,
                   company_domain = coalesce(company_domain, $7),
                   title = case when title = '' then $8 else title end,
                   relationship = case when $9 = 'won' or ($9 = 'booked' and relationship = 'hot') then $9 else relationship end
                 where id = $1 and (
                   (full_name = '' and $2 <> '') or (email is null and $3 is not null) or (linkedin_key = '' and $5 <> '')
                   or (company_name = '' and $6 <> '') or (company_domain is null and $7 is not null) or (title = '' and $8 <> '')
                   or relationship <> case when $9 = 'won' or ($9 = 'booked' and relationship = 'hot') then $9 else relationship end)",
            )
            .bind(id)
            .bind(p.name.trim())
            .bind(&email)
            .bind(&linkedin_url)
            .bind(key.as_deref().unwrap_or_default())
            .bind(p.company.trim())
            .bind(&domain)
            .bind(p.title.trim())
            .bind(&body.relationship)
            .execute(&mut *tx)
            .await;
            match changed {
                Ok(done) if done.rows_affected() > 0 => updated += 1,
                Ok(_) => skipped += 1,
                // Their email or link is someone else's on the list.
                Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                    skipped += 1;
                    error(&mut errors, row, "Already on the list as someone else".into());
                }
                Err(e) => return Err(e.into()),
            }
            continue;
        }
        // Their lead in the project, when they're one, so the alert goes to its owner.
        let lead: Option<Uuid> = match &email {
            Some(email) => sqlx::query_scalar(
                "select id from leads l where project_id = $1 and email = $2
                   and not exists (select 1 from watched_people w where w.project_id = $1 and w.lead_id = l.id)",
            )
            .bind(project_id)
            .bind(email)
            .fetch_optional(&mut *tx)
            .await?,
            None => None,
        };
        let inserted = sqlx::query(
            "insert into watched_people (project_id, lead_id, relationship, full_name, email, linkedin_url, linkedin_key, company_name,
               company_domain, title)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) on conflict do nothing",
        )
        .bind(project_id)
        .bind(lead)
        .bind(&body.relationship)
        .bind(p.name.trim())
        .bind(&email)
        .bind(&linkedin_url)
        .bind(key.as_deref().unwrap_or_default())
        .bind(p.company.trim())
        .bind(&domain)
        .bind(p.title.trim())
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() > 0 { added += 1 } else { skipped += 1 }
    }
    tx.commit().await?;
    Ok(Added { added, updated, skipped, errors })
}

/// Hides a move from the tab and drops its score reason. The lead stays.
pub async fn dismiss(db: &PgPool, project_id: Uuid, change: Uuid) -> AppResult<()> {
    let done = sqlx::query("update job_changes set dismissed_at = now() where id = $1 and project_id = $2 and dismissed_at is null")
        .bind(change)
        .bind(project_id)
        .execute(db)
        .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Moves the lead a move made into the campaign. The caller checks the campaign is the project's.
pub async fn add_to_campaign(db: &PgPool, project_id: Uuid, change: Uuid, segment_id: Uuid) -> AppResult<()> {
    let lead: Option<Option<Uuid>> = sqlx::query_scalar("select new_lead_id from job_changes where id = $1 and project_id = $2")
        .bind(change)
        .bind(project_id)
        .fetch_optional(db)
        .await?;
    let lead = lead.ok_or(AppError::NotFound)?.ok_or_else(|| AppError::Conflict("The lead from this move was deleted".into()))?;
    let mut tx = db.begin().await?;
    sqlx::query("update leads set segment_id = $2 where id = $1").bind(lead).bind(segment_id).execute(&mut *tx).await?;
    sqlx::query("update job_changes set segment_id = $2 where id = $1").bind(change).bind(segment_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Picks the campaign leads from moves join on their own, or None for none. The caller checks the campaign is the project's.
pub async fn set_campaign(db: &PgPool, project_id: Uuid, segment_id: Option<Uuid>) -> sqlx::Result<()> {
    sqlx::query(
        "insert into project_settings (project_id, job_change_segment_id) values ($1, $2)
         on conflict (project_id) do update set job_change_segment_id = excluded.job_change_segment_id",
    )
    .bind(project_id)
    .bind(segment_id)
    .execute(db)
    .await?;
    Ok(())
}

/// What the email writer knows about a lead from a job change: where they were and what they did with us there.
pub async fn histories(db: &PgPool, lead_ids: &[Uuid]) -> sqlx::Result<std::collections::HashMap<Uuid, Value>> {
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "select distinct on (j.new_lead_id) j.new_lead_id, j.old_company, w.relationship, j.new_title
         from job_changes j join watched_people w on w.id = j.watched_id
         where j.new_lead_id = any($1) and j.dismissed_at is null order by j.new_lead_id, j.seen_at desc",
    )
    .bind(lead_ids)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(lead, old, relationship, title)| {
            (lead, serde_json::json!({ "previous_company": old, "what_they_did_there": did_with(&relationship, "us"), "new_role": title }))
        })
        .collect())
}

#[cfg(test)]
mod tests;
