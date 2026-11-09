use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

use super::leads::{self, LeadDetail, LeadRow};
use super::util_input::{self, LEAD_STATUSES};
use super::util_pagination::{PageQuery, Paginated};
use super::util_scope;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::{EmailMessage, Lead};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/inbox", get(list))
        .route("/inbox/threads/{lead_id}", get(thread))
        .route("/inbox/threads/{lead_id}/reply", post(reply))
        .route("/inbox/threads/{lead_id}/status", post(set_status))
        .route("/inbox/threads/{lead_id}/done", post(mark_done))
        .route("/inbox/messages/{id}/approve", post(approve))
}

#[derive(Deserialize)]
struct ListQuery {
    project_id: Option<Uuid>,
    filter: Option<String>,
}

#[derive(Serialize)]
struct InboxThread {
    lead: LeadRow,
    last_message: EmailMessage,
    pending_auto_reply: bool,
    classification: Option<String>,
    project_name: String,
}

#[derive(FromRow)]
struct ThreadRef {
    lead_id: Uuid,
    last_message_id: Uuid,
    pending_auto_reply: bool,
    classification: Option<String>,
    project_name: String,
}

// One thread per lead with at least one inbound message. Drafts and cancelled emails never count as the
// last message, so a pending auto-reply doesn't hide that the lead is waiting for an answer.
const THREADS: &str = "with t as (
       select l.id as lead_id, l.status as lead_status, p.name as project_name,
         last.id as last_message_id, last.direction as last_direction, last.at as last_at,
         reply.classification, reply.handled_at is not null as handled,
         exists (select 1 from email_messages d where d.lead_id = l.id and d.kind = 'auto_reply'
           and d.status = 'draft') as pending_auto_reply,
         exists (select 1 from email_messages a where a.lead_id = l.id and a.kind = 'auto_reply'
           and a.status in ('approved', 'scheduled', 'sending', 'sent')) as auto_replied
       from leads l
       join projects p on p.id = l.project_id
       cross join lateral (
         select e.id, e.direction, coalesce(e.sent_at, e.created_at) as at from email_messages e
         where e.lead_id = l.id and e.status not in ('draft', 'cancelled')
         order by at desc limit 1
       ) last
       cross join lateral (
         select i.classification, i.handled_at from email_messages i
         where i.lead_id = l.id and i.direction = 'inbound'
         order by i.created_at desc limit 1
       ) reply
       where p.org_id = $1 and ($2::uuid is null or l.project_id = $2)
         -- The reply join above already drops leads without one. This lets Postgres start from the few leads that
         -- replied instead of looking up the last message of every lead in the org: 490 ms to 30 ms at 50k leads.
         and l.id in (select i.lead_id from email_messages i where i.direction = 'inbound')
     ), f as (
       select *,
         lead_status in ('hot', 'meeting_booked') as is_hot,
         -- Declines, out-of-office notes, unsubscribes and bounces are answered by the classifier, not by you.
         -- A pending auto-reply draft still waits on you to approve it, so it counts. Marking the lead as booked or
         -- not interested, or marking the thread done, is how you clear a reply you handled outside Splee.
         last_direction = 'inbound' and not handled
           and coalesce(classification, '') not in ('not_interested', 'ooo', 'unsubscribe', 'bounce')
           and lead_status not in ('meeting_booked', 'not_interested', 'bounced', 'unsubscribed', 'do_not_contact') as is_needs_reply
       from t
     )
     select * from f where case $3
       when 'hot' then is_hot
       when 'needs_reply' then is_needs_reply
       when 'auto_replied' then auto_replied
       when 'other' then not (is_hot or is_needs_reply or auto_replied)
       else true end";

/// Threads across the organization in the Needs reply filter, for the digest emails.
pub(crate) async fn needs_reply_count(db: &sqlx::PgPool, org_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar(&format!("select count(*) from ({THREADS}) x"))
        .bind(org_id)
        .bind(None::<Uuid>)
        .bind("needs_reply")
        .fetch_one(db)
        .await
}

async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<InboxThread>>> {
    let filter = q.filter.unwrap_or_else(|| "all".into());
    util_input::one_of("filter", &filter, &["all", "hot", "needs_reply", "auto_replied", "other"])?;

    let total: i64 = sqlx::query_scalar(&format!("select count(*) from ({THREADS}) x"))
        .bind(auth.org_id)
        .bind(q.project_id)
        .bind(&filter)
        .fetch_one(&state.db)
        .await?;
    let refs = sqlx::query_as::<_, ThreadRef>(&format!("{THREADS} order by last_at desc limit $4 offset $5"))
        .bind(auth.org_id)
        .bind(q.project_id)
        .bind(&filter)
        .bind(page.limit())
        .bind(page.offset())
        .fetch_all(&state.db)
        .await?;

    let lead_ids: Vec<Uuid> = refs.iter().map(|r| r.lead_id).collect();
    let message_ids: Vec<Uuid> = refs.iter().map(|r| r.last_message_id).collect();
    let mut leads: HashMap<Uuid, LeadRow> =
        leads::lead_rows(&state.db, &lead_ids).await?.into_iter().map(|l| (l.lead.id, l)).collect();
    let mut messages: HashMap<Uuid, EmailMessage> = sqlx::query_as::<_, EmailMessage>("select * from email_messages where id = any($1)")
        .bind(&message_ids)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|m| (m.id, m))
        .collect();

    let items = refs
        .into_iter()
        .filter_map(|r| {
            Some(InboxThread {
                lead: leads.remove(&r.lead_id)?,
                last_message: messages.remove(&r.last_message_id)?,
                pending_auto_reply: r.pending_auto_reply,
                classification: r.classification,
                project_name: r.project_name,
            })
        })
        .collect();
    Ok(Json(Paginated { items, total }))
}

#[derive(Serialize)]
struct Thread {
    messages: Vec<EmailMessage>,
    lead: LeadDetail,
}

async fn thread(State(state): State<AppState>, auth: Auth, Path(lead_id): Path<Uuid>) -> AppResult<Json<Thread>> {
    let lead: Lead = util_scope::load(&state.db, "leads", lead_id, auth.org_id).await?;
    let lead = leads::detail(&state.db, lead).await?;
    Ok(Json(Thread { messages: lead.emails.clone(), lead }))
}

#[derive(Deserialize)]
struct ReplyBody {
    body: String,
}

async fn reply(
    State(state): State<AppState>,
    auth: Auth,
    Path(lead_id): Path<Uuid>,
    Json(body): Json<ReplyBody>,
) -> AppResult<Json<EmailMessage>> {
    let lead: Lead = util_scope::load(&state.db, "leads", lead_id, auth.org_id).await?;
    let text = body.body.trim();
    if text.is_empty() {
        return Err(AppError::BadRequest("Write a reply first".into()));
    }
    let inbound = sqlx::query_as::<_, EmailMessage>(
        "select * from email_messages where lead_id = $1 and direction = 'inbound' order by created_at desc limit 1",
    )
    .bind(lead.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::BadRequest("This lead hasn't replied yet".into()))?;

    let subject = if inbound.subject.to_lowercase().starts_with("re:") {
        inbound.subject.clone()
    } else {
        format!("Re: {}", inbound.subject)
    };
    let to_email = inbound.from_email.clone().or(lead.email.clone());
    let message = sqlx::query_as::<_, EmailMessage>(
        "insert into email_messages
           (project_id, lead_id, segment_id, mailbox_id, direction, kind, subject, body, status, scheduled_for, in_reply_to, to_email)
         values ($1, $2, $3, $4, 'outbound', 'manual_reply', $5, $6, 'scheduled', now(), $7, $8)
         returning *",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(lead.segment_id)
    .bind(inbound.mailbox_id)
    .bind(subject)
    .bind(text)
    .bind(inbound.message_id)
    .bind(to_email)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(message))
}

#[derive(Deserialize, Default)]
struct ApproveBody {
    body: Option<String>,
}

async fn approve(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    body: Option<Json<ApproveBody>>,
) -> AppResult<Json<EmailMessage>> {
    let message: EmailMessage = util_scope::load(&state.db, "email_messages", id, auth.org_id).await?;
    if message.kind != "auto_reply" || message.status != "draft" {
        return Err(AppError::BadRequest("Only pending auto-replies can be approved".into()));
    }
    let new_body = body.unwrap_or_default().0.body.filter(|b| !b.trim().is_empty());
    let message = sqlx::query_as::<_, EmailMessage>(
        "update email_messages set status = 'scheduled', scheduled_for = now(), body = coalesce($2, body)
         where id = $1 returning *",
    )
    .bind(message.id)
    .bind(new_body)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(message))
}

#[derive(Deserialize)]
struct StatusBody {
    status: String,
}

async fn set_status(
    State(state): State<AppState>,
    auth: Auth,
    Path(lead_id): Path<Uuid>,
    Json(body): Json<StatusBody>,
) -> AppResult<Json<Value>> {
    let lead: Lead = util_scope::load(&state.db, "leads", lead_id, auth.org_id).await?;
    util_input::one_of("status", &body.status, LEAD_STATUSES)?;
    sqlx::query("update leads set status = $2 where id = $1").bind(lead.id).bind(&body.status).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

/// For a reply that needs no answer. It stays in the thread and leaves Needs reply until the lead writes again.
async fn mark_done(State(state): State<AppState>, auth: Auth, Path(lead_id): Path<Uuid>) -> AppResult<Json<Value>> {
    let lead: Lead = util_scope::load(&state.db, "leads", lead_id, auth.org_id).await?;
    sqlx::query("update email_messages set handled_at = now() where lead_id = $1 and direction = 'inbound' and handled_at is null")
        .bind(lead.id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::THREADS;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn needs_reply_skips_handled_replies(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        let mut leads = vec![];
        // None is a reply the classifier hasn't read yet. The last two are questions you marked by hand.
        for (classification, status) in [
            (None, "replied"),
            (Some("question"), "replied"),
            (Some("not_interested"), "not_interested"),
            (Some("ooo"), "replied"),
            (Some("unsubscribe"), "unsubscribed"),
            (Some("bounce"), "bounced"),
            (Some("question"), "not_interested"),
            (Some("question"), "meeting_booked"),
            (Some("question"), "replied"),
            (Some("question"), "replied"),
        ] {
            let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, status) values ($1, $2) returning id")
                .bind(project)
                .bind(status)
                .fetch_one(&db)
                .await
                .unwrap();
            sqlx::query(
                "insert into email_messages (project_id, lead_id, direction, kind, status, sent_at, created_at)
                 values ($1, $2, 'outbound', 'initial', 'sent', now() - interval '1 day', now() - interval '1 day')",
            )
            .bind(project)
            .bind(lead)
            .execute(&db)
            .await
            .unwrap();
            sqlx::query(
                "insert into email_messages (project_id, lead_id, direction, kind, status, classification)
                 values ($1, $2, 'inbound', 'inbound', 'received', $3)",
            )
            .bind(project)
            .bind(lead)
            .bind(classification)
            .execute(&db)
            .await
            .unwrap();
            leads.push(lead);
        }

        // In approve mode the question's auto-reply waits as a draft until you send it.
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'outbound', 'auto_reply', 'draft')")
            .bind(project)
            .bind(leads[1])
            .execute(&db)
            .await
            .unwrap();

        // The first reply was marked done. The second lead wrote again after that, which brings the thread back.
        sqlx::query("update email_messages set handled_at = now() - interval '1 hour' where lead_id = any($1) and direction = 'inbound'")
            .bind(&leads[8..])
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'inbound', 'inbound', 'received')")
            .bind(project)
            .bind(leads[9])
            .execute(&db)
            .await
            .unwrap();

        let threads = |filter: &'static str| {
            let db = db.clone();
            async move {
                let mut ids: Vec<Uuid> = sqlx::query_scalar(&format!("select lead_id from ({THREADS}) x"))
                    .bind(org)
                    .bind(None::<Uuid>)
                    .bind(filter)
                    .fetch_all(&db)
                    .await
                    .unwrap();
                ids.sort();
                ids
            }
        };
        let sorted = |mut ids: Vec<Uuid>| {
            ids.sort();
            ids
        };
        assert_eq!(threads("needs_reply").await, sorted(vec![leads[0], leads[1], leads[9]]));
        assert_eq!(threads("other").await, sorted([&leads[2..7], &leads[8..9]].concat()));
        assert_eq!(threads("hot").await, vec![leads[7]]);
        assert_eq!(super::needs_reply_count(&db, org).await.unwrap(), 3);
    }
}
