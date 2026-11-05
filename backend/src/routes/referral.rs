use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use sqlx::FromRow;

use crate::auth::Auth;
use crate::error::AppResult;
use crate::models::Ts;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/referral", get(get_referral))
}

#[derive(Serialize)]
struct Referral {
    code: String,
    link: String,
    referred: Vec<Referred>,
    earned_micros: i64,
}

#[derive(Serialize, FromRow)]
struct Referred {
    org_name: String,
    created_at: Ts,
    rewarded: bool,
}

async fn get_referral(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Referral>> {
    let code: String = sqlx::query_scalar("select referral_code from organizations where id = $1")
        .bind(auth.org_id)
        .fetch_one(&state.db)
        .await?;
    let referred = sqlx::query_as::<_, Referred>(
        "select name as org_name, created_at, referral_rewarded as rewarded from organizations
         where referred_by = $1 order by created_at desc",
    )
    .bind(auth.org_id)
    .fetch_all(&state.db)
    .await?;
    let earned_micros: i64 = sqlx::query_scalar(
        "select coalesce(sum(amount_micros), 0)::bigint from credit_ledger where org_id = $1 and kind = 'referral_bonus'",
    )
    .bind(auth.org_id)
    .fetch_one(&state.db)
    .await?;
    let link = format!("{}/signup?ref={code}", state.cfg.public_url);
    Ok(Json(Referral { code, link, referred, earned_micros }))
}
