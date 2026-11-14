//! Proposals: the agent writes a deal's proposal from the conversation (agent/proposals.rs), the team sets the prices in the
//! deal's window, and the buyer reads it at PUBLIC_URL/proposal/{token} and accepts it with their name. Accepting wins the
//! deal at the proposal's first-year total and tells the project's chats, and the proposal stops changing.

use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::Json;
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::deals::{self, Move};
use crate::error::{AppError, AppResult};
use crate::hooks::{self, ChatAlert};
use crate::models::{Lead, Ts};
use crate::state::AppState;

pub const MAX_ITEMS: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Section {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Item {
    pub name: String,
    #[serde(default)]
    pub detail: String,
    pub quantity: i64,
    /// In the deal's currency's main unit, like 187.5.
    pub unit_price: f64,
    /// one_time, month or year.
    pub period: String,
}

/// What the items come to in the first year: one-time items once, monthly ones 12 times, yearly ones once.
pub fn first_year(items: &[Item]) -> f64 {
    items.iter().map(|i| i.quantity as f64 * i.unit_price * if i.period == "month" { 12.0 } else { 1.0 }).sum()
}

/// Items as saved: named, a known period, a quantity of at least 1, a price that isn't negative, at most MAX_ITEMS.
pub fn tidy_items(items: Vec<Item>) -> AppResult<Vec<Item>> {
    if items.len() > MAX_ITEMS {
        return Err(AppError::BadRequest(format!("A proposal lists up to {MAX_ITEMS} items")));
    }
    items
        .into_iter()
        .map(|i| {
            let name = i.name.trim().chars().take(120).collect::<String>();
            if name.is_empty() {
                return Err(AppError::BadRequest("Each item needs a name".into()));
            }
            if !["one_time", "month", "year"].contains(&i.period.as_str()) {
                return Err(AppError::BadRequest("period is one_time, month or year".into()));
            }
            if !(1..=100_000).contains(&i.quantity) || !i.unit_price.is_finite() || !(0.0..1e9).contains(&i.unit_price) {
                return Err(AppError::BadRequest("Each item needs a quantity of at least 1 and a price of zero or more".into()));
            }
            let unit_price = (i.unit_price * 100.0).round() / 100.0;
            Ok(Item { name, detail: i.detail.trim().chars().take(300).collect(), quantity: i.quantity, unit_price, period: i.period })
        })
        .collect()
}

/// The deal's proposal for the app.
#[derive(Debug, Serialize, FromRow)]
pub struct Proposal {
    pub id: Uuid,
    pub token: String,
    pub title: Option<String>,
    pub intro: String,
    pub sections: Json<Vec<Section>>,
    pub items: Json<Vec<Item>>,
    pub currency: String,
    pub valid_until: Option<NaiveDate>,
    /// writing while the agent drafts it, ready once it has a title, else failed.
    pub status: String,
    pub views: i32,
    pub first_viewed_at: Option<Ts>,
    pub last_viewed_at: Option<Ts>,
    pub accepted_at: Option<Ts>,
    pub accepted_name: String,
    pub accepted_title: String,
    #[sqlx(skip)]
    pub url: String,
}

pub fn url(public_url: &str, token: &str) -> String {
    format!("{}/proposal/{token}", public_url.trim_end_matches('/'))
}

pub async fn for_deal(db: &PgPool, public_url: &str, deal_id: Uuid) -> sqlx::Result<Option<Proposal>> {
    let p = sqlx::query_as::<_, Proposal>(
        "select p.id, p.token, p.title, p.intro, p.sections, p.items, d.currency, p.valid_until, p.views, p.first_viewed_at, p.last_viewed_at,
                p.accepted_at, p.accepted_name, p.accepted_title,
                case when r.status in ('queued', 'running') then 'writing' when p.title is not null then 'ready' else 'failed' end as status
         from proposals p join deals d on d.id = p.deal_id left join agent_runs r on r.id = p.run_id where p.deal_id = $1",
    )
    .bind(deal_id)
    .fetch_optional(db)
    .await?;
    Ok(p.map(|p| Proposal { url: url(public_url, &p.token), ..p }))
}

/// The deal's proposal, made empty the first time. Errors once it's accepted, since then it can't change.
pub async fn ensure_open(tx: &mut PgConnection, project_id: Uuid, deal_id: Uuid) -> AppResult<Uuid> {
    let token: String = crate::crypto::random_token().chars().take(20).collect();
    let (id, accepted): (Uuid, bool) = sqlx::query_as(
        "insert into proposals (project_id, deal_id, token) values ($1, $2, $3)
         on conflict (deal_id) do update set deal_id = excluded.deal_id returning id, accepted_at is not null",
    )
    .bind(project_id)
    .bind(deal_id)
    .bind(token)
    .fetch_one(tx)
    .await?;
    if accepted {
        return Err(AppError::BadRequest("The proposal was accepted, so it can't change".into()));
    }
    Ok(id)
}

#[derive(Deserialize)]
pub struct Changes {
    pub title: Option<String>,
    pub intro: Option<String>,
    pub items: Option<Vec<Item>>,
    /// Null clears it. Left out keeps it.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    pub valid_until: Option<Option<NaiveDate>>,
}

/// The team's edits, making the proposal first if the deal has none.
pub async fn update(db: &PgPool, project_id: Uuid, deal_id: Uuid, c: Changes) -> AppResult<()> {
    let items = c.items.map(tidy_items).transpose()?;
    let title = c.title.map(|t| t.trim().chars().take(160).collect::<String>());
    if title.as_deref() == Some("") {
        return Err(AppError::BadRequest("The proposal needs a title".into()));
    }
    let mut tx = db.begin().await?;
    let id = ensure_open(&mut tx, project_id, deal_id).await?;
    sqlx::query(
        "update proposals set title = coalesce($2, title), intro = coalesce($3, intro), items = coalesce($4, items),
                valid_until = case when $5 then $6 else valid_until end, updated_at = now()
         where id = $1",
    )
    .bind(id)
    .bind(title)
    .bind(c.intro.map(|i| i.trim().chars().take(1500).collect::<String>()))
    .bind(items.map(Json))
    .bind(c.valid_until.is_some())
    .bind(c.valid_until.flatten())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// What the buyer's page shows.
#[derive(Debug, Serialize, FromRow)]
pub struct Public {
    pub title: String,
    pub intro: String,
    pub sections: Json<Vec<Section>>,
    pub items: Json<Vec<Item>>,
    pub currency: String,
    pub valid_until: Option<NaiveDate>,
    pub accepted_at: Option<Ts>,
    pub accepted_name: String,
    /// The deal closed some other way, so the proposal can't be accepted.
    pub closed: bool,
    pub first_name: String,
    pub company: String,
    pub company_domain: Option<String>,
    pub from_name: String,
    pub from_domain: Option<String>,
    pub sender_name: String,
}

pub async fn public(db: &PgPool, token: &str) -> sqlx::Result<Option<Public>> {
    sqlx::query_as(
        "select p.title, p.intro, p.sections, p.items, d.currency, p.valid_until, p.accepted_at, p.accepted_name,
                p.accepted_at is null and d.status <> 'open' as closed,
                l.first_name, l.company_name as company, l.company_domain::text as company_domain,
                coalesce(nullif(pr.company->>'name', ''), pr.name) as from_name, pr.domain::text as from_domain,
                coalesce(nullif(m.from_name, ''), s.sender_name, '') as sender_name
         from proposals p join deals d on d.id = p.deal_id join leads l on l.id = d.lead_id join projects pr on pr.id = p.project_id
         left join project_settings s on s.project_id = p.project_id
         left join lateral (select e.mailbox_id from email_messages e where e.lead_id = l.id and e.direction = 'outbound' and e.status = 'sent'
                            and e.mailbox_id is not null order by e.sent_at desc limit 1) e on true
         left join mailboxes m on m.id = e.mailbox_id
         where p.token = $1 and p.title is not null",
    )
    .bind(token)
    .fetch_optional(db)
    .await
}

/// Counts a look at the buyer's page and returns whether it was the first.
pub async fn record_view(db: &PgPool, token: &str) -> sqlx::Result<bool> {
    let first: Option<bool> = sqlx::query_scalar(
        "update proposals set views = views + 1, first_viewed_at = coalesce(first_viewed_at, now()), last_viewed_at = now()
         where token = $1 and title is not null returning views = 1",
    )
    .bind(token)
    .fetch_optional(db)
    .await?;
    Ok(first == Some(true))
}

#[derive(Deserialize)]
pub struct Acceptance {
    pub name: String,
    #[serde(default)]
    pub title: String,
}

#[derive(FromRow)]
struct Accepted {
    deal_id: Uuid,
    org_id: Uuid,
    project_id: Uuid,
    items: Json<Vec<Item>>,
    title: String,
}

/// The buyer accepts: the proposal is marked and stops changing, and the deal is won at the first-year total, or at its
/// value when the proposal lists no prices.
pub async fn accept(state: &AppState, token: &str, a: Acceptance) -> AppResult<()> {
    let name = a.name.trim().chars().take(120).collect::<String>();
    if name.is_empty() {
        return Err(AppError::BadRequest("Type your name to accept".into()));
    }
    let db = &state.db;
    let row: Option<Accepted> = sqlx::query_as(
        "update proposals p set accepted_at = now(), accepted_name = $2, accepted_title = $3
         from deals d where d.id = p.deal_id and p.token = $1 and p.title is not null and p.accepted_at is null and d.status = 'open'
           and (p.valid_until is null or p.valid_until >= $4)
         returning p.deal_id, d.org_id, p.project_id, p.items, p.title",
    )
    .bind(token)
    .bind(&name)
    .bind(a.title.trim().chars().take(120).collect::<String>())
    .bind(Utc::now().date_naive())
    .fetch_optional(db)
    .await?;
    let Some(Accepted { deal_id, org_id, project_id, items, title }) = row else {
        return Err(AppError::BadRequest("This proposal can't be accepted anymore. Reply to the email it came in and we'll sort it out".into()));
    };
    let won: Option<Uuid> = sqlx::query_scalar("select id from deal_stages where org_id = $1 and kind = 'won' order by position limit 1")
        .bind(org_id)
        .fetch_optional(db)
        .await?;
    let total = first_year(&items);
    let deal = match won {
        Some(stage) => {
            let value = (!items.is_empty()).then_some(total);
            match deals::move_to(state, org_id, deal_id, Move { stage_id: Some(stage), value, ..Default::default() }).await {
                Ok(deal) => Some(deal),
                Err(e) => {
                    // Keep this simple until deals::move_to can join a transaction: undo the acceptance so the buyer can try again.
                    sqlx::query("update proposals set accepted_at = null, accepted_name = '', accepted_title = '' where token = $1").bind(token).execute(db).await?;
                    return Err(e);
                }
            }
        }
        None => None,
    };
    let lead_id: Uuid = sqlx::query_scalar("select lead_id from deals where id = $1").bind(deal_id).fetch_one(db).await?;
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(lead_id).fetch_one(db).await?;
    let company = if lead.company_name.trim().is_empty() { String::new() } else { format!(" ({})", lead.company_name.trim()) };
    let worth = match &deal {
        Some(d) if d.value_cents > 0 => format!(", {} {} in the first year", deals::amount(d.value_cents), d.currency),
        _ => String::new(),
    };
    let summary = format!("{name}{company} accepted \"{title}\"{worth}");
    let link = format!("{}/app/pipeline?deal={deal_id}", state.cfg.public_url.trim_end_matches('/'));
    let alert = ChatAlert { label: "Proposal accepted", summary: &summary, quote: None, link: &link, link_text: "Open the deal" };
    hooks::post_chat(db, project_id, &alert).await;
    Ok(())
}

/// Tells the project's chats the buyer opened the proposal for the first time.
pub async fn announce_view(db: &PgPool, public_url: &str, token: &str) -> anyhow::Result<()> {
    let (project_id, deal_id, lead_id): (Uuid, Uuid, Uuid) =
        sqlx::query_as("select p.project_id, p.deal_id, d.lead_id from proposals p join deals d on d.id = p.deal_id where p.token = $1").bind(token).fetch_one(db).await?;
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(lead_id).fetch_one(db).await?;
    let who = if lead.full_name.trim().is_empty() { lead.email.clone().unwrap_or_default() } else { lead.full_name.trim().to_string() };
    let who = if lead.company_name.trim().is_empty() { who } else { format!("{who} ({})", lead.company_name.trim()) };
    let summary = format!("{who} opened the proposal");
    let link = format!("{}/app/pipeline?deal={deal_id}", public_url.trim_end_matches('/'));
    let alert = ChatAlert { label: "Proposal opened", summary: &summary, quote: None, link: &link, link_text: "Open the deal" };
    hooks::post_chat(db, project_id, &alert).await;
    Ok(())
}
