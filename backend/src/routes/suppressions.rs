use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::util_csv;
use super::util_input;
use super::util_pagination::{PageQuery, Paginated};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Suppression;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/suppressions", get(list).post(add))
        .route("/suppressions/export", get(export))
        .route("/suppressions/{id}", delete(remove))
}

const FILTER: &str = "from suppressions where org_id = $1 and ($2::text is null or value::text ilike $2)";

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
    let total: i64 = sqlx::query_scalar(&format!("select count(*) {FILTER}"))
        .bind(auth.org_id)
        .bind(&like)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, Suppression>(&format!("select * {FILTER} order by created_at desc limit $3 offset $4"))
        .bind(auth.org_id)
        .bind(&like)
        .bind(page.limit())
        .bind(page.offset())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(Paginated { items, total }))
}

/// Every entry matching the search, newest first, as a CSV download for other sending tools or a record of opt-outs.
async fn export(State(state): State<AppState>, auth: Auth, Query(q): Query<ListQuery>) -> AppResult<Response> {
    let like = util_input::like_pattern(q.q.as_deref());
    let rows = sqlx::query_as::<_, Suppression>(&format!("select * {FILTER} order by created_at desc"))
        .bind(auth.org_id)
        .bind(&like)
        .fetch_all(&state.db)
        .await?;
    let records = rows.iter().map(|s| [s.value.clone(), s.kind.clone(), s.reason.clone(), s.created_at.to_rfc3339()]);
    util_csv::download("do-not-contact.csv", &["value", "kind", "reason", "created_at"], records)
}

#[derive(Deserialize)]
struct AddBody {
    values: String,
}

async fn add(State(state): State<AppState>, auth: Auth, Json(body): Json<AddBody>) -> AppResult<Json<Value>> {
    let raws = body.values.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter(|v| !v.is_empty());
    let added = insert(&state.db, auth.org_id, raws).await?;
    Ok(Json(json!({ "added": added })))
}

/// Adds emails, domains and phone numbers to the org's suppression list and returns how many were new. A phone number has to
/// start with + or 00, since nothing says which country it's in. One value that is none of these rejects the whole batch,
/// naming it.
pub async fn insert<'a>(db: &sqlx::PgPool, org_id: Uuid, raws: impl IntoIterator<Item = &'a str>) -> AppResult<u64> {
    let mut values = Vec::new();
    let mut kinds = Vec::new();
    for raw in raws {
        let parsed = if raw.contains('@') {
            util_input::email(raw).map(|v| (v, "email"))
        } else if raw.starts_with('+') || raw.starts_with("00") {
            crate::phone::normalize(raw, "").map(|v| (v, "phone"))
        } else {
            util_input::domain(raw).map(|v| (v, "domain"))
        };
        let (value, kind) = parsed.ok_or_else(|| AppError::BadRequest(format!("\"{raw}\" is not an email, a domain or a phone number with its country code")))?;
        values.push(value);
        kinds.push(kind);
    }
    let added = sqlx::query(
        "insert into suppressions (org_id, value, kind, reason)
         select $1, v, k, 'manual' from unnest($2::text[], $3::text[]) as u(v, k)
         on conflict (org_id, value) do nothing",
    )
    .bind(org_id)
    .bind(&values)
    .bind(&kinds)
    .execute(db)
    .await?
    .rows_affected();
    Ok(added)
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
