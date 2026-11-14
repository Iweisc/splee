//! Deal desk: a proposal priced past the organization's discount limit is held from the buyer (proposals::HELD) until an
//! owner or admin approves its discount. Anyone on the deal asks, with a reason, the agent reads the deal and recommends
//! approving, countering or declining (agent/deal_desk.rs), and an owner or admin decides. An approval covers the proposal
//! while its discount stays at or under what was approved. Asking and deciding are posted to the project's chats.

use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::hooks::{self, ChatAlert};
use crate::models::Ts;
use crate::state::AppState;

/// An approval asked for, with its deal for the Today list.
#[derive(Debug, Serialize, FromRow)]
pub struct Approval {
    pub id: Uuid,
    pub deal_id: Uuid,
    pub project_id: Uuid,
    pub lead_name: String,
    pub company: String,
    pub discount_pct: f64,
    pub discount_limit_pct: Option<i32>,
    pub reason: String,
    pub asked_by: Option<String>,
    pub created_at: Ts,
    /// The agent's {verdict, counter_pct, summary, reasons, ask_for}, once written.
    pub read: Option<Value>,
    /// writing, ready or failed.
    pub read_status: String,
    /// asked, approved or declined.
    pub status: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<Ts>,
    pub note: String,
}

const APPROVALS: &str = "select a.id, p.deal_id, p.project_id, l.full_name as lead_name, l.company_name as company, a.discount_pct::float8 as discount_pct,
        o.discount_limit_pct, a.reason, coalesce(nullif(u.name, ''), u.email::text) as asked_by, a.created_at, a.read,
        case when a.read is not null then 'ready' when r.status in ('queued', 'running') then 'writing' else 'failed' end as read_status,
        a.status, coalesce(nullif(v.name, ''), v.email::text) as decided_by, a.decided_at, a.note
    from discount_approvals a join proposals p on p.id = a.proposal_id join deals d on d.id = p.deal_id join leads l on l.id = d.lead_id
    join organizations o on o.id = d.org_id left join users u on u.id = a.asked_by left join users v on v.id = a.decided_by
    left join agent_runs r on r.id = a.run_id";

/// The proposal's latest approval, whatever came of it.
pub async fn latest(db: &PgPool, proposal_id: Uuid) -> sqlx::Result<Option<Approval>> {
    sqlx::query_as(&format!("{APPROVALS} where a.proposal_id = $1 order by a.created_at desc limit 1")).bind(proposal_id).fetch_optional(db).await
}

/// Approvals waiting on a decision in the organization, the oldest first.
pub async fn waiting(db: &PgPool, org_id: Uuid) -> sqlx::Result<Vec<Approval>> {
    sqlx::query_as(&format!("{APPROVALS} where d.org_id = $1 and a.status = 'asked' and d.status = 'open' order by a.created_at")).bind(org_id).fetch_all(db).await
}

/// The deal's company, or the lead's name when it has none.
fn company(lead: &str, company: &str) -> String {
    if company.trim().is_empty() { lead.trim().to_string() } else { company.trim().to_string() }
}

fn pct(p: f64) -> String {
    let s = format!("{p:.2}");
    format!("{}%", s.trim_end_matches('0').trim_end_matches('.'))
}

async fn post(state: &AppState, a: &Approval, label: &str, summary: &str) {
    let link = format!("{}/app/pipeline?deal={}", state.cfg.public_url.trim_end_matches('/'), a.deal_id);
    hooks::post_chat(&state.db, a.project_id, &ChatAlert { label, summary, quote: (!a.reason.is_empty()).then_some(a.reason.as_str()), link: &link, link_text: "Open the deal" })
        .await;
}

/// Asks for the deal's proposal's discount to be approved, replacing an ask still waiting, and starts the agent's read.
/// Errors when the proposal isn't held.
pub async fn ask(state: &AppState, org_id: Uuid, deal_id: Uuid, user_id: Uuid, reason: &str) -> AppResult<Uuid> {
    let reason = reason.trim().chars().take(600).collect::<String>();
    if reason.is_empty() {
        return Err(AppError::BadRequest("Say why this discount is needed".into()));
    }
    let mut tx = state.db.begin().await?;
    let held: Option<(Uuid, Uuid, bool)> = sqlx::query_as(&format!(
        "select p.id, p.project_id, {} from proposals p join deals d on d.id = p.deal_id join organizations o on o.id = d.org_id
         where p.deal_id = $1 and d.org_id = $2 and d.status = 'open' and p.accepted_at is null for update of p",
        crate::proposals::HELD
    ))
    .bind(deal_id)
    .bind(org_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((proposal_id, project_id, true)) = held else {
        return Err(AppError::BadRequest("This proposal is within the discount limit, so it needs no approval".into()));
    };
    sqlx::query("delete from discount_approvals where proposal_id = $1 and status = 'asked'").bind(proposal_id).execute(&mut *tx).await?;
    let id: Uuid = sqlx::query_scalar(
        "insert into discount_approvals (proposal_id, discount_pct, reason, asked_by) select id, discount_pct, $2, $3 from proposals where id = $1 returning id",
    )
    .bind(proposal_id)
    .bind(&reason)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    let run = crate::agent::enqueue(&mut *tx, crate::agent::kind::DEAL_DESK, Some(project_id), Some(org_id), json!({ "approval_id": id })).await?;
    sqlx::query("update discount_approvals set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    if let Some(a) = latest(&state.db, proposal_id).await? {
        let limit = a.discount_limit_pct.map(|l| format!(", past the {l}% limit")).unwrap_or_default();
        let summary = format!("{} asked for {} off for {}{limit}", a.asked_by.as_deref().unwrap_or("Someone"), pct(a.discount_pct), company(&a.lead_name, &a.company));
        post(state, &a, "Discount approval asked", &summary).await;
    }
    Ok(run)
}

/// An owner or admin approves or declines a waiting ask.
pub async fn decide(state: &AppState, org_id: Uuid, id: Uuid, user_id: Uuid, approve: bool, note: &str) -> AppResult<Uuid> {
    let decided: Option<Uuid> = sqlx::query_scalar(
        "update discount_approvals a set status = $3, decided_by = $4, decided_at = now(), note = $5
         from proposals p join deals d on d.id = p.deal_id
         where a.id = $1 and p.id = a.proposal_id and d.org_id = $2 and a.status = 'asked' returning p.id",
    )
    .bind(id)
    .bind(org_id)
    .bind(if approve { "approved" } else { "declined" })
    .bind(user_id)
    .bind(note.trim().chars().take(600).collect::<String>())
    .fetch_optional(&state.db)
    .await?;
    let Some(proposal_id) = decided else {
        return Err(AppError::BadRequest("That ask was already decided or replaced".into()));
    };
    if let Some(a) = latest(&state.db, proposal_id).await? {
        let who = a.decided_by.as_deref().unwrap_or("An admin");
        let (label, verb) = if approve { ("Discount approved", "approved") } else { ("Discount declined", "declined") };
        let note = if a.note.is_empty() { String::new() } else { format!(": {}", a.note) };
        post(state, &a, label, &format!("{who} {verb} {} off for {}{note}", pct(a.discount_pct), company(&a.lead_name, &a.company))).await;
    }
    Ok(proposal_id)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::proposals::{Item, discount, stamp};

    pub fn item(price: f64, product: Option<Uuid>) -> Item {
        Item { name: "Seats".into(), detail: String::new(), quantity: 8, unit_price: price, period: "month".into(), product_id: product, list_price: None }
    }

    #[test]
    fn list_prices_come_from_the_price_list_and_the_deepest_discount_counts() {
        let seats = Uuid::new_v4();
        let gone = Uuid::new_v4();
        let products = [(seats, 200.0)];
        let items = stamp(vec![item(150.0, Some(seats)), item(10.0, Some(gone)), item(5.0, None)], &products, true);
        assert_eq!(items.iter().map(|i| (i.product_id, i.list_price)).collect::<Vec<_>>(), [(Some(seats), Some(200.0)), (None, None), (None, None)]);
        assert_eq!(discount(&items), 25.0, "the one listed item is 25% off, the rest don't count");
        assert_eq!(discount(&stamp(vec![item(250.0, Some(seats))], &products, true)), 0.0, "above list is no discount");
        assert_eq!(stamp(vec![item(150.0, Some(seats))], &products, false)[0].list_price, None, "another currency has no list price");
        assert_eq!(discount(&stamp(vec![item(133.33, Some(seats))], &products, true)), 33.34);
        assert_eq!(pct(25.0), "25%");
        assert_eq!(pct(33.34), "33.34%");
    }
}
