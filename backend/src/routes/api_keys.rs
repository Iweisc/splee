use axum::extract::{Path, State};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::audit::{self, Target};
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
pub(super) struct CreateBody {
    name: String,
}

pub(super) async fn create(State(state): State<AppState>, auth: Auth, Json(body): Json<CreateBody>) -> AppResult<Json<Value>> {
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
    audit::record(&state.db, &auth, "api_key.created", Target::new("api_key", api_key.id, &api_key.name), json!({ "prefix": api_key.prefix })).await;
    Ok(Json(json!({ "api_key": api_key, "secret": secret })))
}

pub(super) async fn revoke(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    let revoked: Option<(String, String)> =
        sqlx::query_as("update api_keys set revoked_at = now() where id = $1 and org_id = $2 and revoked_at is null returning name, prefix")
            .bind(id)
            .bind(auth.org_id)
            .fetch_optional(&state.db)
            .await?;
    let Some((name, prefix)) = revoked else { return Err(AppError::NotFound) };
    audit::record(&state.db, &auth, "api_key.revoked", Target::new("api_key", id, &name), json!({ "prefix": prefix })).await;
    Ok(Json(json!({ "ok": true })))
}
