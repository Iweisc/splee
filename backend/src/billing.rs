use sqlx::PgPool;
use uuid::Uuid;

use crate::state::AppState;

/// Pay-as-you-go pricing from the Explee pricing page: $0.04 per sent email, $30 free credits.
pub const EMAIL_COST_MICROS: i64 = 40_000;
pub const SIGNUP_BONUS_MICROS: i64 = 30_000_000;
pub const REFERRAL_BONUS_MICROS: i64 = 30_000_000;
/// A referrer is rewarded once the referred org has spent this much on sent emails.
pub const REFERRAL_QUALIFYING_SPEND_MICROS: i64 = 10_000_000;

pub async fn balance(db: &PgPool, org_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar("select coalesce(sum(amount_micros), 0)::bigint from credit_ledger where org_id = $1")
        .bind(org_id)
        .fetch_one(db)
        .await
}

/// Balance that sending may use. With payments disabled it's effectively unlimited.
pub async fn spendable(state: &AppState, org_id: Uuid) -> sqlx::Result<i64> {
    if !state.cfg.payments_enabled {
        return Ok(i64::MAX / 2);
    }
    balance(&state.db, org_id).await
}

/// Spend on sent emails today, in UTC. Returned as a positive number.
pub async fn spent_today(db: &PgPool, org_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "select coalesce(-sum(amount_micros), 0)::bigint from credit_ledger
         where org_id = $1 and kind = 'email_sent' and created_at >= date_trunc('day', now())",
    )
    .bind(org_id)
    .fetch_one(db)
    .await
}

pub async fn add_entry<'e, E>(
    exec: E,
    org_id: Uuid,
    amount_micros: i64,
    kind: &str,
    ref_id: Option<Uuid>,
    note: &str,
) -> sqlx::Result<()>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query("insert into credit_ledger (org_id, amount_micros, kind, ref_id, note) values ($1, $2, $3, $4, $5)")
        .bind(org_id)
        .bind(amount_micros)
        .bind(kind)
        .bind(ref_id)
        .bind(note)
        .execute(exec)
        .await?;
    Ok(())
}
