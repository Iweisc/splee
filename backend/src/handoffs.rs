//! Handoffs: once a deal is won, the agent (agent/handoffs.rs) writes whoever takes the customer on what they need from
//! the sale: what they bought, what we promised them, who's who, what success means to them and what could go wrong. The
//! same run carries the deal's close plan on as the buyer's onboarding plan (close_plans.rs). Handing the customer to a
//! teammate emails them the handoff and tells the project's chats.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::hooks::{self, ChatAlert};
use crate::models::Ts;
use crate::state::AppState;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Promise {
    pub promise: String,
    /// Their or our words it rests on, word for word from an email or a call, or empty.
    pub quote: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Person {
    pub name: String,
    pub role: String,
    pub note: String,
}

/// What the agent writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Content {
    pub summary: String,
    pub bought: String,
    pub promises: Vec<Promise>,
    pub people: Vec<Person>,
    /// What they said success looks like.
    pub success: Vec<String>,
    pub risks: Vec<String>,
}

/// The handoff as plain text, for email.
pub fn text(c: &Content) -> String {
    let mut out = format!("{}\n\nWhat they bought\n{}", c.summary, c.bought);
    let list = |title: &str, items: Vec<String>| if items.is_empty() { String::new() } else { format!("\n\n{title}\n- {}", items.join("\n- ")) };
    out += &list("What we promised", c.promises.iter().map(|p| if p.quote.is_empty() { p.promise.clone() } else { format!("{} (\"{}\")", p.promise, p.quote) }).collect());
    out += &list("Who's who", c.people.iter().map(|p| [p.name.as_str(), p.role.as_str(), p.note.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(", ")).collect());
    out += &list("Success to them", c.success.clone());
    out += &list("Watch out for", c.risks.clone());
    out
}

/// The deal's handoff for the app.
#[derive(Debug, Serialize, FromRow)]
pub struct Handoff {
    pub id: Uuid,
    /// writing while the agent writes it, ready once it's written, else failed.
    pub status: String,
    pub handoff: Option<Json<Content>>,
    pub owner_id: Option<Uuid>,
    pub owner_name: Option<String>,
    pub handed_by: Option<String>,
    pub handed_at: Option<Ts>,
    pub created_at: Ts,
}

pub async fn for_deal(db: &PgPool, deal_id: Uuid) -> sqlx::Result<Option<Handoff>> {
    sqlx::query_as(
        "select h.id, h.handoff, h.owner_id, coalesce(nullif(o.name, ''), o.email::text) as owner_name,
                coalesce(nullif(b.name, ''), b.email::text) as handed_by, h.handed_at, h.created_at,
                case when r.status in ('queued', 'running') then 'writing' when h.handoff is not null then 'ready' else 'failed' end as status
         from handoffs h left join agent_runs r on r.id = h.run_id left join users o on o.id = h.owner_id left join users b on b.id = h.handed_by
         where h.deal_id = $1",
    )
    .bind(deal_id)
    .fetch_optional(db)
    .await
}

/// Has the agent write the handoff and the onboarding plan now. One being written is reused. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let (id, writing): (Uuid, Option<Uuid>) = sqlx::query_as(
        "with h as (
           insert into handoffs (project_id, deal_id) values ($1, $2)
           on conflict (deal_id) do update set project_id = excluded.project_id returning id, run_id)
         select h.id, r.id from h left join agent_runs r on r.id = h.run_id and r.status in ('queued', 'running')",
    )
    .bind(project_id)
    .bind(deal_id)
    .fetch_one(&mut *tx)
    .await?;
    if let Some(run) = writing {
        tx.commit().await?;
        return Ok(run);
    }
    let plan = crate::close_plans::ensure(&mut tx, project_id, deal_id).await?;
    let run = crate::agent::enqueue(&mut *tx, crate::agent::kind::HANDOFF, Some(project_id), Some(org_id), json!({ "handoff_id": id })).await?;
    sqlx::query("update handoffs set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    // So the plan reads as being written while the onboarding steps are.
    sqlx::query("update close_plans set run_id = $2 where id = $1").bind(plan).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(run)
}

/// The deal was just won: starts its handoff, the first time only. Failures are logged, they never fail the win.
pub async fn on_win(state: &AppState, deal_id: Uuid, project_id: Uuid) {
    let result: sqlx::Result<()> = async {
        let fresh: Option<Uuid> = sqlx::query_scalar(
            "select p.org_id from projects p where p.id = $1 and p.org_id is not null and not exists (select 1 from handoffs where deal_id = $2)",
        )
        .bind(project_id)
        .bind(deal_id)
        .fetch_optional(&state.db)
        .await?;
        let Some(org_id) = fresh else { return Ok(()) };
        // Queued rather than started here: wins happen inside agent runs too, and the agent's worker picks it up in a second.
        request(&state.db, deal_id, project_id, org_id).await.map(|_| ())
    }
    .await;
    if let Err(e) = result {
        tracing::warn!(error = ?e, deal = %deal_id, "couldn't start the handoff");
    }
}

/// Hands the customer to `owner`, a member of the organization, who gets the handoff by email.
pub async fn hand(state: &AppState, org_id: Uuid, deal_id: Uuid, owner: Uuid, by: Uuid) -> AppResult<()> {
    let found: Option<(String, String)> =
        sqlx::query_as("select u.email::text, coalesce(nullif(u.name, ''), u.email::text) from memberships m join users u on u.id = m.user_id where m.org_id = $1 and m.user_id = $2")
            .bind(org_id)
            .bind(owner)
            .fetch_optional(&state.db)
            .await?;
    let Some((email, owner_name)) = found else { return Err(AppError::BadRequest("Pick someone on your team".into())) };
    let row: Option<(Uuid, Json<Content>, String, String, String)> = sqlx::query_as(
        "update handoffs h set owner_id = $2, handed_by = $3, handed_at = now()
         from deals d, leads l, users b where d.id = h.deal_id and l.id = d.lead_id and b.id = $3 and h.deal_id = $1 and h.handoff is not null
         returning h.project_id, h.handoff, l.full_name, l.company_name, coalesce(nullif(b.name, ''), b.email::text)",
    )
    .bind(deal_id)
    .bind(owner)
    .bind(by)
    .fetch_optional(&state.db)
    .await?;
    let Some((project_id, Json(content), lead, company, by_name)) = row else {
        return Err(AppError::BadRequest("The handoff isn't written yet".into()));
    };
    let customer = if company.trim().is_empty() { lead.trim().to_string() } else { company.trim().to_string() };
    let link = format!("{}/app/pipeline?deal={deal_id}", state.cfg.public_url.trim_end_matches('/'));
    let summary = if owner == by { format!("{by_name} is taking on {customer}") } else { format!("{by_name} handed {customer} to {owner_name}") };
    let alert = ChatAlert { label: "Handoff", summary: &summary, quote: Some(&content.summary), link: &link, link_text: "Read the handoff" };
    hooks::post_chat(&state.db, project_id, &alert).await;
    if owner != by {
        let subject = format!("You're taking on {customer}");
        let body = format!("{by_name} handed {customer} to you. Here's what you need from the sale.\n\n{}\n\nThe deal in Splee: {link}", text(&content));
        if let Err(e) = crate::mail::send_system_email(state, org_id, &email, &subject, &body).await {
            tracing::warn!(error = ?e, "couldn't email the handoff");
        }
    }
    Ok(())
}
