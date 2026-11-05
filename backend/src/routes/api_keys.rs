use axum::extract::{Path, State};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::{API_KEY_PREFIX, Auth};
use crate::crypto::{random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::models::ApiKey;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/organization/api-keys", get(list).post(create))
        .route("/organization/api-keys/{id}", delete(revoke))
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<ApiKey>>> {
    let keys = sqlx::query_as::<_, ApiKey>("select * from api_keys where org_id = $1 order by created_at desc")
        .bind(auth.org_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(keys))
}

#[derive(Deserialize)]
struct CreateBody {
    name: String,
}

async fn create(State(state): State<AppState>, auth: Auth, Json(body): Json<CreateBody>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("Give the key a name".into()));
    }
    let secret = format!("{API_KEY_PREFIX}{}", random_token());
    let api_key = sqlx::query_as::<_, ApiKey>(
        "insert into api_keys (org_id, name, prefix, key_hash) values ($1, $2, $3, $4) returning *",
    )
    .bind(auth.org_id)
    .bind(name)
    .bind(&secret[..12])
    .bind(sha256_hex(&secret))
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({ "api_key": api_key, "secret": secret })))
}

async fn revoke(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    let revoked = sqlx::query("update api_keys set revoked_at = now() where id = $1 and org_id = $2 and revoked_at is null")
        .bind(id)
        .bind(auth.org_id)
        .execute(&state.db)
        .await?
        .rows_affected();
    if revoked == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}
