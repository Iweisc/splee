use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::util_input;
use super::util_pagination::{PageQuery, Paginated};
use crate::agent::{self, kind};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::mail;
use crate::models::EmailMessage;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/emails", get(list))
        .route("/emails/{id}", get(get_one).patch(update))
        .route("/emails/{id}/regenerate", post(regenerate))
        .route("/emails/{id}/test", post(send_test))
}

#[derive(Serialize, FromRow)]
struct EmailRow {
    #[serde(flatten)]
    #[sqlx(flatten)]
    email: EmailMessage,
    lead_name: String,
    lead_email: Option<String>,
    company_name: String,
    segment_name: Option<String>,
    project_name: String,
}

const ROW_SELECT: &str = "select e.*, l.full_name as lead_name, l.email::text as lead_email, l.company_name,
       s.name as segment_name, p.name as project_name
     from email_messages e
     join leads l on l.id = e.lead_id
     join projects p on p.id = e.project_id
     left join segments s on s.id = e.segment_id";

#[derive(Deserialize)]
struct ListQuery {
    project_id: Option<Uuid>,
    segment_id: Option<Uuid>,
    status: Option<String>,
    kind: Option<String>,
    q: Option<String>,
    /// "soonest" sorts by scheduled time, earliest first, falling back to creation time. Otherwise newest first.
    order: Option<String>,
}

const LIST_FILTER: &str = "where p.org_id = $1 and e.direction = 'outbound'
       and ($2::uuid is null or e.project_id = $2)
       and ($3::uuid is null or e.segment_id = $3)
       and ($4::text is null or e.status = any(string_to_array($4, ',')))
       and ($5::text is null or e.kind = any(string_to_array($5, ',')))
       and ($6::text is null or e.subject ilike $6 or l.full_name ilike $6
            or l.email::text ilike $6 or l.company_name ilike $6)";

async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<EmailRow>>> {
    let like = util_input::like_pattern(q.q.as_deref());
    let status = q.status.filter(|s| !s.is_empty());
    let kind = q.kind.filter(|k| !k.is_empty());
    let total: i64 = sqlx::query_scalar(&format!(
        "select count(*) from email_messages e join leads l on l.id = e.lead_id
         join projects p on p.id = e.project_id {LIST_FILTER}"
    ))
    .bind(auth.org_id)
    .bind(q.project_id)
    .bind(q.segment_id)
    .bind(&status)
    .bind(&kind)
    .bind(&like)
    .fetch_one(&state.db)
    .await?;
    // An approved email without a time is due now, so its creation time puts it first.
    let order = if q.order.as_deref() == Some("soonest") {
        "coalesce(e.scheduled_for, e.created_at), e.created_at"
    } else {
        "coalesce(e.sent_at, e.scheduled_for, e.created_at) desc"
    };
    let items = sqlx::query_as::<_, EmailRow>(&format!("{ROW_SELECT} {LIST_FILTER} order by {order} limit $7 offset $8"))
    .bind(auth.org_id)
    .bind(q.project_id)
    .bind(q.segment_id)
    .bind(&status)
    .bind(&kind)
    .bind(&like)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Paginated { items, total }))
}

async fn load(db: &PgPool, id: Uuid, org_id: Uuid) -> AppResult<EmailRow> {
    sqlx::query_as::<_, EmailRow>(&format!("{ROW_SELECT} where e.id = $1 and p.org_id = $2"))
        .bind(id)
        .bind(org_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

async fn get_one(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<EmailRow>> {
    Ok(Json(load(&state.db, id, auth.org_id).await?))
}

const EDITABLE: &[&str] = &["draft", "approved", "scheduled", "failed", "cancelled"];

#[derive(Deserialize)]
struct UpdateBody {
    subject: Option<String>,
    body: Option<String>,
    status: Option<String>,
}

async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> AppResult<Json<EmailRow>> {
    let row = load(&state.db, id, auth.org_id).await?;
    if row.email.direction != "outbound" || !EDITABLE.contains(&row.email.status.as_str()) {
        return Err(AppError::BadRequest("This email was already sent and can't be changed".into()));
    }
    if let Some(status) = &body.status {
        util_input::one_of("status", status, &["approved", "cancelled"])?;
    }
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "update email_messages set
           subject = coalesce($2, subject),
           body = coalesce($3, body),
           status = coalesce($4, status),
           scheduled_for = case when $4 = 'approved' then coalesce(scheduled_for, now()) else scheduled_for end,
           error = case when $4 = 'approved' then '' else error end,
           -- Sending a failed email again gives a server that asked to try later its full set of tries again.
           deferrals = case when $4 = 'approved' then 0 else deferrals end
         where id = $1",
    )
    .bind(id)
    .bind(body.subject)
    .bind(body.body)
    .bind(&body.status)
    .execute(&mut *tx)
    .await?;
    if body.status.as_deref() == Some("approved") {
        sqlx::query("update leads set status = 'queued' where id = $1 and status = 'new'")
            .bind(row.email.lead_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Json(load(&state.db, id, auth.org_id).await?))
}

#[derive(Deserialize, Default)]
struct RegenerateBody {
    instructions: Option<String>,
}

async fn regenerate(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    body: Option<Json<RegenerateBody>>,
) -> AppResult<Json<Value>> {
    let row = load(&state.db, id, auth.org_id).await?;
    if row.email.direction != "outbound" || !EDITABLE.contains(&row.email.status.as_str()) {
        return Err(AppError::BadRequest("This email was already sent and can't be rewritten".into()));
    }
    // Rewrite mode handles follow-ups too. The plain writer would add a second first email to an already contacted lead.
    let mut input = json!({ "lead_ids": [row.email.lead_id], "rewrite": true });
    if let Some(instructions) = body.unwrap_or_default().0.instructions.filter(|i| !i.trim().is_empty()) {
        input["instructions"] = json!(instructions);
    }
    let run_id = agent::enqueue(&state.db, kind::WRITE_EMAILS, Some(row.email.project_id), Some(auth.org_id), input).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

#[derive(Deserialize, Default)]
struct TestBody {
    subject: Option<String>,
    body: Option<String>,
}

/// Sends the email, with any unsaved edits, to the signed-in user, so they can check it in a real inbox before approving it.
async fn send_test(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, body: Option<Json<TestBody>>) -> AppResult<Json<Value>> {
    let row = load(&state.db, id, auth.org_id).await?;
    if row.email.direction != "outbound" {
        return Err(AppError::BadRequest("Only emails to leads can be sent as a test".into()));
    }
    let edits = body.unwrap_or_default().0;
    let to: String = sqlx::query_scalar("select email::text from users where id = $1").bind(auth.user_id).fetch_one(&state.db).await?;
    let subject = edits.subject.unwrap_or(row.email.subject);
    let text = edits.body.unwrap_or(row.email.body);
    match mail::send_test(&state, auth.org_id, &to, &row.email.kind, &subject, &text).await {
        Ok(true) => Ok(Json(json!({ "sent_to": to }))),
        Ok(false) => Err(AppError::BadRequest("Connect or resume a mailbox first. The test goes out from it.".into())),
        Err(e) => Err(AppError::BadRequest(format!("Couldn't send the test: {e:#}"))),
    }
}
