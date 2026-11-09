use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{util_input, util_scope};
use crate::agent::{self, kind};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Segment;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/segments", get(list).post(create))
        .route("/segments/{id}", get(get_one).patch(update).delete(remove))
        .route("/segments/{id}/find-companies", post(find_companies))
        .route("/segments/{id}/find-people", post(find_people))
        .route("/segments/{id}/write-emails", post(write_emails))
        .route("/segments/{id}/start", post(start))
        .route("/segments/{id}/pause", post(pause))
}

const ICONS: &[&str] = &[
    "building", "store", "hotel", "heart", "palette", "home", "scissors", "briefcase", "graduation-cap", "stethoscope",
    "factory", "truck", "utensils", "code", "shopping-cart", "users", "landmark", "plane", "dumbbell", "camera",
];
const STATUSES: &[&str] = &["draft", "scaling", "working", "paused"];

#[derive(Serialize, FromRow)]
pub struct SegmentWithStats {
    #[serde(flatten)]
    #[sqlx(flatten)]
    segment: Segment,
    companies: i64,
    leads: i64,
    sent: i64,
    contacted: i64,
    replies: i64,
    hot: i64,
    spend_micros: i64,
    #[sqlx(skip)]
    cost_per_hot_micros: Option<i64>,
}

/// Segments of a project with their funnel numbers. Pass `segment_id` to get just one.
/// Agent runs that research the whole project, like explore, have no segment, so their cost only counts in the project's stats.
pub async fn with_stats(db: &PgPool, project_id: Uuid, segment_id: Option<Uuid>) -> AppResult<Vec<SegmentWithStats>> {
    let mut rows = sqlx::query_as::<_, SegmentWithStats>(
        "select s.*,
           (select count(*) from companies c where c.segment_id = s.id) as companies,
           (select count(*) from leads l where l.segment_id = s.id) as leads,
           (select count(*) from email_messages e
             where e.segment_id = s.id and e.direction = 'outbound' and e.status = 'sent') as sent,
           (select count(*) from leads l where l.segment_id = s.id and l.last_contacted_at is not null) as contacted,
           (select count(*) from leads l where l.segment_id = s.id
             and exists (select 1 from email_messages e where e.lead_id = l.id and e.direction = 'inbound'
                         and e.classification is distinct from 'bounce')) as replies,
           (select count(*) from leads l where l.segment_id = s.id and l.status in ('hot', 'meeting_booked')) as hot,
           (select (coalesce(sum(r.cost_usd), 0) * 1000000)::bigint from agent_runs r
             where r.project_id = s.project_id and r.input->>'segment_id' = s.id::text)
           + (select coalesce(sum(e.cost_micros), 0)::bigint from email_messages e
             where e.segment_id = s.id and e.direction = 'outbound' and e.status = 'sent') as spend_micros
         from segments s
         where s.project_id = $1 and ($2::uuid is null or s.id = $2)
         order by s.position, s.created_at",
    )
    .bind(project_id)
    .bind(segment_id)
    .fetch_all(db)
    .await?;
    for row in &mut rows {
        row.cost_per_hot_micros = (row.hot > 0).then(|| row.spend_micros / row.hot);
    }
    Ok(rows)
}

async fn one_with_stats(db: &PgPool, segment: &Segment) -> AppResult<SegmentWithStats> {
    with_stats(db, segment.project_id, Some(segment.id)).await?.pop().ok_or(AppError::NotFound)
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<SegmentWithStats>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(with_stats(&state.db, id, None).await?))
}

#[derive(Deserialize)]
struct SegmentBody {
    prompt: Option<String>,
    name: Option<String>,
    description: Option<String>,
    icon: Option<String>,
    est_size: Option<i64>,
    fit_score: Option<i32>,
    status: Option<String>,
    targeting: Option<Value>,
    pain_points: Option<String>,
    angle: Option<String>,
    position: Option<i32>,
}

impl SegmentBody {
    fn validate(&self) -> AppResult<()> {
        if let Some(icon) = &self.icon {
            util_input::one_of("icon", icon, ICONS)?;
        }
        if let Some(status) = &self.status {
            util_input::one_of("status", status, STATUSES)?;
        }
        if self.targeting.as_ref().is_some_and(|t| !t.is_object()) {
            return Err(AppError::BadRequest("targeting must be an object".into()));
        }
        if self.name.as_deref().is_some_and(|n| n.trim().is_empty()) {
            return Err(AppError::BadRequest("Name can't be empty".into()));
        }
        Ok(())
    }
}

async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<SegmentBody>,
) -> AppResult<Json<Segment>> {
    auth.project(&state.db, id).await?;
    body.validate()?;
    let prompt = body.prompt.as_deref().map(str::trim).filter(|p| !p.is_empty());
    let name = match (prompt, body.name.as_deref()) {
        (Some(_), _) => "New campaign",
        (None, Some(name)) => name.trim(),
        (None, None) => return Err(AppError::BadRequest("Give the campaign a name or describe it".into())),
    };
    let segment = sqlx::query_as::<_, Segment>(
        "insert into segments (project_id, name, description, icon, est_size, fit_score, targeting, pain_points, angle, position)
         values ($1, $2, coalesce($3, ''), coalesce($4, 'building'), coalesce($5, 0), coalesce($6, 0), coalesce($7, '{}'::jsonb),
                 coalesce($8, ''), coalesce($9, ''),
                 coalesce($10, (select coalesce(max(position) + 1, 0) from segments where project_id = $1)))
         returning *",
    )
    .bind(id)
    .bind(name)
    .bind(prompt.or(body.description.as_deref()))
    .bind(body.icon)
    .bind(body.est_size)
    .bind(body.fit_score)
    .bind(body.targeting)
    .bind(body.pain_points)
    .bind(body.angle)
    .bind(body.position)
    .fetch_one(&state.db)
    .await?;
    if let Some(prompt) = prompt {
        agent::enqueue(
            &state.db,
            kind::SEGMENT_FROM_PROMPT,
            Some(id),
            Some(auth.org_id),
            json!({ "segment_id": segment.id, "prompt": prompt }),
        )
        .await?;
    }
    Ok(Json(segment))
}

async fn get_one(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<SegmentWithStats>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    Ok(Json(one_with_stats(&state.db, &segment).await?))
}

async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<SegmentBody>,
) -> AppResult<Json<SegmentWithStats>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    body.validate()?;
    let segment = sqlx::query_as::<_, Segment>(
        "update segments set
           name = coalesce($2, name),
           description = coalesce($3, description),
           icon = coalesce($4, icon),
           est_size = coalesce($5, est_size),
           fit_score = coalesce($6, fit_score),
           status = coalesce($7, status),
           targeting = coalesce($8, targeting),
           pain_points = coalesce($9, pain_points),
           angle = coalesce($10, angle),
           position = coalesce($11, position)
         where id = $1 returning *",
    )
    .bind(segment.id)
    .bind(body.name.as_deref().map(str::trim))
    .bind(body.description)
    .bind(body.icon)
    .bind(body.est_size)
    .bind(body.fit_score)
    .bind(body.status)
    .bind(body.targeting)
    .bind(body.pain_points)
    .bind(body.angle)
    .bind(body.position)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(one_with_stats(&state.db, &segment).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    sqlx::query("delete from segments where id = $1").bind(segment.id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn enqueue_for(state: &AppState, auth: &Auth, segment: &Segment, run_kind: &str, input: Value) -> AppResult<Json<Value>> {
    let run_id = agent::enqueue(&state.db, run_kind, Some(segment.project_id), Some(auth.org_id), input).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

/// Refuses a run for the whole campaign when every lead already has a first email. It would finish instantly with
/// nothing written, and the caller would never hear why.
pub(super) async fn ensure_unwritten_leads(db: &PgPool, segment: &Segment) -> AppResult<()> {
    if agent::leads_without_initial(db, segment.project_id, Some(segment.id)).await?.is_empty() {
        return Err(AppError::Conflict("Every lead in this campaign already has an email. Find or add more people first.".into()));
    }
    Ok(())
}

/// Keeps only ids that belong to the segment's project.
async fn owned_ids(db: &PgPool, table: &'static str, ids: &[Uuid], project_id: Uuid) -> AppResult<Vec<Uuid>> {
    let sql = format!("select id from {table} where id = any($1) and project_id = $2");
    Ok(sqlx::query_scalar(&sql).bind(ids).bind(project_id).fetch_all(db).await?)
}

#[derive(Deserialize, Default)]
struct FindCompaniesBody {
    count: Option<i64>,
}

async fn find_companies(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    body: Option<Json<FindCompaniesBody>>,
) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let count = body.unwrap_or_default().count.unwrap_or(25).clamp(1, 100);
    enqueue_for(&state, &auth, &segment, kind::FIND_COMPANIES, json!({ "segment_id": id, "count": count })).await
}

#[derive(Deserialize, Default)]
struct FindPeopleBody {
    company_ids: Option<Vec<Uuid>>,
}

async fn find_people(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    body: Option<Json<FindPeopleBody>>,
) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let mut input = json!({ "segment_id": id });
    if let Some(ids) = body.unwrap_or_default().0.company_ids {
        input["company_ids"] = json!(owned_ids(&state.db, "companies", &ids, segment.project_id).await?);
    }
    enqueue_for(&state, &auth, &segment, kind::FIND_PEOPLE, input).await
}

#[derive(Deserialize, Default)]
struct WriteEmailsBody {
    lead_ids: Option<Vec<Uuid>>,
    instructions: Option<String>,
}

async fn write_emails(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    body: Option<Json<WriteEmailsBody>>,
) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let body = body.unwrap_or_default().0;
    let mut input = json!({ "segment_id": id });
    if let Some(ids) = body.lead_ids {
        input["lead_ids"] = json!(owned_ids(&state.db, "leads", &ids, segment.project_id).await?);
    } else {
        ensure_unwritten_leads(&state.db, &segment).await?;
    }
    if let Some(instructions) = body.instructions.filter(|i| !i.trim().is_empty()) {
        input["instructions"] = json!(instructions);
    }
    enqueue_for(&state, &auth, &segment, kind::WRITE_EMAILS, input).await
}

async fn start(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("update segments set status = 'working' where id = $1").bind(segment.id).execute(&mut *tx).await?;
    // Pausing turned queued follow-ups back into drafts, so starting queues them again for the day they were due.
    let lead_ids: Vec<Uuid> = sqlx::query_scalar(
        "update email_messages set status = 'approved',
           scheduled_for = case when kind = 'follow_up' then coalesce(scheduled_for, now()) else now() end
         where segment_id = $1 and kind in ('initial', 'follow_up') and status = 'draft' returning lead_id",
    )
    .bind(segment.id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("update leads set status = 'queued' where id = any($1) and status = 'new'")
        .bind(&lead_ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

async fn pause(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("update segments set status = 'paused' where id = $1").bind(segment.id).execute(&mut *tx).await?;
    // Manual and auto replies are answers to real people, so pausing a campaign leaves them queued.
    let lead_ids: Vec<Uuid> = sqlx::query_scalar(
        "update email_messages set status = 'draft'
         where segment_id = $1 and kind in ('initial', 'follow_up') and status in ('approved', 'scheduled')
         returning lead_id",
    )
    .bind(segment.id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("update leads set status = 'new' where id = any($1) and status = 'queued'")
        .bind(&lead_ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::with_stats;
    use crate::routes::stats::tests::seed_costs;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn campaign_spend_counts_its_own_runs_and_emails(db: PgPool) {
        let seeded = seed_costs(&db).await;
        let rows = with_stats(&db, seeded.project, None).await.unwrap();
        let row = |id| rows.iter().find(|r| r.segment.id == id).unwrap();

        let cheap = row(seeded.cheap);
        assert_eq!((cheap.sent, cheap.hot, cheap.spend_micros), (2, 1, 251_000));
        assert_eq!(cheap.cost_per_hot_micros, Some(251_000));

        let pricey = row(seeded.pricey);
        assert_eq!((pricey.sent, pricey.hot, pricey.spend_micros), (1, 0, 500_700));
        assert_eq!(pricey.cost_per_hot_micros, None);
    }
}
