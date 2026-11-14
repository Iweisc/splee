//! Sales contests on the Targets tab: everyone sees them, owners and admins start and delete them. See contests.rs.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Auth;
use crate::contests::{self, Contest, METRICS};
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::state::AppState;

/// How far ahead a contest may end.
const MAX_DAYS: i64 = 92;

pub fn router() -> Router<AppState> {
    Router::new().route("/contests", get(list).post(start)).route("/contests/{id}", delete(remove))
}

#[derive(Serialize)]
struct View {
    currency: String,
    contests: Vec<Contest>,
}

async fn view(state: &AppState, auth: &Auth) -> AppResult<Json<View>> {
    let (currency, contests) = contests::list(&state.db, auth.org_id).await?;
    Ok(Json(View { currency, contests }))
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<View>> {
    view(&state, &auth).await
}

#[derive(Deserialize)]
struct Start {
    name: String,
    metric: String,
    #[serde(default)]
    prize: String,
    ends_at: Ts,
}

/// Starts a contest now and tells the team's chats. Owners and admins only.
async fn start(State(state): State<AppState>, auth: Auth, Json(body): Json<Start>) -> AppResult<Json<View>> {
    auth.require_admin()?;
    let name = body.name.trim();
    let prize = body.prize.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(AppError::BadRequest("Give the contest a name of up to 80 characters".into()));
    }
    if prize.chars().count() > 120 {
        return Err(AppError::BadRequest("Keep the prize under 120 characters".into()));
    }
    if !METRICS.contains(&body.metric.as_str()) {
        return Err(AppError::BadRequest("Pick what the contest counts".into()));
    }
    if body.ends_at <= Utc::now() || body.ends_at > Utc::now() + chrono::Duration::days(MAX_DAYS) {
        return Err(AppError::BadRequest(format!("A contest ends in the next {MAX_DAYS} days")));
    }
    let (creator, tz): (String, String) = sqlx::query_as(
        "select coalesce(nullif(u.name, ''), u.email::text),
                coalesce((select s.timezone from projects p join project_settings s on s.project_id = p.id where p.org_id = $2 order by p.created_at limit 1), 'UTC')
         from users u where u.id = $1",
    )
    .bind(auth.user_id)
    .bind(auth.org_id)
    .fetch_one(&state.db)
    .await?;
    sqlx::query("insert into contests (org_id, name, metric, prize, ends_at, created_by) values ($1, $2, $3, $4, $5, $6)")
        .bind(auth.org_id)
        .bind(name)
        .bind(&body.metric)
        .bind(prize)
        .bind(body.ends_at)
        .bind(auth.user_id)
        .execute(&state.db)
        .await?;
    let tz: chrono_tz::Tz = tz.parse().unwrap_or(chrono_tz::UTC);
    let until = body.ends_at.with_timezone(&tz).format("%a %-d %b");
    let prize = if prize.is_empty() { String::new() } else { format!(" The prize: {prize}.") };
    contests::post(&state, auth.org_id, "Contest on", &format!("{creator} started {name}, for the most {} until {until}.{prize}", label(&body.metric))).await;
    view(&state, &auth).await
}

/// What the contest counts, as in "the most calls".
fn label(metric: &str) -> &'static str {
    match metric {
        "won_value" => "revenue won",
        "deals_won" => "deals won",
        "pipeline" => "new pipeline",
        "calls" => "calls",
        _ => "practice calls passed",
    }
}

/// Deletes a contest without announcing it. Owners and admins only.
async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<StatusCode> {
    auth.require_admin()?;
    let done = sqlx::query("delete from contests where id = $1 and org_id = $2").bind(id).bind(auth.org_id).execute(&state.db).await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn admins_start_and_delete_contests_and_everyone_sees_them(db: PgPool) {
        use crate::agent::practice::fixture;
        let t = fixture::team(&db).await;
        let state = crate::autopilot::tests::state(db.clone());
        let auth = |user: Uuid, role: &str| Auth { user_id: user, org_id: t.org, role: role.into(), token_hash: String::new() };
        let body = |metric: &str, days: i64| Start { name: " Call week ".into(), metric: metric.into(), prize: "Lunch".into(), ends_at: Utc::now() + chrono::Duration::days(days) };

        assert!(start(State(state.clone()), auth(t.sam, "member"), Json(body("calls", 5))).await.is_err(), "members can't start one");
        assert!(start(State(state.clone()), auth(t.dana, "owner"), Json(body("emails", 5))).await.is_err(), "only the known metrics");
        assert!(start(State(state.clone()), auth(t.dana, "owner"), Json(body("calls", -1))).await.is_err(), "it ends in the future");
        assert!(start(State(state.clone()), auth(t.dana, "owner"), Json(body("calls", 100))).await.is_err(), "within 92 days");
        let Json(started) = start(State(state.clone()), auth(t.dana, "owner"), Json(body("calls", 5))).await.unwrap();
        assert_eq!(started.contests.len(), 1);

        let Json(v) = list(State(state.clone()), auth(t.sam, "member")).await.unwrap();
        let c = &v.contests[0];
        assert_eq!((c.name.as_str(), c.prize.as_str(), c.created_by.as_deref(), c.standings.len()), ("Call week", "Lunch", Some("Dana Reyes"), 2));
        assert!(remove(State(state.clone()), auth(t.sam, "member"), Path(c.id)).await.is_err());
        assert_eq!(remove(State(state.clone()), auth(t.dana, "admin"), Path(c.id)).await.unwrap(), StatusCode::NO_CONTENT);
        assert!(list(State(state), auth(t.sam, "member")).await.unwrap().0.contests.is_empty());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn an_ended_contest_is_announced_once(db: PgPool) {
        use crate::agent::practice::fixture;
        let t = fixture::team(&db).await;
        let state = crate::autopilot::tests::state(db.clone());
        sqlx::query("insert into contests (org_id, name, metric, starts_at, ends_at) values ($1, 'Last week', 'calls', now() - interval '8 days', now() - interval '1 day'), ($1, 'Running', 'calls', now(), now() + interval '1 day')")
            .bind(t.org)
            .execute(&db)
            .await
            .unwrap();
        contests::announce_ended(&state).await.unwrap();
        let announced: Vec<String> = sqlx::query_scalar("select name from contests where announced_at is not null").fetch_all(&db).await.unwrap();
        assert_eq!(announced, ["Last week"]);
        let at: Ts = sqlx::query_scalar("select announced_at from contests where name = 'Last week'").fetch_one(&db).await.unwrap();
        contests::announce_ended(&state).await.unwrap();
        let again: Ts = sqlx::query_scalar("select announced_at from contests where name = 'Last week'").fetch_one(&db).await.unwrap();
        assert_eq!(at, again);
    }
}
