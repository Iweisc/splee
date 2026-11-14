//! Ramp plans on a project's Playbook page: each teammate's latest with how their practice is going, and asking for a new
//! one. Owners and admins see and plan everyone's, everyone else only their own. See agent::ramp.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use super::one_on_ones::allowed;
use crate::agent::practice::{ASSIGNED, Assigned};
use crate::agent::{playbook, ramp};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/ramps", get(list)).route("/projects/{id}/ramps/{user_id}", post(plan))
}

#[derive(Serialize)]
struct Ramp {
    #[serde(flatten)]
    saved: ramp::Saved,
    /// Its practice calls as assigned, with the best score each.
    practice: Vec<Assigned>,
}

#[derive(Serialize)]
struct View {
    ramps: Vec<Ramp>,
    pass_score: i32,
}

async fn view(state: &AppState, auth: &Auth, id: Uuid) -> AppResult<Json<View>> {
    let only = if auth.require_admin().is_ok() { None } else { Some(auth.user_id) };
    let mut ramps = vec![];
    for saved in ramp::latest(&state.db, id, only).await? {
        let practice = sqlx::query_as(&format!("{ASSIGNED} where a.ramp_id = $1 order by a.created_at")).bind(saved.id).fetch_all(&state.db).await?;
        ramps.push(Ramp { saved, practice });
    }
    Ok(Json(View { ramps, pass_score: ramp::PASS }))
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    view(&state, &auth, id).await
}

/// Writes the teammate a new ramp plan from the project's playbook, replacing their last one once it's written.
async fn plan(State(state): State<AppState>, auth: Auth, Path((id, user_id)): Path<(Uuid, Uuid)>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    allowed(&state, &auth, user_id).await?;
    if playbook::written(&state.db, id).await?.is_none() {
        return Err(AppError::BadRequest("Write the playbook first, since the ramp is planned from it".into()));
    }
    if ramp::latest(&state.db, id, Some(user_id)).await?.iter().any(|r| r.status == "writing") {
        return Err(AppError::BadRequest("The agent is already planning this ramp".into()));
    }
    let run = ramp::request(&state.db, id, auth.org_id, user_id, auth.user_id).await?;
    crate::agent::start_now(&state, run).await?;
    view(&state, &auth, id).await
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;
    use crate::agent::practice::fixture;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn members_see_only_their_own_ramp_with_the_best_score_on_each_practice_call(db: PgPool) {
        let t = fixture::team(&db).await;
        let state = crate::autopilot::tests::state(db.clone());
        let auth = |user: Uuid, role: &str| Auth { user_id: user, org_id: t.org, role: role.into(), token_hash: String::new() };
        assert!(matches!(plan(State(state.clone()), auth(t.dana, "owner"), Path((t.project, t.sam))).await, Err(AppError::BadRequest(_))), "no playbook yet");
        assert!(matches!(plan(State(state.clone()), auth(t.sam, "member"), Path((t.project, t.dana))).await, Err(AppError::Forbidden(_))));

        let mut ramps = vec![];
        for user in [t.dana, t.sam] {
            let ramp: Uuid = sqlx::query_scalar("insert into ramps (project_id, user_id, plan) values ($1, $2, '{}') returning id")
                .bind(t.project)
                .bind(user)
                .fetch_one(&db)
                .await
                .unwrap();
            ramps.push(ramp);
        }
        let assignment: Uuid = sqlx::query_scalar(
            "insert into practice_assignments (ramp_id, deal_id, user_id, scenario, skill, drill, pass_score) values ($1, $2, $3, 'pricing', 'Holding price', 'Hold it.', 70) returning id",
        )
        .bind(ramps[1])
        .bind(t.deal)
        .bind(t.sam)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(list(State(state.clone()), auth(t.dana, "owner"), Path(t.project)).await.unwrap().0.ramps.len(), 2);
        let mine = list(State(state.clone()), auth(t.sam, "member"), Path(t.project)).await.unwrap().0;
        assert_eq!(mine.ramps.iter().map(|r| r.saved.user_id).collect::<Vec<_>>(), vec![t.sam]);

        // The best score shows.
        for score in [55, 82, 40] {
            sqlx::query("insert into practice_sessions (project_id, deal_id, user_id, scenario, assignment_id, score) values ($1, $2, $3, 'pricing', $4, $5)")
                .bind(t.project)
                .bind(t.deal)
                .bind(t.sam)
                .bind(assignment)
                .bind(json!({ "score": score }))
                .execute(&db)
                .await
                .unwrap();
            let shown = list(State(state.clone()), auth(t.sam, "member"), Path(t.project)).await.unwrap().0;
            let best = shown.ramps[0].practice[0].score.as_ref().map(|s| s["score"].as_i64().unwrap());
            assert_eq!(best, Some(if score == 55 { 55 } else { 82 }), "after {score}");
        }
    }
}
