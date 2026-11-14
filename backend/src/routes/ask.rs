//! Ask Splee: a question about the project's pipeline starts an `ask` run straight away. The app follows it on
//! /runs/{id}/events and reads the answer from the run's output. See agent::ask.

use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::agent::{self, ask};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/ask", post(start))
}

#[derive(Deserialize)]
struct AskBody {
    question: String,
    #[serde(default)]
    history: Vec<ask::Turn>,
}

async fn start(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<AskBody>) -> AppResult<Json<Value>> {
    super::api_v1::check_project(&state.db, id, auth.org_id).await?;
    let question = body.question.trim();
    if question.is_empty() {
        return Err(AppError::BadRequest("Ask a question".into()));
    }
    if question.chars().count() > ask::MAX_QUESTION {
        return Err(AppError::BadRequest(format!("Questions can be up to {} characters", ask::MAX_QUESTION)));
    }
    let run_id = agent::enqueue(&state.db, agent::kind::ASK, Some(id), Some(auth.org_id), ask::input(question, body.history)).await?;
    agent::start_now(&state, run_id).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn only_a_real_question_about_your_own_project_starts_a_run(db: PgPool) {
        let org = |name: &'static str| sqlx::query_scalar::<_, Uuid>("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db);
        let (ours, theirs) = (org("Ours").await.unwrap(), org("Theirs").await.unwrap());
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id) values ($1) returning id").bind(theirs).fetch_one(&db).await.unwrap();
        let ours_project: Uuid = sqlx::query_scalar("insert into projects (org_id) values ($1) returning id").bind(ours).fetch_one(&db).await.unwrap();
        let state = crate::autopilot::tests::state(db.clone());
        let auth = || Auth { user_id: Uuid::new_v4(), org_id: ours, role: "member".into(), token_hash: String::new() };
        let ask = |project: Uuid, question: String| start(State(state.clone()), auth(), Path(project), Json(AskBody { question, history: vec![] }));

        assert!(matches!(ask(project, "Who replied?".into()).await, Err(AppError::NotFound)), "another organization's project");
        assert!(matches!(ask(ours_project, "  ".into()).await, Err(AppError::BadRequest(_))));
        assert!(matches!(ask(ours_project, "x".repeat(ask::MAX_QUESTION + 1)).await, Err(AppError::BadRequest(_))));
        let runs: i64 = sqlx::query_scalar("select count(*) from agent_runs").fetch_one(&db).await.unwrap();
        assert_eq!(runs, 0);
    }
}
