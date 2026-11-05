use axum::extract::{Path, State};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{util_input, util_scope};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Competitor;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/competitors", get(list).post(create))
        .route("/competitors/{id}", delete(remove))
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Competitor>>> {
    auth.project(&state.db, id).await?;
    let competitors = sqlx::query_as::<_, Competitor>("select * from competitors where project_id = $1 order by created_at")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(competitors))
}

#[derive(Deserialize)]
struct CompetitorBody {
    domain: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    note: String,
}

async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<CompetitorBody>,
) -> AppResult<Json<Competitor>> {
    auth.project(&state.db, id).await?;
    let domain = util_input::domain(&body.domain).ok_or_else(|| AppError::BadRequest("Enter a website like acme.com".into()))?;
    sqlx::query_as::<_, Competitor>(
        "insert into competitors (project_id, domain, name, note) values ($1, $2, $3, $4)
         on conflict (project_id, domain) do nothing returning *",
    )
    .bind(id)
    .bind(&domain)
    .bind(body.name.trim())
    .bind(body.note.trim())
    .fetch_optional(&state.db)
    .await?
    .map(Json)
    .ok_or_else(|| AppError::Conflict(format!("{domain} is already on the list")))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let competitor: Competitor = util_scope::load(&state.db, "competitors", id, auth.org_id).await?;
    sqlx::query("delete from competitors where id = $1").bind(competitor.id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}
