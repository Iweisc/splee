use axum::Json;
use axum::extract::{Multipart, Path, Query, State};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::owner_by_email;
use crate::auth::ApiKeyAuth;
use crate::error::AppResult;
use crate::models::{EmailAttachment, EmailMessage, Lead, Ts};
use crate::routes::attachments;
use crate::routes::emails::{self, EmailRow};
use crate::routes::inbox::{self, InboxThread, ReplyExtras, ThreadMessage as WithFiles};
use crate::routes::util_pagination::{PageQuery, Paginated};
use crate::routes::util_scope;
use crate::state::AppState;

/// An outbound email with who it's for, without the sending details.
#[derive(Serialize)]
pub(in crate::routes) struct Email {
    id: Uuid,
    project_id: Uuid,
    segment_id: Option<Uuid>,
    campaign: Option<String>,
    /// The A/B test variant it was written for. Follow-ups keep the first email's.
    variant_id: Option<Uuid>,
    variant: Option<String>,
    lead_id: Uuid,
    lead_name: String,
    lead_email: Option<String>,
    company_name: String,
    kind: String,
    step: i32,
    subject: String,
    body: String,
    status: String,
    scheduled_for: Option<Ts>,
    created_at: Ts,
    /// "person" or "review_window" once approved.
    approved_via: Option<String>,
    /// When a draft goes out on its own if nobody reviews it.
    sends_at: Option<Ts>,
}

impl From<EmailRow> for Email {
    fn from(r: EmailRow) -> Self {
        let e = r.email;
        Self {
            id: e.id,
            project_id: e.project_id,
            segment_id: e.segment_id,
            campaign: r.segment_name,
            variant_id: e.variant_id,
            variant: r.variant_name,
            lead_id: e.lead_id,
            lead_name: r.lead_name,
            lead_email: r.lead_email,
            company_name: r.company_name,
            kind: e.kind,
            step: e.step,
            subject: e.subject,
            body: e.body,
            status: e.status,
            scheduled_for: e.scheduled_for,
            created_at: e.created_at,
            approved_via: e.approved_via,
            sends_at: r.sends_at,
        }
    }
}

/// Drafts waiting for approval by default, oldest first, so a reviewer goes through them in the order they were written.
pub(in crate::routes) async fn list_emails(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(mut q): Query<emails::ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Email>>> {
    q.status = q.status.filter(|s| !s.is_empty()).or(Some("draft".into()));
    q.order = Some("soonest".into());
    let Paginated { items, total } = emails::page_of(&state.db, auth.org_id, q, &page).await?;
    Ok(Json(Paginated { items: items.into_iter().map(Email::from).collect(), total }))
}

/// The app's Approve button. In a campaign that isn't sending yet, the email waits in the queue until it starts.
pub(in crate::routes) async fn approve_email(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Email>> {
    set_email_status(&state, auth, id, "approved").await
}

/// The app's Cancel button: the email never goes out.
pub(in crate::routes) async fn skip_email(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Email>> {
    set_email_status(&state, auth, id, "cancelled").await
}

async fn set_email_status(state: &AppState, auth: ApiKeyAuth, id: Uuid, status: &str) -> AppResult<Json<Email>> {
    let body = emails::UpdateBody { status: Some(status.into()), ..Default::default() };
    Ok(Json(emails::update_one(&state.db, auth.org_id, id, body).await?.into()))
}

/// A reply thread: the lead and the latest message either way, cut short.
#[derive(Serialize)]
pub(in crate::routes) struct Thread {
    lead_id: Uuid,
    project_id: Uuid,
    /// The lead's campaign.
    campaign_id: Option<Uuid>,
    project_name: String,
    lead_name: String,
    lead_email: Option<String>,
    company_name: String,
    lead_status: String,
    owner_id: Option<Uuid>,
    owner_email: Option<String>,
    classification: Option<String>,
    pending_auto_reply: bool,
    last_message: ThreadMessage,
}

#[derive(Serialize)]
struct ThreadMessage {
    id: Uuid,
    direction: String,
    subject: String,
    snippet: String,
    at: Ts,
}

const SNIPPET_CHARS: usize = 300;

impl From<InboxThread> for Thread {
    fn from(t: InboxThread) -> Self {
        let (owner_email, lead, m) = (t.lead.owner_email, t.lead.lead, t.last_message);
        Self {
            lead_id: lead.id,
            project_id: lead.project_id,
            campaign_id: lead.segment_id,
            project_name: t.project_name,
            lead_name: lead.full_name,
            lead_email: lead.email,
            company_name: lead.company_name,
            lead_status: lead.status,
            owner_id: lead.owner_id,
            owner_email,
            classification: t.classification,
            pending_auto_reply: t.pending_auto_reply,
            last_message: ThreadMessage {
                id: m.id,
                direction: m.direction,
                subject: m.subject,
                snippet: m.body.trim().chars().take(SNIPPET_CHARS).collect(),
                at: m.sent_at.unwrap_or(m.created_at),
            },
        }
    }
}

/// The Inbox page's threads: filter is all, hot, needs_reply, auto_replied or other.
pub(in crate::routes) async fn list_inbox(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(mut q): Query<inbox::ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Thread>>> {
    q.owner = owner_by_email(&state.db, auth.org_id, q.owner).await?;
    let Paginated { items, total } = inbox::page_of(&state.db, auth.org_id, None, q, &page).await?;
    Ok(Json(Paginated { items: items.into_iter().map(Thread::from).collect(), total }))
}

/// One email of a lead's thread, either way, with who it copied in and its files. Download a file with GET
/// /v1/attachments/{id} unless `stored` is false, for a lead's file over 25 MB that Splee didn't keep.
#[derive(Serialize)]
pub(in crate::routes) struct LeadThreadMessage {
    id: Uuid,
    direction: String,
    kind: String,
    channel: String,
    status: String,
    subject: String,
    body: String,
    from_email: Option<String>,
    to_email: Option<String>,
    cc_emails: Vec<String>,
    classification: Option<String>,
    attachments: Vec<EmailAttachment>,
    sent_at: Option<Ts>,
    created_at: Ts,
}

impl From<WithFiles> for LeadThreadMessage {
    fn from(WithFiles { message: m, attachments }: WithFiles) -> Self {
        Self {
            id: m.id,
            direction: m.direction,
            kind: m.kind,
            channel: m.channel,
            status: m.status,
            subject: m.subject,
            body: m.body,
            from_email: m.from_email,
            to_email: m.to_email,
            cc_emails: m.cc_emails,
            classification: m.classification,
            attachments,
            sent_at: m.sent_at,
            created_at: m.created_at,
        }
    }
}

#[derive(Serialize)]
pub(in crate::routes) struct LeadThread {
    lead_id: Uuid,
    /// Oldest first.
    messages: Vec<LeadThreadMessage>,
    /// Who a reply copies in when it doesn't name anyone: everyone the lead copied on their last message, minus your own
    /// mailboxes and the do-not-contact list.
    reply_cc: Vec<String>,
}

/// The lead's emails both ways, like the inbox's thread.
pub(in crate::routes) async fn lead_thread(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<LeadThread>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    let emails = sqlx::query_as::<_, EmailMessage>("select * from email_messages where lead_id = $1 order by created_at")
        .bind(lead.id)
        .fetch_all(&state.db)
        .await?;
    let reply_cc = match emails.iter().rfind(|m| m.direction == "inbound") {
        Some(inbound) => inbox::reply_all_cc(&state.db, auth.org_id, &lead, inbound).await?,
        None => vec![],
    };
    let messages = inbox::with_attachments(&state.db, emails).await?.into_iter().map(LeadThreadMessage::from).collect();
    Ok(Json(LeadThread { lead_id: lead.id, messages, reply_cc }))
}

#[derive(Deserialize)]
pub(in crate::routes) struct ReplyBody {
    body_text: String,
    #[serde(flatten)]
    extras: ReplyExtras,
}

/// The inbox's Send: queues a reply to the lead's last message, which goes out on the sender's next pass.
pub(in crate::routes) async fn reply_to_lead(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<ReplyBody>,
) -> AppResult<Json<LeadThreadMessage>> {
    let queued = inbox::send_reply(&state.db, auth.org_id, id, &body.body_text, &body.extras).await?;
    let mut with_files = inbox::with_attachments(&state.db, vec![queued]).await?;
    Ok(Json(with_files.remove(0).into()))
}

#[derive(Serialize)]
pub(in crate::routes) struct MarkedReplied {
    lead_id: Uuid,
    /// When the lead's latest reply came in. A reply after it brings the thread back to Needs reply.
    answered_through: Ts,
}

/// The inbox's Done, for a reply answered outside Splee. The lead's status stays as it is.
pub(in crate::routes) async fn mark_replied(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<MarkedReplied>> {
    let answered_through = inbox::mark_replied(&state.db, auth.org_id, id).await?;
    Ok(Json(MarkedReplied { lead_id: id, answered_through }))
}

/// A multipart form with the file as `file`, kept for a day for a reply's attachment_ids.
pub(in crate::routes) async fn upload_attachment(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    form: Multipart,
) -> AppResult<Json<EmailAttachment>> {
    Ok(Json(attachments::upload_to(&state.db, auth.org_id, form).await?))
}

pub(in crate::routes) async fn download_attachment(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Response> {
    attachments::download_from(&state.db, auth.org_id, id).await
}
