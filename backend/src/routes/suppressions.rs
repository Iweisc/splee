use axum::extract::{Path, Query, State};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::util_input;
use super::util_pagination::{PageQuery, Paginated};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Suppression;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/suppressions", get(list).post(add))
        .route("/suppressions/{id}", delete(remove))
}

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
}

async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Suppression>>> {
    let like = util_input::like_pattern(q.q.as_deref());
    let filter = "from suppressions where org_id = $1 and ($2::text is null or value::text ilike $2)";
    let total: i64 = sqlx::query_scalar(&format!("select count(*) {filter}"))
        .bind(auth.org_id)
        .bind(&like)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, Suppression>(&format!("select * {filter} order by created_at desc limit $3 offset $4"))
        .bind(auth.org_id)
        .bind(&like)
        .bind(page.limit())
        .bind(page.offset())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(Paginated { items, total }))
}

#[derive(Deserialize)]
struct AddBody {
    values: String,
}

async fn add(State(state): State<AppState>, auth: Auth, Json(body): Json<AddBody>) -> AppResult<Json<Value>> {
    let mut values = Vec::new();
    let mut kinds = Vec::new();
    for raw in body.values.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter(|v| !v.is_empty()) {
        let parsed = if raw.contains('@') {
            util_input::email(raw).map(|v| (v, "email"))
        } else {
            util_input::domain(raw).map(|v| (v, "domain"))
        };
        let (value, kind) = parsed.ok_or_else(|| AppError::BadRequest(format!("\"{raw}\" is not an email or a domain")))?;
        values.push(value);
        kinds.push(kind);
    }
    let added = sqlx::query(
        "insert into suppressions (org_id, value, kind, reason)
         select $1, v, k, 'manual' from unnest($2::text[], $3::text[]) as u(v, k)
         on conflict (org_id, value) do nothing",
    )
    .bind(auth.org_id)
    .bind(&values)
    .bind(&kinds)
    .execute(&state.db)
    .await?
    .rows_affected();
    Ok(Json(json!({ "added": added })))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let deleted = sqlx::query("delete from suppressions where id = $1 and org_id = $2")
        .bind(id)
        .bind(auth.org_id)
        .execute(&state.db)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}
