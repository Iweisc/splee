//! Hiring signals: open roles at a project's companies that match the roles it watches, and adding the companies behind
//! them to a campaign.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::segments::{FindPeopleBody, start_find_people};
use super::util_scope;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::hiring;
use crate::models::{Segment, Ts};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/hiring/openings", get(openings))
        .route("/projects/{id}/hiring/roles", get(roles))
        .route("/projects/{id}/hiring/add", post(add))
}

/// An open role and the company behind it. company_id is set when the company is in the project, and leads counts the
/// project's leads on its domain.
#[derive(Serialize, FromRow)]
pub struct Signal {
    id: Uuid,
    domain: String,
    company_id: Option<Uuid>,
    company_name: String,
    segment_id: Option<Uuid>,
    leads: i64,
    title: String,
    role: String,
    location: String,
    department: String,
    url: String,
    posted_at: Option<Ts>,
    first_seen: Ts,
}

#[derive(Deserialize)]
struct OpeningsQuery {
    /// Only roles first seen in this many days. Every open role without it.
    days: Option<i32>,
}

/// The page and the badges show this many at most. A project checks 500 domains a day, and few of them hire for the roles.
const MAX_OPENINGS: i64 = 1000;

async fn openings(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(q): Query<OpeningsQuery>) -> AppResult<Json<Vec<Signal>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(signals(&state.db, id, q.days.filter(|d| *d > 0)).await?))
}

async fn signals(db: &PgPool, project_id: Uuid, days: Option<i32>) -> sqlx::Result<Vec<Signal>> {
    sqlx::query_as(
        "select j.id, j.domain::text, c.id as company_id, coalesce(nullif(c.name, ''), l.company_name, j.domain::text) as company_name,
                c.segment_id, coalesce(l.leads, 0) as leads, j.title, j.role, j.location, j.department, j.url, j.posted_at, j.first_seen
         from job_openings j
         left join companies c on c.project_id = j.project_id and c.domain = j.domain
         left join lateral (select count(*) as leads, nullif(max(company_name), '') as company_name from leads
                            where project_id = j.project_id and company_domain = j.domain) l on true
         where j.project_id = $1 and j.closed_at is null and ($2::int is null or j.first_seen > now() - make_interval(days => $2))
         order by j.first_seen desc, company_name, j.title
         limit $3",
    )
    .bind(project_id)
    .bind(days)
    .bind(MAX_OPENINGS)
    .fetch_all(db)
    .await
}

/// The roles the Targeting page suggests until the project picks its own.
async fn roles(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    Ok(Json(json!({ "default_roles": hiring::default_roles(&state.db, id).await? })))
}

#[derive(Deserialize)]
struct AddBody {
    segment_id: Uuid,
    company_ids: Vec<Uuid>,
}

/// Moves companies that have no leads yet into the campaign and starts "Find people" on them, so the agent finds who to
/// email about the role.
async fn add(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<AddBody>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", body.segment_id, auth.org_id).await?;
    if segment.project_id != id {
        return Err(AppError::BadRequest("That campaign isn't part of this project".into()));
    }
    let moved = move_to_campaign(&state.db, id, segment.id, &body.company_ids).await?;
    if moved.is_empty() {
        return Err(AppError::Conflict("These companies already have leads".into()));
    }
    let run_id = start_find_people(&state.db, auth.org_id, &segment, FindPeopleBody { company_ids: Some(moved.clone()) }).await?;
    Ok(Json(json!({ "run_id": run_id, "added": moved.len() })))
}

async fn move_to_campaign(db: &PgPool, project_id: Uuid, segment_id: Uuid, company_ids: &[Uuid]) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "update companies c set segment_id = $3, status = 'new'
         where c.project_id = $1 and c.id = any($2)
           and not exists (select 1 from leads l where l.project_id = c.project_id and (l.company_id = c.id or l.company_domain = c.domain))
         returning c.id",
    )
    .bind(project_id)
    .bind(company_ids)
    .bind(segment_id)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn signals_name_the_company_and_only_companies_without_leads_move(db: PgPool) {
        let project: Uuid = sqlx::query_scalar("insert into projects default values returning id").fetch_one(&db).await.unwrap();
        let segment: Uuid =
            sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Hiring') returning id").bind(project).fetch_one(&db).await.unwrap();
        let company = |domain: &'static str, name: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into companies (project_id, domain, name, status) values ($1, $2, $3, 'no_people') returning id")
                .bind(project)
                .bind(domain)
                .bind(name)
                .fetch_one(&db)
        };
        let fresh = company("fresh.com", "Fresh").await.unwrap();
        let known = company("known.com", "Known").await.unwrap();
        sqlx::query("insert into leads (project_id, full_name, email, company_name, company_domain) values ($1, 'Bo', 'bo@known.com', 'Known', 'known.com'),
                     ($1, 'Cy', 'cy@leadonly.com', 'Lead Only', 'leadonly.com')")
            .bind(project)
            .execute(&db)
            .await
            .unwrap();
        for (domain, title, age, closed) in [
            ("fresh.com", "SDR", "1 day", false),
            ("known.com", "Account Executive", "10 days", false),
            ("leadonly.com", "SDR", "2 days", false),
            ("fresh.com", "Old SDR", "1 day", true),
        ] {
            sqlx::query(
                "insert into job_openings (project_id, domain, provider, title, url, first_seen, closed_at)
                 values ($1, $2, 'lever', $3, 'https://jobs.lever.co/' || $2 || '/' || $3, now() - $4::interval, case when $5 then now() end)",
            )
            .bind(project)
            .bind(domain)
            .bind(title)
            .bind(age)
            .bind(closed)
            .execute(&db)
            .await
            .unwrap();
        }

        let week = super::signals(&db, project, Some(7)).await.unwrap();
        let shown: Vec<(&str, &str, i64, bool)> = week.iter().map(|s| (s.company_name.as_str(), s.title.as_str(), s.leads, s.company_id.is_some())).collect();
        assert_eq!(shown, [("Fresh", "SDR", 0, true), ("Lead Only", "SDR", 1, false)]);
        assert_eq!(super::signals(&db, project, None).await.unwrap().len(), 3);

        let moved = super::move_to_campaign(&db, project, segment, &[fresh, known]).await.unwrap();
        assert_eq!(moved, [fresh]);
        let (segment_id, status): (Option<Uuid>, String) =
            sqlx::query_as("select segment_id, status from companies where id = $1").bind(fresh).fetch_one(&db).await.unwrap();
        assert_eq!((segment_id, status.as_str()), (Some(segment), "new"));
    }
}
