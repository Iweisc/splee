//! The Feedback page: what buyers ask the product to do, ranked by the open and lost deals of the people who asked, with
//! their words. The team renames, merges, plans, declines and ships requests, and shipping can have the agent email
//! everyone who asked. See agent::feedback.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::util_scope;
use crate::agent::feedback;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/feedback", get(list))
        .route("/projects/{id}/feedback/scan", post(scan_now))
        .route("/feedback/{id}", post(update).delete(remove))
        .route("/feedback/{id}/ship", post(ship))
        .route("/feedback/{id}/merge", post(merge))
        .route("/feedback-mentions/{id}", delete(remove_mention))
}

/// Quotes a request shows, the newest first.
const MAX_MENTIONS: i64 = 25;
const STATUSES: [&str; 3] = ["open", "planned", "declined"];

#[derive(Serialize, sqlx::FromRow)]
struct Scan {
    /// scanning while a run reads, else idle.
    status: String,
    scanned_at: Option<Ts>,
}

#[derive(Serialize, sqlx::FromRow)]
struct Request {
    id: Uuid,
    title: String,
    detail: String,
    status: String,
    shipped_note: String,
    shipped_at: Option<Ts>,
    created_at: Ts,
    people: i64,
    companies: i64,
    /// The open, lost and won deals of the people who asked, in the organization's currency.
    open_cents: i64,
    lost_cents: i64,
    won_cents: i64,
    last_said_at: Option<Ts>,
    /// While the agent drafts the emails saying it shipped.
    announcing: bool,
    drafted: i64,
    sent: i64,
    skipped: i64,
    #[sqlx(skip)]
    mentions: Vec<Mention>,
}

#[derive(Serialize, sqlx::FromRow)]
struct Mention {
    #[serde(skip)]
    request_id: Uuid,
    id: Uuid,
    lead_id: Uuid,
    name: String,
    company: String,
    source: String,
    quote: String,
    said_at: Ts,
    deal_id: Option<Uuid>,
    deal_status: Option<String>,
}

#[derive(Serialize)]
struct View {
    currency: String,
    scan: Scan,
    requests: Vec<Request>,
}

async fn view(state: &AppState, project_id: Uuid, org_id: Uuid) -> AppResult<Json<View>> {
    let db = &state.db;
    let currency: String = sqlx::query_scalar("select currency from organizations where id = $1").bind(org_id).fetch_one(db).await?;
    let scan = sqlx::query_as::<_, Scan>(
        "select case when r.status in ('queued', 'running') then 'scanning' else 'idle' end as status, s.scanned_at
         from feedback_scans s left join agent_runs r on r.id = s.run_id where s.project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await?
    .unwrap_or(Scan { status: "idle".into(), scanned_at: None });
    let mut requests = sqlx::query_as::<_, Request>(
        "select f.id, f.title, f.detail, f.status, f.shipped_note, f.shipped_at, f.created_at,
                (select count(distinct m.lead_id) from feedback_mentions m where m.request_id = f.id) as people,
                (select count(distinct coalesce(nullif(lower(l.company_domain::text), ''), nullif(lower(trim(l.company_name)), ''), l.id::text))
                 from feedback_mentions m join leads l on l.id = m.lead_id where m.request_id = f.id) as companies,
                v.open_cents, v.lost_cents, v.won_cents,
                (select max(m.said_at) from feedback_mentions m where m.request_id = f.id) as last_said_at,
                coalesce(r.status in ('queued', 'running'), false) as announcing,
                coalesce(n.drafted, 0) as drafted, coalesce(n.sent, 0) as sent, coalesce(n.skipped, 0) as skipped
         from feedback_requests f
         left join agent_runs r on r.id = f.announce_run_id
         left join lateral (
             select coalesce(sum(d.value_cents) filter (where d.status = 'open'), 0)::bigint as open_cents,
                    coalesce(sum(d.value_cents) filter (where d.status = 'lost'), 0)::bigint as lost_cents,
                    coalesce(sum(d.value_cents) filter (where d.status = 'won'), 0)::bigint as won_cents
             from deals d where d.lead_id in (select m.lead_id from feedback_mentions m where m.request_id = f.id) and lower(d.currency) = lower($2)
         ) v on true
         left join lateral (
             select count(*) filter (where e.status = 'draft') as drafted,
                    count(*) filter (where e.status in ('approved', 'scheduled', 'sending', 'sent')) as sent,
                    count(*) filter (where n.email_id is null and n.skipped <> '') as skipped
             from feedback_notices n left join email_messages e on e.id = n.email_id where n.request_id = f.id
         ) n on true
         where f.project_id = $1
         order by v.open_cents + v.lost_cents desc, people desc, f.created_at desc",
    )
    .bind(project_id)
    .bind(&currency)
    .fetch_all(db)
    .await?;
    let mentions = sqlx::query_as::<_, Mention>(
        "select * from (
             select m.request_id, m.id, m.lead_id, l.full_name as name, l.company_name as company, m.source, m.quote, m.said_at,
                    d.id as deal_id, d.status as deal_status,
                    row_number() over (partition by m.request_id order by m.said_at desc) as n
             from feedback_mentions m join feedback_requests f on f.id = m.request_id join leads l on l.id = m.lead_id
             left join lateral (select id, status from deals where lead_id = m.lead_id order by created_at desc limit 1) d on true
             where f.project_id = $1
         ) x where n <= $2 order by said_at desc",
    )
    .bind(project_id)
    .bind(MAX_MENTIONS)
    .fetch_all(db)
    .await?;
    let mut by_request: HashMap<Uuid, Vec<Mention>> = HashMap::new();
    for m in mentions {
        by_request.entry(m.request_id).or_default().push(m);
    }
    for r in &mut requests {
        r.mentions = by_request.remove(&r.id).unwrap_or_default();
    }
    Ok(Json(View { currency, scan, requests }))
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    view(&state, id, auth.org_id).await
}

/// Has the agent read what's new now, or keeps the scan already running.
async fn scan_now(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    let run = feedback::request(&state.db, id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    view(&state, id, auth.org_id).await
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    project_id: Uuid,
}

async fn load(state: &AppState, auth: &Auth, id: Uuid) -> AppResult<Row> {
    util_scope::load(&state.db, "feedback_requests", id, auth.org_id).await
}

#[derive(Deserialize)]
struct Update {
    title: Option<String>,
    detail: Option<String>,
    status: Option<String>,
}

/// Renames a request or moves it to open, planned or declined. Shipping goes through /ship.
async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Update>) -> AppResult<Json<View>> {
    let row = load(&state, &auth, id).await?;
    if body.title.as_deref().is_some_and(|t| t.trim().is_empty() || t.len() > 200) || body.detail.as_deref().is_some_and(|d| d.len() > 1000) {
        return Err(AppError::BadRequest("Give it a title under 200 characters".into()));
    }
    if body.status.as_deref().is_some_and(|s| !STATUSES.contains(&s)) {
        return Err(AppError::BadRequest("A request is open, planned or declined, or shipped from its Ship button".into()));
    }
    sqlx::query(
        "update feedback_requests set title = coalesce($2, title), detail = coalesce($3, detail), status = coalesce($4, status),
                shipped_at = case when $4 is null then shipped_at end
         where id = $1",
    )
    .bind(row.id)
    .bind(body.title.as_deref().map(str::trim))
    .bind(body.detail.as_deref().map(str::trim))
    .bind(body.status.as_deref())
    .execute(&state.db)
    .await?;
    view(&state, row.project_id, auth.org_id).await
}

#[derive(Deserialize)]
struct Ship {
    #[serde(default)]
    note: String,
    email: bool,
}

/// Marks it shipped, and with `email` has the agent draft an email to everyone who asked and hasn't had one.
async fn ship(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Ship>) -> AppResult<Json<View>> {
    let row = load(&state, &auth, id).await?;
    if body.note.len() > 2000 {
        return Err(AppError::BadRequest("Keep the note under 2,000 characters".into()));
    }
    if body.email && body.note.trim().is_empty() {
        return Err(AppError::BadRequest("Say what shipped, so the emails can tell them".into()));
    }
    if let Some(run) = feedback::ship(&state.db, row.id, row.project_id, auth.org_id, &body.note, body.email).await? {
        crate::agent::start_now(&state, run).await?;
    }
    view(&state, row.project_id, auth.org_id).await
}

#[derive(Deserialize)]
struct Merge {
    into: Uuid,
}

/// Moves a request's quotes and emails into another one in the same project and deletes it.
async fn merge(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Merge>) -> AppResult<Json<View>> {
    let row = load(&state, &auth, id).await?;
    let into = load(&state, &auth, body.into).await?;
    if into.project_id != row.project_id || into.id == row.id {
        return Err(AppError::BadRequest("Pick another request in this project".into()));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "insert into feedback_mentions (request_id, lead_id, source, source_id, quote, said_at, created_at)
         select $2, lead_id, source, source_id, quote, said_at, created_at from feedback_mentions where request_id = $1 on conflict do nothing",
    )
    .bind(row.id)
    .bind(into.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "insert into feedback_notices (request_id, lead_id, email_id, skipped, created_at)
         select $2, lead_id, email_id, skipped, created_at from feedback_notices where request_id = $1 on conflict do nothing",
    )
    .bind(row.id)
    .bind(into.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("delete from feedback_requests where id = $1").bind(row.id).execute(&mut *tx).await?;
    tx.commit().await?;
    view(&state, row.project_id, auth.org_id).await
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    let row = load(&state, &auth, id).await?;
    sqlx::query("delete from feedback_requests where id = $1").bind(row.id).execute(&state.db).await?;
    view(&state, row.project_id, auth.org_id).await
}

/// Takes a quote off a request the agent filed it under by mistake.
async fn remove_mention(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    let request_id: Uuid = sqlx::query_scalar("select request_id from feedback_mentions where id = $1").bind(id).fetch_optional(&state.db).await?.ok_or(AppError::NotFound)?;
    let row = load(&state, &auth, request_id).await?;
    sqlx::query("delete from feedback_mentions where id = $1").bind(id).execute(&state.db).await?;
    view(&state, row.project_id, auth.org_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn requests_rank_by_the_deals_behind_them_and_merge_into_one(db: sqlx::PgPool) {
        let t = crate::agent::practice::fixture::team(&db).await;
        let ines: Uuid = sqlx::query_scalar("select lead_id from deals where id = $1").bind(t.deal).fetch_one(&db).await.unwrap();
        let lead = |name: &'static str, company: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, full_name, company_name) values ($1, $2, $3) returning id").bind(t.project).bind(name).bind(company).fetch_one(&db)
        };
        let colleague = lead("Rui Sousa", "lumenfold ").await.unwrap();
        let kai = lead("Kai Moss", "Orbitdesk").await.unwrap();
        sqlx::query(
            "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, closed_at)
             select org_id, project_id, $2, stage_id, 'lost', 200000, 'usd', now() from deals where id = $1",
        )
        .bind(t.deal)
        .bind(kai)
            .execute(&db)
            .await
            .unwrap();
        let request = |title: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into feedback_requests (project_id, title) values ($1, $2) returning id").bind(t.project).bind(title).fetch_one(&db)
        };
        let (sync, sso, crm) = (request("Salesforce sync").await.unwrap(), request("SSO").await.unwrap(), request("CRM sync").await.unwrap());
        for (request, lead, quote) in [(sync, ines, "need Salesforce"), (sync, colleague, "Salesforce please"), (sso, kai, "no SSO, no deal"), (crm, colleague, "sync our CRM")] {
            sqlx::query("insert into feedback_mentions (request_id, lead_id, source, source_id, quote, said_at) values ($1, $2, 'email', gen_random_uuid(), $3, now())")
                .bind(request)
                .bind(lead)
                .bind(quote)
                .execute(&db)
                .await
                .unwrap();
        }
        let state = crate::autopilot::tests::state(db.clone());
        let auth = || Auth { user_id: t.sam, org_id: t.org, role: "member".into(), token_hash: String::new() };

        let Json(v) = list(State(state.clone()), auth(), Path(t.project)).await.unwrap();
        let rows: Vec<_> = v.requests.iter().map(|r| (r.title.as_str(), r.people, r.companies, r.open_cents, r.lost_cents, r.mentions.len())).collect();
        assert_eq!(rows, vec![("Salesforce sync", 2, 1, 480000, 0, 2), ("SSO", 1, 1, 0, 200000, 1), ("CRM sync", 1, 1, 0, 0, 1)]);
        assert_eq!(v.requests[1].mentions[0].deal_status.as_deref(), Some("lost"));

        let status = |s: &str| Update { title: None, detail: None, status: Some(s.into()) };
        assert!(update(State(state.clone()), auth(), Path(sso), Json(status("shipped"))).await.is_err(), "shipping goes through /ship");
        let Json(v) = update(State(state.clone()), auth(), Path(sso), Json(status("planned"))).await.unwrap();
        assert_eq!(v.requests[1].status, "planned");

        let Json(v) = merge(State(state.clone()), auth(), Path(crm), Json(Merge { into: sync })).await.unwrap();
        let rows: Vec<_> = v.requests.iter().map(|r| (r.title.as_str(), r.people, r.mentions.len())).collect();
        assert_eq!(rows, vec![("Salesforce sync", 2, 3), ("SSO", 1, 1)]);
        assert!(merge(State(state.clone()), auth(), Path(sync), Json(Merge { into: sync })).await.is_err());

        let other = Auth { org_id: Uuid::new_v4(), ..auth() };
        assert!(remove(State(state.clone()), other, Path(sso)).await.is_err(), "another organization's request is out of reach");
        let mention = v.requests[1].mentions[0].id;
        let Json(v) = remove_mention(State(state.clone()), auth(), Path(mention)).await.unwrap();
        assert_eq!((v.requests[1].people, v.requests[1].lost_cents), (0, 0));
    }
}
