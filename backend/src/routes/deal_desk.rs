//! Deal desk routes: the organization's price list and discount limit, asking for a held proposal's discount to be
//! approved, and owners and admins deciding. See crate::deal_desk.

use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::auth::Auth;
use crate::deal_desk::{self, Approval};
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::proposals::{self, Proposal};
use crate::state::AppState;

/// Products on the price list, so the proposal editor's picker stays short.
const MAX_PRODUCTS: i64 = 100;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/products", get(list).post(add))
        .route("/products/{id}", put(edit).delete(archive))
        .route("/deal-desk/limit", put(limit))
        .route("/discount-approvals", get(waiting))
        .route("/discount-approvals/{id}", post(decide))
        .route("/deals/{id}/proposal/approval", post(ask))
}

#[derive(Serialize, FromRow)]
struct Product {
    id: Uuid,
    name: String,
    detail: String,
    price: f64,
    period: String,
}

#[derive(Serialize)]
struct PriceList {
    currency: String,
    discount_limit_pct: Option<i32>,
    products: Vec<Product>,
}

async fn price_list(state: &AppState, org_id: Uuid) -> AppResult<Json<PriceList>> {
    let (currency, discount_limit_pct): (String, Option<i32>) =
        sqlx::query_as("select currency, discount_limit_pct from organizations where id = $1").bind(org_id).fetch_one(&state.db).await?;
    let products = sqlx::query_as("select id, name, detail, price::float8 as price, period from products where org_id = $1 and archived_at is null order by name")
        .bind(org_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(PriceList { currency, discount_limit_pct, products }))
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<PriceList>> {
    price_list(&state, auth.org_id).await
}

#[derive(Deserialize)]
struct NewProduct {
    name: String,
    #[serde(default)]
    detail: String,
    price: f64,
    period: String,
}

fn tidy(p: NewProduct) -> AppResult<NewProduct> {
    let name = p.name.trim().chars().take(120).collect::<String>();
    if name.is_empty() {
        return Err(AppError::BadRequest("Give the product a name".into()));
    }
    if !["one_time", "month", "year"].contains(&p.period.as_str()) {
        return Err(AppError::BadRequest("period is one_time, month or year".into()));
    }
    if !p.price.is_finite() || !(0.0..1e9).contains(&p.price) {
        return Err(AppError::BadRequest("The price is zero or more".into()));
    }
    Ok(NewProduct { name, detail: p.detail.trim().chars().take(300).collect(), price: (p.price * 100.0).round() / 100.0, period: p.period })
}

async fn add(State(state): State<AppState>, auth: Auth, Json(body): Json<NewProduct>) -> AppResult<Json<PriceList>> {
    auth.require_admin()?;
    let p = tidy(body)?;
    let count: i64 = sqlx::query_scalar("select count(*) from products where org_id = $1 and archived_at is null").bind(auth.org_id).fetch_one(&state.db).await?;
    if count >= MAX_PRODUCTS {
        return Err(AppError::BadRequest(format!("The price list holds up to {MAX_PRODUCTS} products")));
    }
    sqlx::query("insert into products (org_id, name, detail, price, period) values ($1, $2, $3, $4::numeric, $5)")
        .bind(auth.org_id)
        .bind(&p.name)
        .bind(&p.detail)
        .bind(p.price)
        .bind(&p.period)
        .execute(&state.db)
        .await?;
    price_list(&state, auth.org_id).await
}

/// A new price counts for proposals saved from now on. Ones already saved keep the list price they were saved with.
async fn edit(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<NewProduct>) -> AppResult<Json<PriceList>> {
    auth.require_admin()?;
    let p = tidy(body)?;
    let done = sqlx::query("update products set name = $3, detail = $4, price = $5::numeric, period = $6 where id = $1 and org_id = $2 and archived_at is null")
        .bind(id)
        .bind(auth.org_id)
        .bind(&p.name)
        .bind(&p.detail)
        .bind(p.price)
        .bind(&p.period)
        .execute(&state.db)
        .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    price_list(&state, auth.org_id).await
}

/// Takes it off the price list. Proposals that use it keep it.
async fn archive(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<PriceList>> {
    auth.require_admin()?;
    let done = sqlx::query("update products set archived_at = now() where id = $1 and org_id = $2 and archived_at is null")
        .bind(id)
        .bind(auth.org_id)
        .execute(&state.db)
        .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    price_list(&state, auth.org_id).await
}

#[derive(Deserialize)]
struct Limit {
    discount_limit_pct: Option<i32>,
}

async fn limit(State(state): State<AppState>, auth: Auth, Json(body): Json<Limit>) -> AppResult<Json<PriceList>> {
    auth.require_admin()?;
    if body.discount_limit_pct.is_some_and(|l| !(0..=100).contains(&l)) {
        return Err(AppError::BadRequest("The limit is between 0% and 100%".into()));
    }
    sqlx::query("update organizations set discount_limit_pct = $2 where id = $1").bind(auth.org_id).bind(body.discount_limit_pct).execute(&state.db).await?;
    price_list(&state, auth.org_id).await
}

/// Asks waiting on a decision, for owners and admins' Today. Everyone else gets none.
async fn waiting(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<Approval>>> {
    if auth.require_admin().is_err() {
        return Ok(Json(vec![]));
    }
    Ok(Json(deal_desk::waiting(&state.db, auth.org_id).await?))
}

#[derive(Deserialize)]
struct AskBody {
    #[serde(default)]
    reason: String,
}

async fn ask(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<AskBody>) -> AppResult<Json<Option<Proposal>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    let run = deal_desk::ask(&state, auth.org_id, deal.id, auth.user_id, &body.reason).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(proposals::for_deal(&state.db, &state.cfg.public_url, deal.id).await?))
}

#[derive(Deserialize)]
struct Decision {
    approve: bool,
    #[serde(default)]
    note: String,
}

async fn decide(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Decision>) -> AppResult<Json<Option<Proposal>>> {
    auth.require_admin()?;
    let proposal_id = deal_desk::decide(&state, auth.org_id, id, auth.user_id, body.approve, &body.note).await?;
    let deal_id: Uuid = sqlx::query_scalar("select deal_id from proposals where id = $1").bind(proposal_id).fetch_one(&state.db).await?;
    Ok(Json(proposals::for_deal(&state.db, &state.cfg.public_url, deal_id).await?))
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn admins_keep_the_price_list_and_limit_and_members_only_read_them(db: PgPool) {
        use crate::agent::practice::fixture;
        let t = fixture::team(&db).await;
        let state = crate::autopilot::tests::state(db.clone());
        let auth = |user: Uuid, role: &str| Auth { user_id: user, org_id: t.org, role: role.into(), token_hash: String::new() };
        let seats = || NewProduct { name: " Seats ".into(), detail: String::new(), price: 199.999, period: "month".into() };

        assert!(add(State(state.clone()), auth(t.sam, "member"), Json(seats())).await.is_err(), "members can't add");
        assert!(add(State(state.clone()), auth(t.dana, "owner"), Json(NewProduct { period: "week".into(), ..seats() })).await.is_err());
        let Json(v) = add(State(state.clone()), auth(t.dana, "owner"), Json(seats())).await.unwrap();
        assert_eq!((v.products[0].name.as_str(), v.products[0].price), ("Seats", 200.0));
        let id = v.products[0].id;
        let Json(v) = edit(State(state.clone()), auth(t.dana, "admin"), Path(id), Json(NewProduct { price: 180.0, ..seats() })).await.unwrap();
        assert_eq!(v.products[0].price, 180.0);
        assert!(limit(State(state.clone()), auth(t.sam, "member"), Json(Limit { discount_limit_pct: Some(10) })).await.is_err());
        assert!(limit(State(state.clone()), auth(t.dana, "owner"), Json(Limit { discount_limit_pct: Some(120) })).await.is_err());
        let Json(v) = limit(State(state.clone()), auth(t.dana, "owner"), Json(Limit { discount_limit_pct: Some(15) })).await.unwrap();
        assert_eq!(v.discount_limit_pct, Some(15));
        let Json(v) = archive(State(state.clone()), auth(t.dana, "owner"), Path(id)).await.unwrap();
        assert!(v.products.is_empty());
        assert!(waiting(State(state), auth(t.sam, "member")).await.unwrap().0.is_empty());
    }
}
