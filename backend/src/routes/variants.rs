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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};
    use sqlx::PgPool;

    use super::*;
    use crate::ab_test::tests::{Campaign, add_leads, seed};
    use crate::config::Config;

    fn state(db: PgPool) -> AppState {
        AppState { db, cfg: Arc::new(Config::test()), events: tokio::sync::broadcast::channel(16).0, rate_limits: Default::default() }
    }

    fn owner(org_id: Uuid) -> Auth {
        Auth { user_id: Uuid::new_v4(), org_id, role: "owner".into(), token_hash: String::new() }
    }

    async fn org_of(db: &PgPool, c: &Campaign) -> Uuid {
        sqlx::query_scalar("select org_id from projects where id = $1").bind(c.project).fetch_one(db).await.unwrap()
    }

    async fn put(state: &AppState, org: Uuid, segment: Uuid, body: Value) -> AppResult<AbTest> {
        let body: SaveBody = serde_json::from_value(body).unwrap();
        save(State(state.clone()), owner(org), Path(segment), Json(body)).await.map(|Json(t)| t)
    }

    /// The campaign's variants as saved, in order, and its auto-pick.
    async fn saved(db: &PgPool, segment: Uuid) -> (Vec<(Uuid, String, String, i32, bool)>, Option<i32>, Option<Uuid>) {
        let variants = sqlx::query_as("select id, name, instructions, position, paused_at is not null from segment_variants where segment_id = $1 order by position")
            .bind(segment)
            .fetch_all(db)
            .await
            .unwrap();
        let (after, winner) = sqlx::query_as("select variant_pick_after, variant_winner_id from segments where id = $1").bind(segment).fetch_one(db).await.unwrap();
        (variants, after, winner)
    }

    /// Auto-pick as it leaves a campaign that picked `winner`: the others paused, and a note.
    async fn picked(db: &PgPool, segment: Uuid, winner: Uuid) {
        sqlx::query("update segment_variants set paused_at = now() where segment_id = $1 and id <> $2").bind(segment).bind(winner).execute(db).await.unwrap();
        sqlx::query("update segments set variant_pick_after = 100, variant_winner_id = $2, variant_picked_at = now(), variant_pick_note = 'B won' where id = $1")
            .bind(segment)
            .bind(winner)
            .execute(db)
            .await
            .unwrap();
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn saving_variants_refuses_a_bad_list_and_changes_nothing(db: PgPool) {
        let state = state(db.clone());
        let c = seed(&db, 2).await;
        let org = org_of(&db, &c).await;
        let other = seed(&db, 1).await;
        let (a, b) = (c.variants[0], c.variants[1]);
        let before = saved(&db, c.segment).await;
        let named = |name: &str| json!({ "name": name });
        let refused = [
            (json!({ "variants": [named("A"), named("B"), named("C"), named("D"), named("E")] }), "A campaign can test up to 4 variants"),
            (json!({ "variants": [named("A"), named("  ")] }), "Give each variant a name"),
            (json!({ "variants": [named(&"x".repeat(41))] }), "Keep variant names under 40 characters"),
            (json!({ "variants": [{ "name": "A", "instructions": "x".repeat(1001) }] }), "Keep each variant's instructions under 1000 characters"),
            (json!({ "variants": [{ "id": a, "name": "A" }], "pick_after": 50 }), "Auto-pick needs at least two variants"),
            (json!({ "variants": [named("A"), named("B")], "pick_after": 29 }), "Auto-pick needs between 30 and 10000 sends per variant"),
            (json!({ "variants": [named("A"), named("B")], "pick_after": 10_001 }), "Auto-pick needs between 30 and 10000 sends per variant"),
            (
                json!({ "variants": [{ "id": a, "name": "A" }, { "id": other.variants[0], "name": "Theirs" }] }),
                "One of the variants no longer exists. Reload the page and try again.",
            ),
        ];
        for (body, want) in refused {
            match put(&state, org, c.segment, body.clone()).await {
                Err(AppError::BadRequest(m)) => assert_eq!(m, want, "{body}"),
                other => panic!("{body}: {:?}", other.map(|t| t.variants.len())),
            }
            assert_eq!(saved(&db, c.segment).await, before, "{body}");
        }
        let theirs = org_of(&db, &other).await;
        assert!(matches!(put(&state, theirs, c.segment, json!({ "variants": [] })).await, Err(AppError::NotFound)));
        assert_eq!(saved(&db, c.segment).await, before);

        // Both ends of each limit are fine, and names and instructions are trimmed.
        let long = "x".repeat(40);
        let body = json!({ "variants": [{ "id": a, "name": format!(" {long} "), "instructions": format!("{} ", "y".repeat(1000)) }, { "id": b, "name": "B" }], "pick_after": 30 });
        let test = put(&state, org, c.segment, body).await.unwrap();
        assert_eq!((test.variants[0].variant.name.as_str(), test.variants[0].variant.instructions.len(), test.pick_after), (long.as_str(), 1000, Some(30)));
        let four = json!({ "variants": [{ "id": a, "name": "A" }, { "id": b, "name": "B" }, named("C"), named("D")], "pick_after": 10_000 });
        let test = put(&state, org, c.segment, four).await.unwrap();
        assert_eq!((test.variants.len(), test.pick_after), (4, Some(10_000)));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn saving_keeps_the_listed_variants_in_order_and_drops_the_rest(db: PgPool) {
        let state = state(db.clone());
        let c = seed(&db, 3).await;
        let org = org_of(&db, &c).await;
        let (a, b, v3) = (c.variants[0], c.variants[1], c.variants[2]);
        let leads = add_leads(&db, &c, 3).await;
        for (lead, variant) in leads.iter().zip([a, b, v3]) {
            sqlx::query("update leads set variant_id = $2 where id = $1").bind(lead.id).bind(variant).execute(&db).await.unwrap();
            sqlx::query(
                "insert into email_messages (project_id, lead_id, segment_id, variant_id, direction, kind, status, subject, body)
                 values ($1, $2, $3, $4, 'outbound', 'initial', 'sent', 'Quick question', 'Written for ' || $2::text)",
            )
            .bind(c.project)
            .bind(lead.id)
            .bind(c.segment)
            .bind(variant)
            .execute(&db)
            .await
            .unwrap();
        }

        let test = put(&state, org, c.segment, json!({ "variants": [{ "id": v3, "name": " C2 ", "instructions": " Short " }, { "name": "New" }, { "id": a, "name": "A" }], "pick_after": 50 }))
            .await
            .unwrap();
        assert_eq!(test.variants.iter().map(|v| v.variant.name.as_str()).collect::<Vec<_>>(), ["C2", "New", "A"]);
        // The list replaces what was saved, so A sent without instructions loses them.
        let (variants, after, _) = saved(&db, c.segment).await;
        let new = variants[1].0;
        assert!(![a, b, v3].contains(&new));
        assert_eq!(variants, [(v3, "C2".into(), "Short".into(), 0, false), (new, "New".into(), "".into(), 1, false), (a, "A".into(), "".into(), 2, false)]);
        assert_eq!(after, Some(50));
        for (lead, tag) in leads.iter().zip([Some(a), None, Some(v3)]) {
            let saved: Option<Uuid> = sqlx::query_scalar("select variant_id from leads where id = $1").bind(lead.id).fetch_one(&db).await.unwrap();
            let (email, body): (Option<Uuid>, String) =
                sqlx::query_as("select variant_id, body from email_messages where lead_id = $1").bind(lead.id).fetch_one(&db).await.unwrap();
            assert_eq!((saved, email), (tag, tag), "the dropped variant's lead and email lose its tag");
            assert_eq!(body, format!("Written for {}", lead.id), "the email keeps its text");
        }

        // While auto-pick's winner stays, so does its pick. Dropping the winner voids the pick and resumes the others.
        picked(&db, c.segment, a).await;
        let Json(before) = get_test(State(state.clone()), owner(org), Path(c.segment)).await.unwrap();
        let kept = put(&state, org, c.segment, json!({ "variants": [{ "id": a, "name": "A" }, { "id": v3, "name": "C2" }, { "id": new, "name": "New" }], "pick_after": 100 }))
            .await
            .unwrap();
        assert!(before.picked_at.is_some());
        assert_eq!((before.winner_id, before.pick_note.as_str(), before.pick_after), (Some(a), "B won", Some(100)));
        assert_eq!(
            (kept.winner_id, kept.picked_at, kept.pick_note.as_str(), kept.pick_after),
            (before.winner_id, before.picked_at, before.pick_note.as_str(), before.pick_after)
        );
        let (variants, _, winner) = saved(&db, c.segment).await;
        assert_eq!((winner, variants.iter().map(|v| v.4).collect::<Vec<_>>()), (Some(a), vec![false, true, true]));
        let test = put(&state, org, c.segment, json!({ "variants": [{ "id": v3, "name": "C2" }, { "id": new, "name": "New" }] })).await.unwrap();
        let (variants, after, winner) = saved(&db, c.segment).await;
        assert_eq!((winner, after, variants.iter().map(|v| v.4).collect::<Vec<_>>()), (None, None, vec![false, false]));
        assert_eq!((test.winner_id, test.picked_at, test.pick_note.as_str()), (None, None, ""));

        // An empty list ends the test.
        assert!(put(&state, org, c.segment, json!({ "variants": [] })).await.unwrap().variants.is_empty());
        assert_eq!(saved(&db, c.segment).await, (vec![], None, None));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn resuming_undoes_auto_pick_and_turns_it_off(db: PgPool) {
        let state = state(db.clone());
        let c = seed(&db, 3).await;
        let org = org_of(&db, &c).await;
        let theirs = org_of(&db, &seed(&db, 1).await).await;
        picked(&db, c.segment, c.variants[1]).await;

        let Json(shown) = get_test(State(state.clone()), owner(org), Path(c.segment)).await.unwrap();
        assert_eq!((shown.winner_id, shown.pick_after, shown.pick_note.as_str()), (Some(c.variants[1]), Some(100), "B won"));
        assert!(shown.picked_at.is_some());
        assert!(matches!(get_test(State(state.clone()), owner(theirs), Path(c.segment)).await, Err(AppError::NotFound)));
        assert!(matches!(resume(State(state.clone()), owner(theirs), Path(c.segment)).await, Err(AppError::NotFound)));
        assert_eq!(saved(&db, c.segment).await.2, Some(c.variants[1]), "another org can't resume it");

        let Json(resumed) = resume(State(state.clone()), owner(org), Path(c.segment)).await.unwrap();
        assert_eq!((resumed.winner_id, resumed.picked_at, resumed.pick_note.as_str(), resumed.pick_after), (None, None, "", None));
        assert!(resumed.variants.iter().all(|v| v.variant.paused_at.is_none()));
        let (variants, after, winner) = saved(&db, c.segment).await;
        assert_eq!((variants.len(), after, winner), (3, None, None), "the variants stay, and auto-pick is off");
    }
}
