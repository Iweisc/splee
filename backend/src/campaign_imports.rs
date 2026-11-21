//! A lead list sent to POST /v1/campaigns/import becomes a new campaign holding exactly the leads that could be imported. The
//! request makes the campaign, keeps every submitted lead in campaign_import_rows and returns. `drain` then runs each pending
//! import in one transaction that holds its row's lock, so two backends never run the same one, and an import cut off by a
//! restart rolls back to pending and runs again.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::error::AppResult;
use crate::models::Ts;
use crate::routes::suppression_lists::SUPPRESSED_VALUES;
use crate::routes::util_input;
use crate::routes::util_lead_import::{self, NewLead};

pub const MAX_LEADS: usize = 30_000;
const FAILED: &str = "Splee couldn't import these leads. Send them again with a new Idempotency-Key";
const CAMPAIGN_DELETED: &str = "The campaign was deleted before its leads were imported";
const CAMPAIGN_ARCHIVED: &str = "The campaign was archived before its leads were imported";
const SKIPPED: &str = "skipped";
const DEDUPED: &str = "deduped";

/// One lead as POST /v1/campaigns/import takes it. Every field but linkedin_url and client_ref is needed, and a lead without
/// one is skipped rather than failing the request, so any of them may be missing or null.
#[derive(Debug, Default, Deserialize)]
pub struct SubmittedLead {
    pub email: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company_domain: Option<String>,
    pub job_title: Option<String>,
    pub linkedin_url: Option<String>,
    /// The caller's own id for the lead, handed back with its outcome.
    pub client_ref: Option<String>,
}

/// Keeps the leads for `drain` and returns the import's id, in the transaction that made the campaign.
pub async fn queue(tx: &mut PgConnection, org_id: Uuid, project_id: Uuid, segment_id: Uuid, leads: Vec<SubmittedLead>) -> sqlx::Result<Uuid> {
    let mut cols: [Vec<String>; 6] = Default::default();
    let mut refs = Vec::with_capacity(leads.len());
    for lead in leads {
        let fields = [lead.email, lead.first_name, lead.last_name, lead.company_domain, lead.job_title, lead.linkedin_url];
        for (col, value) in cols.iter_mut().zip(fields) {
            col.push(value.unwrap_or_default());
        }
        refs.push(lead.client_ref);
    }
    let id: Uuid = sqlx::query_scalar("insert into campaign_imports (org_id, project_id, segment_id) values ($1, $2, $3) returning id")
        .bind(org_id)
        .bind(project_id)
        .bind(segment_id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query(
        "insert into campaign_import_rows (import_id, row_index, email, first_name, last_name, company_domain, job_title, linkedin_url, client_ref)
         select $1, (u.n - 1)::int, u.email, u.first_name, u.last_name, u.company_domain, u.job_title, u.linkedin_url, u.client_ref
         from unnest($2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::text[], $8::text[]) with ordinality
           as u(email, first_name, last_name, company_domain, job_title, linkedin_url, client_ref, n)",
    )
    .bind(id)
    .bind(&cols[0])
    .bind(&cols[1])
    .bind(&cols[2])
    .bind(&cols[3])
    .bind(&cols[4])
    .bind(&cols[5])
    .bind(&refs)
    .execute(&mut *tx)
    .await?;
    Ok(id)
}

pub fn spawn_worker(db: PgPool) {
    // The request starts a drain itself. This picks up an import a restart cut off.
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(60), || drain(&db), "campaign imports").await });
}

#[derive(FromRow)]
struct Pending {
    id: Uuid,
    org_id: Uuid,
    project_id: Uuid,
    segment_id: Option<Uuid>,
}

/// Runs every pending import, oldest first.
pub async fn drain(db: &PgPool) -> anyhow::Result<()> {
    loop {
        let mut tx = db.begin().await?;
        let task = sqlx::query_as::<_, Pending>(
            "select id, org_id, project_id, segment_id from campaign_imports where status = 'pending'
             order by created_at limit 1 for update skip locked",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(task) = task else { return Ok(()) };
        match run(&mut tx, &task).await {
            Ok(()) => tx.commit().await?,
            Err(e) => {
                tx.rollback().await?;
                tracing::error!(import_id = %task.id, error = ?e, "campaign import failed");
                sqlx::query("update campaign_imports set status = 'failed', error = $2, completed_at = now() where id = $1 and status = 'pending'")
                    .bind(task.id)
                    .bind(FAILED)
                    .execute(db)
                    .await?;
            }
        }
    }
}

#[derive(FromRow)]
struct Submitted {
    row_index: i32,
    email: String,
    first_name: String,
    last_name: String,
    company_domain: String,
    job_title: String,
    linkedin_url: String,
}

/// What the project already holds, and the emails seen so far in this import.
#[derive(Default)]
struct Known {
    suppressed: HashSet<String>,
    /// Each lead's lowercased email, and whether it was contacted.
    existing: HashMap<String, bool>,
    seen: HashSet<String>,
}

impl Known {
    /// The lead's email, or its outcome and reason code.
    fn classify(&mut self, lead: &Submitted) -> Result<String, (&'static str, &'static str)> {
        let email = match util_input::email(&lead.email) {
            Some(email) => email,
            None if lead.email.trim().is_empty() => return Err((SKIPPED, "missing_email")),
            None => return Err((SKIPPED, "invalid_email")),
        };
        for (value, reason) in [(&lead.first_name, "missing_first_name"), (&lead.last_name, "missing_last_name")] {
            if value.trim().is_empty() {
                return Err((SKIPPED, reason));
            }
        }
        let domain = match util_input::domain(&lead.company_domain) {
            Some(domain) => domain,
            None if lead.company_domain.trim().is_empty() => return Err((SKIPPED, "missing_company_domain")),
            None => return Err((SKIPPED, "invalid_company_domain")),
        };
        if lead.job_title.trim().is_empty() {
            return Err((SKIPPED, "missing_job_title"));
        }
        if !self.seen.insert(email.clone()) {
            return Err((DEDUPED, "duplicate_in_request"));
        }
        let email_domain = email.rsplit('@').next().unwrap_or_default();
        if [email.as_str(), email_domain, domain.as_str()].iter().any(|v| self.suppressed.contains(*v)) {
            return Err((DEDUPED, "do_not_contact"));
        }
        match self.existing.get(&email) {
            Some(true) => Err((DEDUPED, "already_contacted")),
            // A lead belongs to one campaign, so one already in the project stays where it is.
            Some(false) => Err((DEDUPED, "already_in_project")),
            None => Ok(email),
        }
    }
}

async fn run(tx: &mut PgConnection, task: &Pending) -> AppResult<()> {
    // Shared, so the campaign can't be deleted or archived while its leads go in.
    let segment = match task.segment_id {
        Some(id) => {
            sqlx::query_as::<_, (Uuid, bool)>("select id, archived_at is not null from segments where id = $1 for share")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
        }
        None => None,
    };
    let segment = match segment {
        Some((id, false)) => id,
        found => {
            sqlx::query("update campaign_imports set status = 'failed', error = $2, completed_at = now() where id = $1")
                .bind(task.id)
                .bind(if found.is_some() { CAMPAIGN_ARCHIVED } else { CAMPAIGN_DELETED })
                .execute(&mut *tx)
                .await?;
            return Ok(());
        }
    };
    let submitted = sqlx::query_as::<_, Submitted>(
        "select row_index, email, first_name, last_name, company_domain, job_title, linkedin_url from campaign_import_rows
         where import_id = $1 order by row_index",
    )
    .bind(task.id)
    .fetch_all(&mut *tx)
    .await?;
    let mut known = Known {
        suppressed: sqlx::query_scalar::<_, String>(SUPPRESSED_VALUES)
            .bind(task.org_id)
            .bind(task.project_id)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect(),
        existing: sqlx::query_as::<_, (String, bool)>(
            "select lower(email::text), last_contacted_at is not null from leads where project_id = $1 and email is not null",
        )
        .bind(task.project_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect(),
        seen: HashSet::new(),
    };

    let mut outcomes: Vec<(i32, &str, Option<&str>, Option<String>)> = Vec::with_capacity(submitted.len());
    let mut leads = Vec::new();
    for row in submitted {
        match known.classify(&row) {
            Ok(email) => {
                outcomes.push((row.row_index, "imported", None, Some(email.clone())));
                let lead = NewLead {
                    email,
                    first_name: row.first_name,
                    last_name: row.last_name,
                    title: row.job_title,
                    company_domain: row.company_domain,
                    linkedin_url: row.linkedin_url,
                    segment_id: Some(segment),
                    ..Default::default()
                };
                leads.push((row.row_index as usize, lead));
            }
            Err((outcome, reason)) => outcomes.push((row.row_index, outcome, Some(reason), None)),
        }
    }
    let inserted = util_lead_import::insert(&mut *tx, task.org_id, task.project_id, "api", leads).await?;
    let ids: HashMap<String, Uuid> = sqlx::query_as::<_, (Uuid, String)>("select id, lower(email::text) from leads where id = any($1)")
        .bind(&inserted.ids)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(id, email)| (email, id))
        .collect();

    let (mut rows, mut kinds, mut reasons, mut lead_ids) = (vec![], vec![], vec![], vec![]);
    for (row, outcome, reason, email) in outcomes {
        let lead_id = email.and_then(|e| ids.get(&e).copied());
        // Another import or request added the same email in between.
        let (outcome, reason) = if outcome == "imported" && lead_id.is_none() { (DEDUPED, Some("already_in_project")) } else { (outcome, reason) };
        rows.push(row);
        kinds.push(outcome);
        reasons.push(reason);
        lead_ids.push(lead_id);
    }
    sqlx::query(
        "update campaign_import_rows r set outcome = u.outcome, reason = u.reason, lead_id = u.lead_id
         from unnest($2::int[], $3::text[], $4::text[], $5::uuid[]) as u(row_index, outcome, reason, lead_id)
         where r.import_id = $1 and r.row_index = u.row_index",
    )
    .bind(task.id)
    .bind(&rows)
    .bind(&kinds)
    .bind(&reasons)
    .bind(&lead_ids)
    .execute(&mut *tx)
    .await?;
    let count = |kind: &str| kinds.iter().filter(|k| **k == kind).count() as i32;
    sqlx::query("update campaign_imports set status = 'completed', imported = $2, skipped = $3, deduped = $4, completed_at = now() where id = $1")
        .bind(task.id)
        .bind(count("imported"))
        .bind(count(SKIPPED))
        .bind(count(DEDUPED))
        .execute(&mut *tx)
        .await?;
    Ok(())
}

#[derive(Debug, FromRow)]
pub struct Task {
    pub id: Uuid,
    pub project_id: Uuid,
    pub segment_id: Option<Uuid>,
    pub status: String,
    pub imported: i32,
    pub skipped: i32,
    pub deduped: i32,
    pub error: String,
    pub created_at: Ts,
    pub completed_at: Option<Ts>,
}

pub async fn find(db: &PgPool, org_id: Uuid, id: Uuid) -> sqlx::Result<Option<Task>> {
    sqlx::query_as(
        "select id, project_id, segment_id, status, imported, skipped, deduped, error, created_at, completed_at from campaign_imports
         where id = $1 and org_id = $2",
    )
    .bind(id)
    .bind(org_id)
    .fetch_optional(db)
    .await
}

/// A submitted lead and what became of it.
#[derive(Debug, Serialize, FromRow)]
pub struct RowOutcome {
    /// Its zero-based index in the request's leads.
    pub row: i32,
    pub email: String,
    pub client_ref: Option<String>,
    /// pending until the import runs, then imported, skipped or deduped.
    pub status: String,
    /// Why it was skipped or deduped, as a code like missing_job_title or do_not_contact.
    pub reason: Option<String>,
    /// The lead it became, while the lead exists.
    pub person_id: Option<Uuid>,
}

/// A page of the import's rows in the order they were sent, and how many it has.
pub async fn rows(db: &PgPool, import_id: Uuid, limit: i64, offset: i64) -> sqlx::Result<(Vec<RowOutcome>, i64)> {
    let items = sqlx::query_as(
        r#"select row_index as "row", email, client_ref, coalesce(outcome, 'pending') as status, reason, lead_id as person_id
           from campaign_import_rows where import_id = $1 order by row_index limit $2 offset $3"#,
    )
    .bind(import_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await?;
    let total = sqlx::query_scalar("select count(*) from campaign_import_rows where import_id = $1").bind(import_id).fetch_one(db).await?;
    Ok((items, total))
}

#[cfg(test)]
mod tests {
    use super::{DEDUPED, Known, SKIPPED, Submitted};

    fn lead(email: &str) -> Submitted {
        Submitted {
            row_index: 0,
            email: email.into(),
            first_name: "Sarah".into(),
            last_name: "Chen".into(),
            company_domain: "https://www.Northwind.studio/".into(),
            job_title: "Founder".into(),
            linkedin_url: String::new(),
        }
    }

    #[test]
    fn a_lead_missing_a_needed_field_is_skipped_with_its_reason() {
        let mut known = Known::default();
        let blank = |f: fn(&mut Submitted)| {
            let mut l = lead("sarah@northwind.studio");
            f(&mut l);
            l
        };
        let cases = [
            (blank(|l| l.email = " ".into()), "missing_email"),
            (blank(|l| l.email = "sarah@".into()), "invalid_email"),
            (blank(|l| l.first_name = String::new()), "missing_first_name"),
            (blank(|l| l.last_name = String::new()), "missing_last_name"),
            (blank(|l| l.company_domain = String::new()), "missing_company_domain"),
            (blank(|l| l.company_domain = "not a domain".into()), "invalid_company_domain"),
            (blank(|l| l.job_title = "  ".into()), "missing_job_title"),
        ];
        for (l, reason) in cases {
            assert_eq!(known.classify(&l), Err((SKIPPED, reason)));
        }
        assert_eq!(known.classify(&lead(" Sarah@Northwind.studio ")), Ok("sarah@northwind.studio".into()));
    }

    #[test]
    fn repeats_known_leads_and_do_not_contact_entries_are_deduped() {
        let mut known = Known {
            suppressed: ["blocked@acme.example".to_string(), "banned.example".into(), "northwind.studio".into()].into(),
            existing: [("sent@acme.example".to_string(), true), ("waiting@acme.example".into(), false)].into(),
            ..Default::default()
        };
        let other = |email: &str| Submitted { company_domain: "acme.example".into(), ..lead(email) };
        assert_eq!(known.classify(&other("new@acme.example")), Ok("new@acme.example".into()));
        assert_eq!(known.classify(&other("NEW@acme.example")), Err((DEDUPED, "duplicate_in_request")));
        assert_eq!(known.classify(&other("blocked@acme.example")), Err((DEDUPED, "do_not_contact")));
        assert_eq!(known.classify(&other("anyone@banned.example")), Err((DEDUPED, "do_not_contact")));
        // The company's domain counts too, not only the email's.
        assert_eq!(known.classify(&lead("sarah@gmail.com")), Err((DEDUPED, "do_not_contact")));
        assert_eq!(known.classify(&other("sent@acme.example")), Err((DEDUPED, "already_contacted")));
        assert_eq!(known.classify(&other("waiting@acme.example")), Err((DEDUPED, "already_in_project")));
    }
}
