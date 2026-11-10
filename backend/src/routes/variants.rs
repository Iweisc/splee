//! A campaign's A/B test: its variants, their results and auto-pick. The logic lives in crate::ab_test.

use std::collections::HashSet;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use super::util_scope;
use crate::ab_test::{self, AbTest, MAX_VARIANTS, MIN_SENDS};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Segment;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/segments/{id}/variants", get(get_test).put(save))
        .route("/segments/{id}/variants/resume", post(resume))
}

const MAX_NAME: usize = 40;
const MAX_INSTRUCTIONS: usize = 1000;
const MAX_PICK_AFTER: i32 = 10_000;

async fn get_test(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<AbTest>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    Ok(Json(ab_test::load_one(&state.db, &segment).await?))
}

#[derive(Deserialize)]
struct VariantBody {
    id: Option<Uuid>,
    name: String,
    #[serde(default)]
    instructions: String,
}

#[derive(Deserialize)]
struct SaveBody {
    variants: Vec<VariantBody>,
    /// Sends per variant before auto-pick keeps a clear winner. Null turns it off.
    pick_after: Option<i32>,
}

impl SaveBody {
    fn validate(&self) -> AppResult<()> {
        let bad = |msg: String| Err(AppError::BadRequest(msg));
        if self.variants.len() > MAX_VARIANTS {
            return bad(format!("A campaign can test up to {MAX_VARIANTS} variants"));
        }
        for v in &self.variants {
            if v.name.trim().is_empty() {
                return bad("Give each variant a name".into());
            }
            if v.name.trim().chars().count() > MAX_NAME {
                return bad(format!("Keep variant names under {MAX_NAME} characters"));
            }
            if v.instructions.trim().chars().count() > MAX_INSTRUCTIONS {
                return bad(format!("Keep each variant's instructions under {MAX_INSTRUCTIONS} characters"));
            }
        }
        if let Some(n) = self.pick_after {
            if self.variants.len() < 2 {
                return bad("Auto-pick needs at least two variants".into());
            }
            if !(MIN_SENDS as i32..=MAX_PICK_AFTER).contains(&n) {
                return bad(format!("Auto-pick needs between {MIN_SENDS} and {MAX_PICK_AFTER} sends per variant"));
            }
        }
        Ok(())
    }
}

/// Replaces the campaign's variants with the list, in its order. Variants left out are deleted, and their leads and emails
/// lose the tag. Emails already written keep their text either way.
async fn save(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<SaveBody>) -> AppResult<Json<AbTest>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    body.validate()?;
    let mut tx = state.db.begin().await?;
    // Auto-pick and assignment lock the campaign row too, so none of them act on a test the others are changing.
    sqlx::query("select 1 from segments where id = $1 for update").bind(segment.id).execute(&mut *tx).await?;
    let existing: HashSet<Uuid> =
        sqlx::query_scalar("select id from segment_variants where segment_id = $1").bind(segment.id).fetch_all(&mut *tx).await?.into_iter().collect();
    if body.variants.iter().any(|v| v.id.is_some_and(|id| !existing.contains(&id))) {
        return Err(AppError::BadRequest("One of the variants no longer exists. Reload the page and try again.".into()));
    }
    let kept: Vec<Uuid> = body.variants.iter().filter_map(|v| v.id).collect();
    sqlx::query("delete from segment_variants where segment_id = $1 and id <> all($2)").bind(segment.id).bind(&kept).execute(&mut *tx).await?;
    for (position, v) in body.variants.iter().enumerate() {
        sqlx::query(
            "insert into segment_variants (id, segment_id, name, instructions, position) values (coalesce($1, gen_random_uuid()), $2, $3, $4, $5)
             on conflict (id) do update set name = excluded.name, instructions = excluded.instructions, position = excluded.position",
        )
        .bind(v.id)
        .bind(segment.id)
        .bind(v.name.trim())
        .bind(v.instructions.trim())
        .bind(position as i32)
        .execute(&mut *tx)
        .await?;
    }
    // A pick whose winner was deleted is void, so the paused variants it left behind are resumed.
    let winner_gone = segment.variant_winner_id.is_some_and(|w| !kept.contains(&w));
    if winner_gone {
        clear_pick(&mut tx, segment.id).await?;
    }
    sqlx::query("update segments set variant_pick_after = $2 where id = $1").bind(segment.id).bind(body.pick_after).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(reload(&state, segment.id).await?))
}

/// Undoes auto-pick: every variant takes new leads again and auto-pick is turned off, so it doesn't pick the same winner
/// again an hour later.
async fn resume(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<AbTest>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("select 1 from segments where id = $1 for update").bind(segment.id).execute(&mut *tx).await?;
    clear_pick(&mut tx, segment.id).await?;
    sqlx::query("update segments set variant_pick_after = null where id = $1").bind(segment.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(reload(&state, segment.id).await?))
}

async fn clear_pick(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, segment_id: Uuid) -> sqlx::Result<()> {
    sqlx::query("update segment_variants set paused_at = null where segment_id = $1").bind(segment_id).execute(&mut **tx).await?;
    sqlx::query("update segments set variant_winner_id = null, variant_picked_at = null, variant_pick_note = '' where id = $1")
        .bind(segment_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn reload(state: &AppState, segment_id: Uuid) -> AppResult<AbTest> {
    let segment = sqlx::query_as::<_, Segment>("select * from segments where id = $1").bind(segment_id).fetch_one(&state.db).await?;
    Ok(ab_test::load_one(&state.db, &segment).await?)
}
