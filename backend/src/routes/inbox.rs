use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Datelike, Duration, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

use super::attachments as attachments_routes;
use super::leads::{self, LeadDetail, LeadRow};
use super::util_input::{self, LEAD_STATUSES};
use super::util_pagination::{PageQuery, Paginated};
use super::util_scope;
use crate::agent::{self, kind};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::mail::attachments;
use crate::models::{EmailAttachment, EmailMessage, Lead, Mailbox, Ts};
use crate::ownership::{self, OwnerFilter};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/inbox", get(list))
        .route("/inbox/threads/{lead_id}", get(thread))
        .route("/inbox/threads/{lead_id}/reply", post(reply))
        .route("/inbox/threads/{lead_id}/draft", post(draft))
        .route("/inbox/threads/{lead_id}/status", post(set_status))
        .route("/inbox/threads/{lead_id}/done", post(mark_done))
        .route("/inbox/threads/{lead_id}/remind", post(remind))
        .route("/inbox/messages/{id}/approve", post(approve))
}

/// The most people a reply copies in.
pub(crate) const MAX_CC: usize = 20;

#[derive(Deserialize)]
pub(super) struct ListQuery {
    pub project_id: Option<Uuid>,
    /// Only the threads of this campaign's leads.
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    pub segment_id: Option<Uuid>,
    pub filter: Option<String>,
    pub q: Option<String>,
    /// Mine, unassigned, mine_or_unassigned or a member's id. See ownership::OwnerFilter.
    pub owner: Option<String>,
}

#[derive(Serialize)]
pub(super) struct InboxThread {
    pub lead: LeadRow,
    pub last_message: EmailMessage,
    pub pending_auto_reply: bool,
    pub classification: Option<String>,
    pub project_name: String,
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
// last message, so a pending auto-reply doesn't hide that the lead is waiting for an answer. $5 and $6 are an OwnerFilter, $7 a
// campaign.
fn threads() -> String {
    format!("with t as (
       select l.id as lead_id, l.status as lead_status, l.remind_at, p.name as project_name,
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
       where p.org_id = $1 and ($2::uuid is null or l.project_id = $2) and ($7::uuid is null or l.segment_id = $7)
         -- The reply join above already drops leads without one. This lets Postgres start from the few leads that
         -- replied instead of looking up the last message of every lead in the org: 490 ms to 30 ms at 50k leads.
         and l.id in (select i.lead_id from email_messages i where i.direction = 'inbound')
         and ($4::text is null or l.full_name ilike $4 or l.email::text ilike $4 or l.company_name ilike $4)
         and {}
     ), f as (
       select *,
         lead_status in ('hot', 'meeting_booked') as is_hot,
         -- Declines, out-of-office notes, unsubscribes and bounces are answered by the classifier, not by you.
         -- A pending auto-reply draft still waits on you to approve it, so it counts. Marking the lead as booked or
         -- not interested, or marking the thread done, is how you clear a reply you handled outside Splee.
         -- A reminder you set brings the thread back whoever wrote last and whatever the lead's status.
         (last_direction = 'inbound' and not handled
           and coalesce(classification, '') not in ('not_interested', 'ooo', 'unsubscribe', 'bounce')
           and lead_status not in ('meeting_booked', 'not_interested', 'bounced', 'unsubscribed', 'do_not_contact'))
           or coalesce(remind_at <= now(), false) as is_needs_reply
       from t
     )
     select * from f where case $3
       when 'hot' then is_hot
       when 'needs_reply' then is_needs_reply
       when 'auto_replied' then auto_replied
       when 'other' then not (is_hot or is_needs_reply or auto_replied)
       else true end", ownership::sql("l", 5, 6))
}

/// Threads in the Needs reply filter, across the organization or in one project, for the digest emails and weekly chat reports.
pub(crate) async fn needs_reply_count(db: &sqlx::PgPool, org_id: Uuid, project_id: Option<Uuid>) -> sqlx::Result<i64> {
    sqlx::query_scalar(&format!("select count(*) from ({}) x", threads()))
        .bind(org_id)
        .bind(project_id)
        .bind("needs_reply")
        .bind(None::<String>)
        .bind(OwnerFilter::Everyone.mode())
        .bind(None::<Uuid>)
        .bind(None::<Uuid>)
        .fetch_one(db)
        .await
}

async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<InboxThread>>> {
    Ok(Json(page_of(&state.db, auth.org_id, Some(auth.user_id), q, &page).await?))
}

/// A page of the organization's reply threads, the latest message first. `project_id` is matched against the org in the query.
/// `me` is who "mine" means, None for an API key.
pub(super) async fn page_of(db: &sqlx::PgPool, org_id: Uuid, me: Option<Uuid>, q: ListQuery, page: &PageQuery) -> AppResult<Paginated<InboxThread>> {
    let filter = q.filter.unwrap_or_else(|| "all".into());
    util_input::one_of("filter", &filter, &["all", "hot", "needs_reply", "auto_replied", "other"])?;
    let like = util_input::like_pattern(q.q.as_deref());
    let owner = OwnerFilter::parse(q.owner.as_deref(), me)?;

    let total: i64 = sqlx::query_scalar(&format!("select count(*) from ({}) x", threads()))
        .bind(org_id)
        .bind(q.project_id)
        .bind(&filter)
        .bind(&like)
        .bind(owner.mode())
        .bind(owner.user())
        .bind(q.segment_id)
        .fetch_one(db)
        .await?;
    let refs = sqlx::query_as::<_, ThreadRef>(&format!("{} order by last_at desc limit $8 offset $9", threads()))
        .bind(org_id)
        .bind(q.project_id)
        .bind(&filter)
        .bind(&like)
        .bind(owner.mode())
        .bind(owner.user())
        .bind(q.segment_id)
        .bind(page.limit())
        .bind(page.offset())
        .fetch_all(db)
        .await?;

    let lead_ids: Vec<Uuid> = refs.iter().map(|r| r.lead_id).collect();
    let message_ids: Vec<Uuid> = refs.iter().map(|r| r.last_message_id).collect();
    let mut leads: HashMap<Uuid, LeadRow> =
        leads::lead_rows(db, &lead_ids).await?.into_iter().map(|l| (l.lead.id, l)).collect();
    let mut messages: HashMap<Uuid, EmailMessage> = sqlx::query_as::<_, EmailMessage>("select * from email_messages where id = any($1)")
        .bind(&message_ids)
        .fetch_all(db)
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
    Ok(Paginated { items, total })
}

#[derive(Serialize)]
struct Thread {
    messages: Vec<ThreadMessage>,
    lead: LeadDetail,
    /// The agent run writing a reply draft right now, so a thread opened mid-run can follow it.
    draft_run: Option<Uuid>,
    /// Who a reply copies in unless you change it. See reply_all_cc.
    reply_cc: Vec<String>,
    /// The time Send later offers first.
    send_later: SendLater,
}

/// 08:00 on the next weekday in the lead's time zone, from their number or country, else in the project's, the one sequence
/// emails keep to then too.
#[derive(Serialize)]
struct SendLater {
    at: Ts,
    time_zone: String,
    /// The time zone is the lead's.
    theirs: bool,
}

const SEND_LATER_HOUR: u32 = 8;
/// How far ahead a reply can wait. Further out, what it answers has gone stale.
const SEND_LATER_DAYS: i64 = 30;

/// The first weekday SEND_LATER_HOUR in `tz` after `now`.
fn next_morning(tz: Tz, now: Ts) -> Ts {
    let today = now.with_timezone(&tz).date_naive();
    (0..10)
        .map(|d| today + Duration::days(d))
        .filter(|day| !matches!(day.weekday(), Weekday::Sat | Weekday::Sun))
        .filter_map(|day| tz.from_local_datetime(&day.and_hms_opt(SEND_LATER_HOUR, 0, 0)?).earliest())
        .map(|at| at.with_timezone(&Utc))
        .find(|at| *at > now)
        .expect("a weekday morning comes within 10 days")
}

async fn send_later(db: &sqlx::PgPool, lead: &Lead) -> sqlx::Result<SendLater> {
    let (tz, theirs) = match crate::phone::time_zone(&lead.phone, &lead.country_code) {
        Some(tz) => (tz, true),
        None => {
            let project: Option<String> =
                sqlx::query_scalar("select timezone from project_settings where project_id = $1").bind(lead.project_id).fetch_optional(db).await?;
            (project.and_then(|z| z.parse().ok()).unwrap_or(Tz::UTC), false)
        }
    };
    Ok(SendLater { at: next_morning(tz, Utc::now()), time_zone: tz.name().to_string(), theirs })
}

/// An email with its files, without their bytes.
#[derive(Serialize)]
pub(crate) struct ThreadMessage {
    #[serde(flatten)]
    pub message: EmailMessage,
    pub attachments: Vec<EmailAttachment>,
}

pub(crate) async fn with_attachments(db: &sqlx::PgPool, messages: Vec<EmailMessage>) -> sqlx::Result<Vec<ThreadMessage>> {
    let mut files = attachments::for_messages(db, &messages.iter().map(|m| m.id).collect::<Vec<_>>()).await?;
    Ok(messages.into_iter().map(|m| ThreadMessage { attachments: files.remove(&m.id).unwrap_or_default(), message: m }).collect())
}

async fn thread(State(state): State<AppState>, auth: Auth, Path(lead_id): Path<Uuid>) -> AppResult<Json<Thread>> {
    let lead: Lead = util_scope::load(&state.db, "leads", lead_id, auth.org_id).await?;
    let draft_run = active_draft_run(&state.db, lead.id).await?;
    let reply_cc = match last_inbound(&state.db, lead.id).await? {
        Some(inbound) => reply_all_cc(&state.db, auth.org_id, &lead, &inbound).await?,
        None => vec![],
    };
    let send_later = send_later(&state.db, &lead).await?;
    let lead = leads::detail(&state.db, lead).await?;
    let messages = with_attachments(&state.db, lead.emails.clone()).await?;
    Ok(Json(Thread { messages, lead, draft_run, reply_cc, send_later }))
}

async fn last_inbound(db: &sqlx::PgPool, lead_id: Uuid) -> sqlx::Result<Option<EmailMessage>> {
    sqlx::query_as::<_, EmailMessage>("select * from email_messages where lead_id = $1 and direction = 'inbound' order by created_at desc limit 1")
        .bind(lead_id)
        .fetch_optional(db)
        .await
}

/// Reply all: everyone the lead copied on `inbound`, except the lead, the org's own mailboxes and anyone on the
/// do-not-contact list.
pub(crate) async fn reply_all_cc(db: &sqlx::PgPool, org_id: Uuid, lead: &Lead, inbound: &EmailMessage) -> sqlx::Result<Vec<String>> {
    let ours: Vec<String> = sqlx::query_scalar("select lower(email::text) from mailboxes where org_id = $1").bind(org_id).fetch_all(db).await?;
    let lead_addresses = [lead.email.as_deref(), inbound.from_email.as_deref()].map(|e| e.map(str::to_lowercase));
    let copied: Vec<String> = inbound
        .cc_emails
        .iter()
        .filter(|c| !ours.contains(c) && !lead_addresses.iter().flatten().any(|l| l == *c))
        .take(MAX_CC)
        .cloned()
        .collect();
    let suppressed = crate::mail::suppressed_among(db, org_id, lead.project_id, &copied).await?;
    Ok(copied.into_iter().filter(|c| !suppressed.contains(c)).collect())
}

/// The CC list someone typed: real addresses, at most MAX_CC, none on the do-not-contact list. `to` is dropped, since the
/// lead already gets it.
async fn check_cc(db: &sqlx::PgPool, org_id: Uuid, project_id: Uuid, list: &[String], to: Option<&str>) -> AppResult<Vec<String>> {
    if list.len() > MAX_CC {
        return Err(AppError::BadRequest(format!("Copy in at most {MAX_CC} people")));
    }
    let mut cc: Vec<String> = vec![];
    for raw in list {
        let email = util_input::email(raw).ok_or_else(|| AppError::BadRequest(format!("{} isn't an email address", raw.trim())))?;
        if !to.is_some_and(|t| t.eq_ignore_ascii_case(&email)) && !cc.contains(&email) {
            cc.push(email);
        }
    }
    if let Some(first) = crate::mail::suppressed_among(db, org_id, project_id, &cc).await?.first() {
        return Err(AppError::BadRequest(format!("{first} is on your do-not-contact list, so Splee won't copy them in")));
    }
    Ok(cc)
}

async fn mailbox(db: &sqlx::PgPool, id: Option<Uuid>) -> sqlx::Result<Option<Mailbox>> {
    let Some(id) = id else { return Ok(None) };
    sqlx::query_as::<_, Mailbox>("select * from mailboxes where id = $1").bind(id).fetch_optional(db).await
}

/// A queued or running reply draft for any of the lead's messages, whether someone asked for it or a reply triggered it.
async fn active_draft_run<'e, E: sqlx::PgExecutor<'e>>(db: E, lead_id: Uuid) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar(
        "select r.id from agent_runs r join email_messages m on m.id = (r.input ->> 'message_id')::uuid
         where r.kind = $2 and r.status in ('queued', 'running') and m.lead_id = $1
         order by r.created_at desc limit 1",
    )
    .bind(lead_id)
    .bind(kind::DRAFT_REPLY)
    .fetch_optional(db)
    .await
}

#[derive(Deserialize)]
struct ReplyBody {
    body: String,
    #[serde(flatten)]
    extras: ReplyExtras,
}

/// Who a reply copies in and the files it takes. cc_emails, when sent, is the whole list. Without it, reply_all (the
/// default) copies everyone the lead copied in last. attachment_ids are uploads from POST /attachments.
#[derive(Deserialize, Default)]
pub(crate) struct ReplyExtras {
    pub cc_emails: Option<Vec<String>>,
    pub reply_all: Option<bool>,
    #[serde(default)]
    pub attachment_ids: Vec<Uuid>,
    /// Sends it then instead of right away, within SEND_LATER_DAYS. Until then it can be cancelled like any scheduled email.
    pub send_at: Option<Ts>,
}

async fn reply(
    State(state): State<AppState>,
    auth: Auth,
    Path(lead_id): Path<Uuid>,
    Json(body): Json<ReplyBody>,
) -> AppResult<Json<EmailMessage>> {
    Ok(Json(send_reply(&state.db, auth.org_id, lead_id, &body.body, &body.extras).await?))
}

/// Queues a reply written by a person to the lead's last message. The inbox's Send, the API's and Slack's Edit all end up here.
pub(crate) async fn send_reply(db: &sqlx::PgPool, org_id: Uuid, lead_id: Uuid, text: &str, extras: &ReplyExtras) -> AppResult<EmailMessage> {
    let lead: Lead = util_scope::load(db, "leads", lead_id, org_id).await?;
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::BadRequest("Write a reply first".into()));
    }
    let now = Utc::now();
    if extras.send_at.is_some_and(|at| at <= now || at > now + Duration::days(SEND_LATER_DAYS)) {
        return Err(AppError::BadRequest(format!("Pick a time to send it in the next {SEND_LATER_DAYS} days")));
    }
    let inbound = last_inbound(db, lead.id).await?.ok_or_else(|| AppError::BadRequest("This lead hasn't replied yet".into()))?;

    let subject = if inbound.subject.to_lowercase().starts_with("re:") {
        inbound.subject.clone()
    } else {
        format!("Re: {}", inbound.subject)
    };
    let to_email = inbound.from_email.clone().or(lead.email.clone());
    let cc = match &extras.cc_emails {
        Some(list) => check_cc(db, org_id, lead.project_id, list, to_email.as_deref()).await?,
        None if extras.reply_all.unwrap_or(true) => reply_all_cc(db, org_id, &lead, &inbound).await?,
        None => vec![],
    };
    let sending_mailbox = mailbox(db, inbound.mailbox_id).await?;
    let mut tx = db.begin().await?;
    // Takes turns with a reply draft being saved, so the draft either lands first and is cancelled below or sees this reply.
    sqlx::query("select 1 from leads where id = $1 for update").bind(lead.id).execute(&mut *tx).await?;
    let message = sqlx::query_as::<_, EmailMessage>(
        "insert into email_messages
           (project_id, lead_id, segment_id, mailbox_id, direction, kind, subject, body, status, scheduled_for, in_reply_to, to_email, cc_emails)
         values ($1, $2, $3, $4, 'outbound', 'manual_reply', $5, $6, 'scheduled', coalesce($10, now()), $7, $8, $9)
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
    .bind(&cc)
    .bind(extras.send_at)
    .fetch_one(&mut *tx)
    .await?;
    // In the same transaction, so the sender never sees the reply without its files.
    attachments_routes::take(&mut tx, org_id, message.id, &extras.attachment_ids, sending_mailbox.as_ref()).await?;
    // You answered yourself, so an agent draft still waiting would be a second answer if someone approved it later.
    sqlx::query("update email_messages set status = 'cancelled' where lead_id = $1 and kind = 'auto_reply' and status = 'draft'")
        .bind(lead.id)
        .execute(&mut *tx)
        .await?;
    // Following up is what a due reminder asked for. One still ahead stays, to check back if they don't answer.
    sqlx::query("update leads set remind_at = null where id = $1 and remind_at <= now()").bind(lead.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(message)
}

/// An edit to a pending auto-reply: its text, who it copies in in place of the lead's CC list, and files to add.
#[derive(Deserialize, Default)]
pub(crate) struct ApproveBody {
    pub body: Option<String>,
    pub cc_emails: Option<Vec<String>>,
    #[serde(default)]
    pub attachment_ids: Vec<Uuid>,
}

/// Has the agent draft an answer to the lead's last reply, as auto-replies do. The draft waits for approval in the thread.
async fn draft(State(state): State<AppState>, auth: Auth, Path(lead_id): Path<Uuid>) -> AppResult<Json<Value>> {
    let lead: Lead = util_scope::load(&state.db, "leads", lead_id, auth.org_id).await?;
    let (inbound, channel): (Uuid, String) = sqlx::query_as(
        "select id, channel from email_messages where lead_id = $1 and direction = 'inbound' and classification is distinct from 'bounce'
         order by created_at desc limit 1",
    )
    .bind(lead.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::BadRequest("This lead hasn't replied yet".into()))?;
    if channel != "email" {
        return Err(AppError::BadRequest("Splee drafts email replies, and they last wrote by text. Answer them by text instead".into()));
    }
    // Locking the lead makes two quick requests take turns, so the second sees the first one's run.
    let mut tx = state.db.begin().await?;
    sqlx::query("select 1 from leads where id = $1 for update").bind(lead.id).execute(&mut *tx).await?;
    let pending: bool = sqlx::query_scalar("select exists (select 1 from email_messages where lead_id = $1 and kind = 'auto_reply' and status = 'draft')")
        .bind(lead.id)
        .fetch_one(&mut *tx)
        .await?;
    if pending || active_draft_run(&mut *tx, lead.id).await?.is_some() {
        return Err(AppError::Conflict("A reply draft is already on its way or waiting in this thread".into()));
    }
    let run_id =
        agent::enqueue(&mut *tx, kind::DRAFT_REPLY, Some(lead.project_id), Some(auth.org_id), json!({ "message_id": inbound, "manual": true })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "run_id": run_id })))
}

async fn approve(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    body: Option<Json<ApproveBody>>,
) -> AppResult<Json<EmailMessage>> {
    Ok(Json(approve_reply(&state.db, auth.org_id, id, body.unwrap_or_default().0).await?))
}

/// Sends a pending auto-reply, with the edit's text, CC list and files when it has them. The inbox's Approve and Slack's
/// Send reply both end up here.
pub(crate) async fn approve_reply(db: &sqlx::PgPool, org_id: Uuid, id: Uuid, edit: ApproveBody) -> AppResult<EmailMessage> {
    let message: EmailMessage = util_scope::load(db, "email_messages", id, org_id).await?;
    if message.kind != "auto_reply" || message.status != "draft" {
        return Err(AppError::BadRequest("Only pending auto-replies can be approved".into()));
    }
    let new_body = edit.body.filter(|b| !b.trim().is_empty());
    let cc = match &edit.cc_emails {
        Some(list) => Some(check_cc(db, org_id, message.project_id, list, message.to_email.as_deref()).await?),
        None => None,
    };
    let sending_mailbox = mailbox(db, message.mailbox_id).await?;
    let mut tx = db.begin().await?;
    // Only a draft moves, so a second approval can't put a reply the sender already took back in the queue.
    let approved = sqlx::query_as::<_, EmailMessage>(
        "update email_messages set status = 'scheduled', scheduled_for = now(), body = coalesce($2, body), cc_emails = coalesce($3, cc_emails)
         where id = $1 and status = 'draft' returning *",
    )
    .bind(message.id)
    .bind(new_body)
    .bind(cc)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::Conflict("This reply was already sent".into()))?;
    attachments_routes::take(&mut tx, org_id, approved.id, &edit.attachment_ids, sending_mailbox.as_ref()).await?;
    tx.commit().await?;
    Ok(approved)
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
    let updated = sqlx::query_as::<_, Lead>("update leads set status = $2 where id = $1 returning *")
        .bind(lead.id)
        .bind(&body.status)
        .fetch_one(&state.db)
        .await?;
    crate::hooks::team_set_status(&state, &lead.status, &updated).await;
    Ok(Json(json!({ "ok": true })))
}

/// For a reply that needs no answer. It stays in the thread and leaves Needs reply until the lead writes again.
async fn mark_done(State(state): State<AppState>, auth: Auth, Path(lead_id): Path<Uuid>) -> AppResult<Json<Value>> {
    mark_replied(&state.db, auth.org_id, lead_id).await?;
    Ok(Json(json!({ "ok": true })))
}

/// The inbox's Done, and POST /v1/leads/{id}/mark-replied for a reply answered outside Splee. It clears the reminder and cancels
/// the agent's reply draft still waiting, which would be a second answer. Returns when the latest reply it covers came in, and is
/// a 409 for a lead who never replied.
pub(crate) async fn mark_replied(db: &sqlx::PgPool, org_id: Uuid, lead_id: Uuid) -> AppResult<Ts> {
    let lead: Lead = util_scope::load(db, "leads", lead_id, org_id).await?;
    let mut tx = db.begin().await?;
    // Takes turns with a reply draft being saved, as in send_reply.
    sqlx::query("select 1 from leads where id = $1 for update").bind(lead.id).execute(&mut *tx).await?;
    let through: Option<Ts> = sqlx::query_scalar("select max(created_at) from email_messages where lead_id = $1 and direction = 'inbound'")
        .bind(lead.id)
        .fetch_one(&mut *tx)
        .await?;
    let through = through.ok_or_else(|| AppError::Conflict("This lead hasn't replied, so there's nothing to mark".into()))?;
    sqlx::query("update email_messages set handled_at = now() where lead_id = $1 and direction = 'inbound' and handled_at is null")
        .bind(lead.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update email_messages set status = 'cancelled' where lead_id = $1 and kind = 'auto_reply' and status = 'draft'")
        .bind(lead.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update leads set remind_at = null where id = $1").bind(lead.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(through)
}

#[derive(Deserialize)]
struct RemindBody {
    days: Option<i32>,
    /// An exact time, for a reminder picked on a calendar. Takes the place of `days`.
    at: Option<Ts>,
}

/// Takes the thread out of Needs reply like Done, and brings it back after `days` or at `at`. Neither clears the reminder.
async fn remind(State(state): State<AppState>, auth: Auth, Path(lead_id): Path<Uuid>, Json(body): Json<RemindBody>) -> AppResult<Json<Value>> {
    let lead: Lead = util_scope::load(&state.db, "leads", lead_id, auth.org_id).await?;
    if body.days.is_some_and(|d| !(1..=30).contains(&d)) {
        return Err(AppError::BadRequest("Pick a reminder between 1 and 30 days away".into()));
    }
    if body.at.is_some_and(|at| at <= Utc::now() || at > Utc::now() + Duration::days(365)) {
        return Err(AppError::BadRequest("Pick a reminder time in the next year".into()));
    }
    let at = body.at.or(body.days.map(|d| Utc::now() + Duration::days(d.into())));
    set_reminder(&state.db, lead.id, at).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Brings the thread back at `at`, taking it out of Needs reply until then, or clears the reminder. Slack's Snooze uses it too.
pub(crate) async fn set_reminder(db: &sqlx::PgPool, lead_id: Uuid, at: Option<Ts>) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("update leads set remind_at = $2 where id = $1").bind(lead_id).bind(at).execute(&mut *tx).await?;
    if at.is_some() {
        sqlx::query("update email_messages set handled_at = now() where lead_id = $1 and direction = 'inbound' and handled_at is null")
            .bind(lead_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    use chrono::{Duration, Utc};
    use chrono_tz::Tz;

    use super::{ApproveBody, ReplyExtras, approve_reply, next_morning, reply_all_cc, send_later, send_reply, threads};
    use crate::error::AppError;
    use crate::models::{EmailMessage, Lead};

    fn refused(result: Result<EmailMessage, AppError>) -> String {
        match result {
            Err(AppError::BadRequest(why)) => why,
            other => panic!("expected a 400, got {:?}", other.map(|m| m.id)),
        }
    }

    #[test]
    fn send_later_offers_the_next_weekday_morning() {
        let at = |s: &str| s.parse::<crate::models::Ts>().unwrap();
        let london: Tz = "Europe/London".parse().unwrap();
        // Wednesday 25 November 2026: before 8:00 it's that morning, after it the next one. Friday afternoon waits for Monday.
        assert_eq!(next_morning(london, at("2026-11-25T07:30:00Z")), at("2026-11-25T08:00:00Z"));
        assert_eq!(next_morning(london, at("2026-11-25T08:00:00Z")), at("2026-11-26T08:00:00Z"));
        assert_eq!(next_morning(london, at("2026-11-27T15:00:00Z")), at("2026-11-30T08:00:00Z"));
        assert_eq!(next_morning(london, at("2026-11-28T09:00:00Z")), at("2026-11-30T08:00:00Z"));
        // In New York it's 8:00 there, 13:00 UTC in winter, and summer time moves it.
        let new_york: Tz = "America/New_York".parse().unwrap();
        assert_eq!(next_morning(new_york, at("2026-11-25T12:00:00Z")), at("2026-11-25T13:00:00Z"));
        assert_eq!(next_morning(new_york, at("2026-07-01T12:30:00Z")), at("2026-07-02T12:00:00Z"));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_reply_can_wait_until_a_time_within_30_days(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        sqlx::query("insert into project_settings (project_id, timezone) values ($1, 'America/Chicago')").bind(project).execute(&db).await.unwrap();
        let lead = |email: &'static str, phone: &'static str| {
            sqlx::query_as::<_, Lead>("insert into leads (project_id, email, phone, status) values ($1, $2, $3, 'replied') returning *")
                .bind(project)
                .bind(email)
                .bind(phone)
                .fetch_one(&db)
        };
        let (sam, kim) = (lead("sam@beta.test", "+447700900123").await.unwrap(), lead("kim@beta.test", "").await.unwrap());
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, status, subject, message_id, from_email)
             values ($1, $2, 'inbound', 'inbound', 'received', 'Pricing?', 'r1@beta.test', 'sam@beta.test')",
        )
        .bind(project)
        .bind(sam.id)
        .execute(&db)
        .await
        .unwrap();

        // Send later offers 8:00 their time from Sam's UK number, and the project's time zone for Kim, who has no number.
        let offered = send_later(&db, &sam).await.unwrap();
        assert_eq!((offered.time_zone.as_str(), offered.theirs), ("Europe/London", true));
        assert_eq!(offered.at, next_morning("Europe/London".parse().unwrap(), Utc::now()));
        let offered = send_later(&db, &kim).await.unwrap();
        assert_eq!((offered.time_zone.as_str(), offered.theirs), ("America/Chicago", false));

        let reply = |send_at: Option<crate::models::Ts>| {
            let db = db.clone();
            async move { send_reply(&db, org, sam.id, "Here's the pricing.", &ReplyExtras { send_at, ..Default::default() }).await }
        };
        let at = Utc::now() + Duration::days(2);
        let later = reply(Some(at)).await.unwrap();
        assert_eq!((later.status.as_str(), later.scheduled_for.map(|s| s.timestamp_micros())), ("scheduled", Some(at.timestamp_micros())));
        let now = reply(None).await.unwrap();
        assert!(now.scheduled_for.unwrap() <= Utc::now());
        assert!(refused(reply(Some(Utc::now() - Duration::minutes(1))).await).contains("in the next 30 days"));
        assert!(refused(reply(Some(Utc::now() + Duration::days(31))).await).contains("in the next 30 days"));

        // Cancelled before it goes, it stays. Once the sender has taken a reply, it can't be cancelled.
        let cancel = |id: Uuid| {
            let body = crate::routes::emails::UpdateBody { status: Some("cancelled".into()), ..Default::default() };
            crate::routes::emails::update_one(&db, org, id, body)
        };
        assert_eq!(cancel(later.id).await.unwrap().email.status, "cancelled");
        sqlx::query("update email_messages set status = 'sending' where id = $1").bind(now.id).execute(&db).await.unwrap();
        assert!(cancel(now.id).await.is_err());
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_reply_copies_in_the_leads_cc_list_and_takes_its_files(db: PgPool) {
        let new_org = || sqlx::query_scalar::<_, Uuid>("insert into organizations (name) values ('Test') returning id").fetch_one(&db);
        let (org, other) = (new_org().await.unwrap(), new_org().await.unwrap());
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        let mailbox: Uuid = sqlx::query_scalar(
            "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, 'jo@alpha.test', 'smtp.gmail.com', 'jo', 'x')
             returning id",
        )
        .bind(org)
        .fetch_one(&db)
        .await
        .unwrap();
        let lead = sqlx::query_as::<_, Lead>("insert into leads (project_id, email, status) values ($1, 'sam@beta.test', 'replied') returning *")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        let inbound = sqlx::query_as::<_, EmailMessage>(
            "insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status, subject, message_id, from_email, cc_emails)
             values ($1, $2, $3, 'inbound', 'inbound', 'received', 'Hello', 'r1@beta.test', 'sam@beta.test',
                     '{kim@beta.test,ops@beta.test,jo@alpha.test,sam@beta.test}')
             returning *",
        )
        .bind(project)
        .bind(lead.id)
        .bind(mailbox)
        .fetch_one(&db)
        .await
        .unwrap();
        for (value, kind) in [("ops@beta.test", "email"), ("blocked.test", "domain")] {
            sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, $2, $3, 'manual')")
                .bind(org)
                .bind(value)
                .bind(kind)
                .execute(&db)
                .await
                .unwrap();
        }
        let upload = |org: Uuid, size: i64| {
            sqlx::query_scalar::<_, Uuid>(
                "insert into email_attachments (org_id, filename, content_type, size_bytes) values ($1, 'brief.pdf', 'application/pdf', $2) returning id",
            )
            .bind(org)
            .bind(size)
            .fetch_one(&db)
        };
        let reply = |extras: ReplyExtras| {
            let db = db.clone();
            async move { send_reply(&db, org, lead.id, "Here it is.", &extras).await }
        };
        let cc = |list: &[&str]| Some(list.iter().map(|c| c.to_string()).collect::<Vec<_>>());

        // Reply all leaves out the lead, our own mailbox and the do-not-contact list.
        assert_eq!(reply_all_cc(&db, org, &lead, &inbound).await.unwrap(), ["kim@beta.test"]);
        let sent = reply(ReplyExtras::default()).await.unwrap();
        assert_eq!(sent.cc_emails, ["kim@beta.test"]);
        assert!(reply(ReplyExtras { reply_all: Some(false), ..Default::default() }).await.unwrap().cc_emails.is_empty());

        // A list someone typed is checked, and the lead's own address comes off it.
        let typed = reply(ReplyExtras { cc_emails: cc(&["Kim@Beta.test", "sam@beta.test", "lee@gamma.test"]), ..Default::default() }).await.unwrap();
        assert_eq!(typed.cc_emails, ["kim@beta.test", "lee@gamma.test"]);
        assert!(refused(reply(ReplyExtras { cc_emails: cc(&["ops@beta.test"]), ..Default::default() }).await).contains("do-not-contact"));
        assert!(refused(reply(ReplyExtras { cc_emails: cc(&["ana@blocked.test"]), ..Default::default() }).await).contains("do-not-contact"));
        assert!(refused(reply(ReplyExtras { cc_emails: cc(&["not an address"]), ..Default::default() }).await).contains("isn't an email address"));
        let crowd: Vec<String> = (0..21).map(|n| format!("p{n}@gamma.test")).collect();
        assert!(refused(reply(ReplyExtras { cc_emails: Some(crowd), ..Default::default() }).await).contains("at most 20"));

        // Files: the org's own unused uploads, at most 10, within what Gmail sends in one email.
        let files = vec![upload(org, 9).await.unwrap(), upload(org, 6).await.unwrap()];
        let with_files = reply(ReplyExtras { attachment_ids: files.clone(), ..Default::default() }).await.unwrap();
        let taken: Vec<Uuid> =
            sqlx::query_scalar("select id from email_attachments where message_id = $1 order by id").bind(with_files.id).fetch_all(&db).await.unwrap();
        let mut want = files.clone();
        want.sort();
        assert_eq!(taken, want);
        assert!(refused(reply(ReplyExtras { attachment_ids: vec![files[0]], ..Default::default() }).await).contains("gone or already sent"));
        let theirs = upload(other, 9).await.unwrap();
        assert!(refused(reply(ReplyExtras { attachment_ids: vec![theirs], ..Default::default() }).await).contains("gone or already sent"));
        let mut eleven = vec![];
        for _ in 0..11 {
            eleven.push(upload(org, 1).await.unwrap());
        }
        assert!(refused(reply(ReplyExtras { attachment_ids: eleven, ..Default::default() }).await).contains("at most 10 files"));
        let big = vec![upload(org, 10 * 1024 * 1024).await.unwrap(), upload(org, 10 * 1024 * 1024).await.unwrap()];
        assert!(refused(reply(ReplyExtras { attachment_ids: big.clone(), ..Default::default() }).await).contains("Gmail sends at most"));
        // A refused reply wasn't queued and left its uploads free.
        let unused: i64 = sqlx::query_scalar("select count(*) from email_attachments where id = any($1) and message_id is null")
            .bind(&big)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(unused, 2);

        // An auto-reply waiting for approval keeps its CC list and takes files when it's approved.
        let draft: Uuid = sqlx::query_scalar(
            "insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status, subject, body, to_email, cc_emails)
             values ($1, $2, $3, 'outbound', 'auto_reply', 'draft', 'Re: Hello', 'Thanks', 'sam@beta.test', '{kim@beta.test}') returning id",
        )
        .bind(project)
        .bind(lead.id)
        .bind(mailbox)
        .fetch_one(&db)
        .await
        .unwrap();
        let file = upload(org, 9).await.unwrap();
        let approved = approve_reply(&db, org, draft, ApproveBody { attachment_ids: vec![file], ..Default::default() }).await.unwrap();
        assert_eq!((approved.status.as_str(), approved.cc_emails), ("scheduled", vec!["kim@beta.test".to_string()]));
        let on: Option<Uuid> = sqlx::query_scalar("select message_id from email_attachments where id = $1").bind(file).fetch_one(&db).await.unwrap();
        assert_eq!(on, Some(draft));
    }

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
            let n = leads.len();
            let lead: Uuid = sqlx::query_scalar(
                "insert into leads (project_id, status, full_name, email, company_name) values ($1, $2, $3, $4, $5) returning id",
            )
            .bind(project)
            .bind(status)
            .bind(format!("Lead {n}"))
            .bind(format!("lead{n}@example.test"))
            .bind(format!("Company {n}"))
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

        let search = |filter: &'static str, q: Option<&'static str>| {
            let db = db.clone();
            async move {
                let mut ids: Vec<Uuid> = sqlx::query_scalar(&format!("select lead_id from ({}) x", threads()))
                    .bind(org)
                    .bind(None::<Uuid>)
                    .bind(filter)
                    .bind(super::util_input::like_pattern(q))
                    .bind("everyone")
                    .bind(None::<Uuid>)
                    .bind(None::<Uuid>)
                    .fetch_all(&db)
                    .await
                    .unwrap();
                ids.sort();
                ids
            }
        };
        let threads = |filter: &'static str| search(filter, None);
        let sorted = |mut ids: Vec<Uuid>| {
            ids.sort();
            ids
        };
        // A due reminder brings back a thread you'd otherwise never answer. One still ahead doesn't.
        sqlx::query("update leads set remind_at = now() - interval '1 minute' where id = $1").bind(leads[2]).execute(&db).await.unwrap();
        sqlx::query("update leads set remind_at = now() + interval '1 day' where id = $1").bind(leads[3]).execute(&db).await.unwrap();

        assert_eq!(threads("needs_reply").await, sorted(vec![leads[0], leads[1], leads[2], leads[9]]));
        assert_eq!(threads("other").await, sorted([&leads[3..7], &leads[8..9]].concat()));
        assert_eq!(threads("hot").await, vec![leads[7]]);
        assert_eq!(super::needs_reply_count(&db, org, None).await.unwrap(), 4);
        // Search matches the lead's name, email or company and keeps the filter.
        assert_eq!(search("all", Some("lead 3")).await, vec![leads[3]]);
        assert_eq!(search("all", Some("LEAD1@")).await, vec![leads[1]]);
        assert_eq!(search("needs_reply", Some("company 9")).await, vec![leads[9]]);
        assert!(search("needs_reply", Some("company 4")).await.is_empty());
    }
}
