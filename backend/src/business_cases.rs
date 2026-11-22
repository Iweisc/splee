//! Business cases: for an open deal, the agent (agent/business_cases.rs) writes what the product is worth to the buyer,
//! line by line, each number marked as something they told us or our estimate. The buyer's champion opens it at
//! PUBLIC_URL/case/{token}, puts in their own numbers, which the deal's window and the project's chats hear about, and
//! prints it for whoever signs. The page shows nothing until it's written, and nothing once the deal is lost.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::hooks::{self, ChatAlert};
use crate::models::Ts;

/// Biggest number a line takes, from the agent or the buyer.
pub const MAX_NUMBER: f64 = 1_000_000_000.0;

/// One thing the product is worth to them: `quantity` `unit`s, each worth `unit_value` in the deal's currency, every
/// `per`, for `count` `count_unit`s. Like 18 hours at 45 a week for 6 account managers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Line {
    pub label: String,
    pub quantity: f64,
    pub unit: String,
    pub unit_value: f64,
    /// week, month or year.
    pub per: String,
    pub count: f64,
    pub count_unit: String,
    /// said when the buyer gave the number, estimate when we did.
    pub basis: String,
    /// What they said it rests on, or how we estimated it.
    pub source: String,
}

/// The numbers of a line the buyer can change.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Numbers {
    pub quantity: f64,
    pub unit_value: f64,
    pub count: f64,
}

impl Line {
    pub fn numbers(&self) -> Numbers {
        Numbers { quantity: self.quantity, unit_value: self.unit_value, count: self.count }
    }
}

/// A year's worth of a line. A year is 48 working weeks. The page does the same sum (frontend case-view.tsx).
pub fn annual(per: &str, n: Numbers) -> f64 {
    let times = match per {
        "week" => 48.0,
        "month" => 12.0,
        _ => 1.0,
    };
    n.quantity * n.unit_value * n.count * times
}

/// What the agent writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Content {
    pub title: String,
    /// Two sentences for whoever signs.
    pub summary: String,
    /// Their problem, in their words where we have them.
    pub problem: String,
    pub lines: Vec<Line>,
    /// What changes that isn't money, like forecasts they trust.
    pub outcomes: Vec<String>,
    /// What waiting costs them.
    pub waiting: String,
}

impl Content {
    /// The year's total with `numbers` where the buyer gave them.
    pub fn annual(&self, numbers: Option<&[Numbers]>) -> f64 {
        self.lines.iter().enumerate().map(|(i, l)| annual(&l.per, numbers.and_then(|n| n.get(i).copied()).unwrap_or_else(|| l.numbers()))).sum()
    }
}

/// What the deal costs them in the first year: the proposal's first year once they can read it, else the deal's value.
#[derive(Debug, Serialize)]
pub struct Investment {
    pub amount: f64,
    pub currency: String,
    /// proposal or deal.
    pub from: &'static str,
}

async fn investment(db: &PgPool, deal_id: Uuid) -> sqlx::Result<Investment> {
    if let Some(p) = crate::proposals::for_deal(db, "", deal_id).await?.filter(|p| p.title.is_some() && !p.held && !p.items.is_empty()) {
        return Ok(Investment { amount: crate::proposals::first_year(&p.items), currency: p.currency, from: "proposal" });
    }
    let (cents, currency): (i64, String) = sqlx::query_as("select value_cents, currency from deals where id = $1").bind(deal_id).fetch_one(db).await?;
    Ok(Investment { amount: cents as f64 / 100.0, currency, from: "deal" })
}

/// The deal's business case for the app.
#[derive(Debug, Serialize, FromRow)]
pub struct Case {
    pub id: Uuid,
    pub deal_id: Uuid,
    pub token: String,
    /// writing while the agent writes it, ready once it's written, skipped when it said why not, else failed.
    pub status: String,
    pub business_case: Option<Json<Content>>,
    pub skipped: String,
    /// The buyer's own numbers, one per line, null until they change any.
    pub numbers: Option<Json<Vec<Numbers>>>,
    pub edited_by: String,
    pub edited_at: Option<Ts>,
    pub views: i32,
    pub first_viewed_at: Option<Ts>,
    pub last_viewed_at: Option<Ts>,
    #[sqlx(skip)]
    pub url: String,
    #[sqlx(skip)]
    pub investment: Option<Investment>,
}

pub fn url(public_url: &str, token: &str) -> String {
    format!("{}/case/{token}", public_url.trim_end_matches('/'))
}

pub async fn for_deal(db: &PgPool, public_url: &str, deal_id: Uuid) -> sqlx::Result<Option<Case>> {
    let case = sqlx::query_as::<_, Case>(
        "select c.id, c.deal_id, c.token, c.business_case, c.skipped, c.numbers, c.edited_by, c.edited_at, c.views, c.first_viewed_at, c.last_viewed_at,
                case when r.status in ('queued', 'running') then 'writing' when c.business_case is not null then 'ready'
                     when c.skipped <> '' then 'skipped' else 'failed' end as status
         from business_cases c left join agent_runs r on r.id = c.run_id where c.deal_id = $1",
    )
    .bind(deal_id)
    .fetch_optional(db)
    .await?;
    let Some(case) = case else { return Ok(None) };
    Ok(Some(Case { url: url(public_url, &case.token), investment: Some(investment(db, deal_id).await?), ..case }))
}

/// Has the agent write the case now, keeping its link. The buyer's numbers go with the old lines. One being written is
/// reused. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let (id, writing): (Uuid, Option<Uuid>) = sqlx::query_as(
        "with c as (
           insert into business_cases (project_id, deal_id) values ($1, $2)
           on conflict (deal_id) do update set project_id = excluded.project_id returning id, run_id)
         select c.id, r.id from c left join agent_runs r on r.id = c.run_id and r.status in ('queued', 'running')",
    )
    .bind(project_id)
    .bind(deal_id)
    .fetch_one(&mut *tx)
    .await?;
    if let Some(run) = writing {
        tx.commit().await?;
        return Ok(run);
    }
    let run = crate::agent::enqueue(&mut *tx, crate::agent::kind::BUSINESS_CASE, Some(project_id), Some(org_id), json!({ "case_id": id })).await?;
    sqlx::query("update business_cases set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(run)
}

/// What the champion's page shows.
#[derive(Debug, Serialize, FromRow)]
pub struct Public {
    pub business_case: Json<Content>,
    /// Which writing of the case this is. Saving numbers sends it back, so a page opened before the case was written
    /// again can't put its numbers on different lines.
    pub revision: String,
    pub numbers: Option<Json<Vec<Numbers>>>,
    pub edited_by: String,
    pub edited_at: Option<Ts>,
    pub first_name: String,
    pub company: String,
    pub company_domain: Option<String>,
    pub from_name: String,
    pub from_domain: Option<String>,
    pub sender_name: String,
    #[sqlx(skip)]
    pub investment: Option<Investment>,
}

pub async fn public(db: &PgPool, token: &str) -> sqlx::Result<Option<Public>> {
    let found: Option<Uuid> = sqlx::query_scalar(
        "select c.deal_id from business_cases c join deals d on d.id = c.deal_id where c.token = $1 and c.business_case is not null and d.status <> 'lost'",
    )
    .bind(token)
    .fetch_optional(db)
    .await?;
    let Some(deal_id) = found else { return Ok(None) };
    let page = sqlx::query_as::<_, Public>(
        "select c.business_case, md5(c.business_case::text) as revision, c.numbers, c.edited_by, c.edited_at, l.first_name, l.company_name as company, l.company_domain::text as company_domain,
                coalesce(nullif(pr.company->>'name', ''), pr.name) as from_name, pr.domain::text as from_domain, coalesce(s.sender_name, '') as sender_name
         from business_cases c join deals d on d.id = c.deal_id join leads l on l.id = d.lead_id join projects pr on pr.id = c.project_id
         left join project_settings s on s.project_id = c.project_id
         where c.deal_id = $1",
    )
    .bind(deal_id)
    .fetch_one(db)
    .await?;
    Ok(Some(Public { investment: Some(investment(db, deal_id).await?), ..page }))
}

/// The deal's link and who it's with, for the project's chats.
async fn about(db: &PgPool, public_url: &str, token: &str) -> sqlx::Result<(Uuid, String, String)> {
    let (project_id, deal_id, who): (Uuid, Uuid, String) = sqlx::query_as(
        "select c.project_id, c.deal_id, coalesce(nullif(trim(l.full_name), ''), l.email::text, 'The buyer')
                || coalesce(' (' || nullif(trim(l.company_name), '') || ')', '')
         from business_cases c join deals d on d.id = c.deal_id join leads l on l.id = d.lead_id where c.token = $1",
    )
    .bind(token)
    .fetch_one(db)
    .await?;
    Ok((project_id, format!("{}/app/pipeline?deal={deal_id}", public_url.trim_end_matches('/')), who))
}

/// Counts a look at the page and tells the project's chats about the first one.
pub async fn record_view(db: &PgPool, public_url: &str, token: &str) -> sqlx::Result<()> {
    let first: Option<bool> = sqlx::query_scalar(
        "update business_cases set views = views + 1, first_viewed_at = coalesce(first_viewed_at, now()), last_viewed_at = now()
         where token = $1 and business_case is not null returning views = 1",
    )
    .bind(token)
    .fetch_optional(db)
    .await?;
    if first == Some(true) {
        let (project_id, link, who) = about(db, public_url, token).await?;
        let summary = format!("{who} opened the business case");
        hooks::post_chat(db, project_id, &ChatAlert { label: "Business case opened", summary: &summary, quote: None, link: &link, link_text: "Open the deal" })
            .await;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct Edit {
    pub name: String,
    pub numbers: Vec<Numbers>,
    /// The page's `revision`. A page from before it was sent has none, and reloads like any other old page.
    #[serde(default)]
    pub revision: String,
}

const WRITTEN_AGAIN: &str = "The case was written again since you opened it. Reload the page to see it, then put your numbers in.";

/// Saves the buyer's own numbers, one per line, and tells the project's chats what the case comes to now.
pub async fn edit(db: &PgPool, public_url: &str, token: &str, edit: &Edit) -> AppResult<()> {
    let case: Option<(Json<Content>, String, String)> = sqlx::query_as(
        "select c.business_case, md5(c.business_case::text), d.currency from business_cases c join deals d on d.id = c.deal_id
         where c.token = $1 and c.business_case is not null and d.status <> 'lost'",
    )
    .bind(token)
    .fetch_optional(db)
    .await?;
    let (Json(case), revision, currency) = case.ok_or(AppError::NotFound)?;
    if edit.revision != revision {
        return Err(AppError::Conflict(WRITTEN_AGAIN.into()));
    }
    let fits = |n: f64| n.is_finite() && (0.0..=MAX_NUMBER).contains(&n);
    if edit.numbers.len() != case.lines.len() || !edit.numbers.iter().all(|n| fits(n.quantity) && fits(n.unit_value) && fits(n.count)) {
        return Err(AppError::BadRequest("Each number is between 0 and a billion".into()));
    }
    let name = edit.name.trim();
    // Only onto the writing it was checked against, in case the agent wrote it again in between.
    let saved = sqlx::query("update business_cases set numbers = $2, edited_by = $3, edited_at = now() where token = $1 and md5(business_case::text) = $4")
        .bind(token)
        .bind(Json(&edit.numbers))
        .bind(name)
        .bind(&revision)
        .execute(db)
        .await?;
    if saved.rows_affected() == 0 {
        return Err(AppError::Conflict(WRITTEN_AGAIN.into()));
    }
    let (project_id, link, who) = about(db, public_url, token).await?;
    let summary = format!(
        "{name} put their own numbers in {who}'s business case. It now comes to {} {currency} a year, against {} before.",
        rounded(case.annual(Some(&edit.numbers))),
        rounded(case.annual(None)),
    );
    hooks::post_chat(db, project_id, &ChatAlert { label: "Business case numbers", summary: &summary, quote: None, link: &link, link_text: "Open the deal" }).await;
    Ok(())
}

/// A sum in the currency's main unit as a whole number, like "84,240". It stays a float, since a line's numbers can each be
/// up to a billion and the sum can be past what an i64 of cents holds.
pub fn rounded(n: f64) -> String {
    crate::deals::grouped(&format!("{:.0}", n.max(0.0)))
}
