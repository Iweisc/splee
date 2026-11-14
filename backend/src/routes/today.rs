//! The Today page: what's waiting on you across the organization's projects. Replies to answer, emails to approve, calls
//! due, meetings in the next day and deals gone quiet, each with its count and the first few. `owner` narrows everything
//! but approvals to whose leads they are, mine and unassigned by default like the Inbox count.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::inbox;
use super::util_pagination::PageQuery;
use crate::agent::nudges::QUIET_DAYS;
use crate::auth::Auth;
use crate::deals::LAST_TOUCH;
use crate::error::AppResult;
use crate::models::Ts;
use crate::ownership::{self, OwnerFilter};
use crate::state::AppState;

/// Items each part lists. The count says how many more there are.
const SHOWN: i64 = 5;

pub fn router() -> Router<AppState> {
    Router::new().route("/today", get(today))
}

#[derive(Deserialize)]
struct TodayQuery {
    owner: Option<String>,
}

#[derive(Serialize)]
struct Part<T> {
    total: i64,
    items: Vec<T>,
}

#[derive(Serialize)]
struct Reply {
    lead_id: Uuid,
    project_id: Uuid,
    project_name: String,
    name: String,
    company: String,
    classification: Option<String>,
    snippet: String,
    at: Ts,
}

#[derive(Serialize, FromRow)]
struct Waiting {
    project_id: Uuid,
    project_name: String,
    kind: String,
    count: i64,
}

#[derive(Serialize, FromRow)]
struct CallDue {
    id: Uuid,
    lead_id: Uuid,
    project_id: Uuid,
    name: String,
    company: String,
    due_at: Ts,
}

#[derive(Serialize, FromRow)]
struct Meeting {
    lead_id: Uuid,
    project_id: Uuid,
    name: String,
    company: String,
    meeting_at: Ts,
    /// The brief written for this call, if one is ready. See agent/briefs.rs.
    brief: bool,
}

#[derive(Serialize, FromRow)]
struct Quiet {
    deal_id: Uuid,
    lead_id: Uuid,
    project_id: Uuid,
    name: String,
    company: String,
    stage: String,
    value_cents: i64,
    currency: String,
    last_touch_at: Ts,
    /// A nudge the agent drafted this quiet stretch waits for approval. See agent/nudges.rs.
    nudge_ready: bool,
}

#[derive(Serialize)]
struct Today {
    replies: Part<Reply>,
    /// Drafts waiting for approval, by project and kind. Everyone's, since approving is the team's job.
    approvals: Vec<Waiting>,
    calls: Part<CallDue>,
    meetings: Vec<Meeting>,
    quiet: Part<Quiet>,
}

async fn today(State(state): State<AppState>, auth: Auth, Query(q): Query<TodayQuery>) -> AppResult<Json<Today>> {
    let owner = OwnerFilter::parse(Some(q.owner.as_deref().unwrap_or("mine_or_unassigned")), Some(auth.user_id))?;
    let db = &state.db;
    let threads = inbox::page_of(
        db,
        auth.org_id,
        Some(auth.user_id),
        inbox::ListQuery { project_id: None, filter: Some("needs_reply".into()), q: None, owner: q.owner.clone().or(Some("mine_or_unassigned".into())) },
        &PageQuery { page: Some(1), per_page: Some(SHOWN) },
    )
    .await?;
    let replies = Part {
        total: threads.total,
        items: threads
            .items
            .into_iter()
            .map(|t| Reply {
                lead_id: t.lead.lead.id,
                project_id: t.lead.lead.project_id,
                project_name: t.project_name,
                name: t.lead.lead.full_name.clone(),
                company: t.lead.lead.company_name.clone(),
                classification: t.classification,
                snippet: crate::mail::inbound::strip_quoted(&t.last_message.body).chars().take(160).collect::<String>().trim().to_string(),
                at: t.last_message.sent_at.unwrap_or(t.last_message.created_at),
            })
            .collect(),
    };
    Ok(Json(Today {
        replies,
        approvals: approvals(db, auth.org_id).await?,
        calls: calls(db, auth.org_id, &owner).await?,
        meetings: meetings(db, auth.org_id, &owner).await?,
        quiet: quiet(db, auth.org_id, &owner).await?,
    }))
}

async fn approvals(db: &PgPool, org_id: Uuid) -> sqlx::Result<Vec<Waiting>> {
    sqlx::query_as(
        "select p.id as project_id, p.name as project_name, e.kind, count(*) as count from email_messages e join projects p on p.id = e.project_id
         where p.org_id = $1 and e.direction = 'outbound' and e.status = 'draft'
         group by p.id, p.name, e.kind order by count(*) desc",
    )
    .bind(org_id)
    .fetch_all(db)
    .await
}

/// Open calls due by the end of the next 12 hours, the most overdue first.
async fn calls(db: &PgPool, org_id: Uuid, owner: &OwnerFilter) -> sqlx::Result<Part<CallDue>> {
    let from = format!(
        "from calls c join leads l on l.id = c.lead_id join projects p on p.id = c.project_id
         where p.org_id = $1 and c.status = 'open' and c.due_at < now() + interval '12 hours' and {}",
        ownership::sql("c", 2, 3)
    );
    let total = sqlx::query_scalar(&format!("select count(*) {from}")).bind(org_id).bind(owner.mode()).bind(owner.user()).fetch_one(db).await?;
    let items = sqlx::query_as(&format!(
        "select c.id, c.lead_id, c.project_id, l.full_name as name, l.company_name as company, c.due_at {from} order by c.due_at limit $4"
    ))
    .bind(org_id)
    .bind(owner.mode())
    .bind(owner.user())
    .bind(SHOWN)
    .fetch_all(db)
    .await?;
    Ok(Part { total, items })
}

/// Calls leads booked in the next day, and one that started in the last hour and may still be on.
async fn meetings(db: &PgPool, org_id: Uuid, owner: &OwnerFilter) -> sqlx::Result<Vec<Meeting>> {
    sqlx::query_as(&format!(
        "select l.id as lead_id, l.project_id, l.full_name as name, l.company_name as company, l.meeting_at,
                exists (select 1 from meeting_briefs b where b.lead_id = l.id and b.meeting_at = l.meeting_at and b.brief is not null) as brief
         from leads l join projects p on p.id = l.project_id
         where p.org_id = $1 and l.meeting_at > now() - interval '1 hour' and l.meeting_at < now() + interval '1 day' and {}
         order by l.meeting_at limit 20",
        ownership::sql("l", 2, 3)
    ))
    .bind(org_id)
    .bind(owner.mode())
    .bind(owner.user())
    .fetch_all(db)
    .await
}

/// Open deals with no email, call or stage change in QUIET_DAYS, the longest quiet first.
async fn quiet(db: &PgPool, org_id: Uuid, owner: &OwnerFilter) -> sqlx::Result<Part<Quiet>> {
    let rows = format!(
        "select d.id as deal_id, d.lead_id, d.project_id, l.full_name as name, l.company_name as company, s.name as stage, d.value_cents,
                d.currency, {LAST_TOUCH} as last_touch_at
         from deals d join leads l on l.id = d.lead_id join deal_stages s on s.id = d.stage_id
         where d.org_id = $1 and d.status = 'open' and {}",
        ownership::sql("l", 2, 3)
    );
    let quiet = format!("with q as ({rows}) select * from q where last_touch_at < now() - make_interval(days => $4)");
    let total = sqlx::query_scalar(&format!("select count(*) from ({quiet}) x"))
        .bind(org_id)
        .bind(owner.mode())
        .bind(owner.user())
        .bind(QUIET_DAYS)
        .fetch_one(db)
        .await?;
    let items = sqlx::query_as(&format!(
        "select x.deal_id, x.lead_id, x.project_id, x.name, x.company, x.stage, x.value_cents, x.currency, x.last_touch_at,
                exists (select 1 from deal_nudges n join email_messages e on e.id = n.email_id
                        where n.deal_id = x.deal_id and e.status = 'draft' and n.created_at >= x.last_touch_at) as nudge_ready
         from ({quiet}) x order by x.last_touch_at limit $5"
    ))
    .bind(org_id)
    .bind(owner.mode())
    .bind(owner.user())
    .bind(QUIET_DAYS)
    .bind(SHOWN)
    .fetch_all(db)
    .await?;
    Ok(Part { total, items })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn today_lists_what_waits_on_you_and_leaves_out_other_peoples_and_organizations(db: PgPool) {
        let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('dana@example.test', 'x') returning id").fetch_one(&db).await.unwrap();
        let other: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('jo@example.test', 'x') returning id").fetch_one(&db).await.unwrap();
        let mut orgs = vec![];
        for name in ["Ours", "Theirs"] {
            let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap();
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(&db).await.unwrap();
            let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Proposal', 'open', 0) returning id")
                .bind(org)
                .fetch_one(&db)
                .await
                .unwrap();
            // Ana replied and waits on us, Bo has a call due and a meeting soon, Cy's deal went quiet, and Di is someone else's.
            for (name, owner) in [("Ana", None), ("Bo", None), ("Cy", None), ("Di", Some(other))] {
                let lead: Uuid = sqlx::query_scalar(
                    "insert into leads (project_id, full_name, email, company_name, status, owner_id) values ($1, $2, lower($2) || '@acme.example', 'Acme', 'replied', $3) returning id",
                )
                .bind(project)
                .bind(name)
                .bind(owner)
                .fetch_one(&db)
                .await
                .unwrap();
                let setup = match name {
                    "Ana" | "Di" => "insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, created_at)
                                     values ($1, $2, 'inbound', 'inbound', 'Re: hi', 'How much is it?\n\nOn Tue we wrote:\n> hi', 'received', now())",
                    "Bo" => "with c as (insert into calls (project_id, lead_id, kind, status, due_at) values ($1, $2, 'manual', 'open', now() - interval '1 hour'))
                             update leads set meeting_at = now() + interval '3 hours' where id = $2",
                    _ => "insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, sent_at)
                          values ($1, $2, 'outbound', 'manual_reply', 'Proposal', 'Here it is', 'sent', now() - interval '20 days')",
                };
                sqlx::query(setup).bind(project).bind(lead).execute(&db).await.unwrap();
                if name == "Cy" {
                    sqlx::query("insert into deals (org_id, project_id, lead_id, stage_id, value_cents, stage_changed_at) values ($1, $2, $3, $4, 1800000, now() - interval '21 days')")
                        .bind(org)
                        .bind(project)
                        .bind(lead)
                        .bind(stage)
                        .execute(&db)
                        .await
                        .unwrap();
                }
            }
            sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, subject, body, status) select project_id, id, 'outbound', 'initial', 's', 'b', 'draft' from leads where project_id = $1 and full_name = 'Bo'")
                .bind(project)
                .execute(&db)
                .await
                .unwrap();
            orgs.push(org);
        }

        let state = crate::autopilot::tests::state(db.clone());
        let auth = Auth { user_id: user, org_id: orgs[0], role: "owner".into(), token_hash: String::new() };
        let Json(t) = today(State(state.clone()), auth, Query(TodayQuery { owner: None })).await.unwrap();
        assert_eq!(t.replies.total, 1, "Di's reply is someone else's");
        assert_eq!((t.replies.items[0].name.as_str(), t.replies.items[0].snippet.as_str()), ("Ana", "How much is it?"));
        assert_eq!(t.approvals.iter().map(|w| (w.kind.as_str(), w.count, w.project_name.as_str())).collect::<Vec<_>>(), vec![("initial", 1, "Ours")]);
        assert_eq!((t.calls.total, t.calls.items[0].name.as_str()), (1, "Bo"));
        assert_eq!(t.meetings.iter().map(|m| (m.name.as_str(), m.brief)).collect::<Vec<_>>(), vec![("Bo", false)]);
        assert_eq!((t.quiet.total, t.quiet.items[0].name.as_str(), t.quiet.items[0].nudge_ready), (1, "Cy", false));

        let auth = Auth { user_id: user, org_id: orgs[0], role: "owner".into(), token_hash: String::new() };
        let Json(t) = today(State(state), auth, Query(TodayQuery { owner: Some("everyone".into()) })).await.unwrap();
        assert_eq!(t.replies.total, 2, "everyone's replies");
    }
}
