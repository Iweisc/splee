use axum::extract::{Path, State};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{util_input, util_scope};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Material;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/materials", get(list).post(create))
        .route("/materials/{id}", patch(update).delete(remove))
}

const KINDS: &[&str] = &["case_study", "doc", "faq", "link", "note"];

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Material>>> {
    auth.project(&state.db, id).await?;
    let materials = sqlx::query_as::<_, Material>("select * from materials where project_id = $1 order by created_at desc")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(materials))
}

#[derive(Deserialize)]
struct MaterialBody {
    kind: Option<String>,
    title: Option<String>,
    content: Option<String>,
    url: Option<String>,
}

impl MaterialBody {
    fn validate(&self) -> AppResult<()> {
        if let Some(kind) = &self.kind {
            util_input::one_of("kind", kind, KINDS)?;
        }
        if self.title.as_deref().is_some_and(|t| t.trim().is_empty()) {
            return Err(AppError::BadRequest("Title can't be empty".into()));
        }
        Ok(())
    }
}

async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<MaterialBody>,
) -> AppResult<Json<Material>> {
    auth.project(&state.db, id).await?;
    body.validate()?;
    let title = body.title.as_deref().map(str::trim).ok_or_else(|| AppError::BadRequest("Title is required".into()))?;
    let material = sqlx::query_as::<_, Material>(
        "insert into materials (project_id, kind, title, content, url)
         values ($1, coalesce($2, 'note'), $3, coalesce($4, ''), coalesce($5, '')) returning *",
    )
    .bind(id)
    .bind(&body.kind)
    .bind(title)
    .bind(&body.content)
    .bind(body.url.as_deref().map(str::trim))
    .fetch_one(&state.db)
    .await?;
    Ok(Json(material))
}

async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<MaterialBody>,
) -> AppResult<Json<Material>> {
    let material: Material = util_scope::load(&state.db, "materials", id, auth.org_id).await?;
    body.validate()?;
    let material = sqlx::query_as::<_, Material>(
        "update materials set kind = coalesce($2, kind), title = coalesce($3, title),
           content = coalesce($4, content), url = coalesce($5, url)
         where id = $1 returning *",
    )
    .bind(material.id)
    .bind(&body.kind)
    .bind(body.title.as_deref().map(str::trim))
    .bind(&body.content)
    .bind(body.url.as_deref().map(str::trim))
    .fetch_one(&state.db)
    .await?;
    Ok(Json(material))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let material: Material = util_scope::load(&state.db, "materials", id, auth.org_id).await?;
    sqlx::query("delete from materials where id = $1").bind(material.id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}
