//! The Plays page: a project's plays, what each one did lately, and turning them on, off or changing them. See crate::plays.

use axum::extract::{Path, State};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::util_input;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::plays::{ACTIONS, MAX_PLAYS, TRIGGERS, fits};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/plays", get(list).post(create))
        .route("/plays/{id}", patch(update).delete(remove))
}

#[derive(Serialize, FromRow)]
pub struct PlayRow {
    id: Uuid,
    project_id: Uuid,
    name: String,
    trigger: String,
    action: String,
    segment_id: Option<Uuid>,
    segment_name: Option<String>,
    min_score: i32,
    daily_cap: i32,
    enabled: bool,
    checked_at: Ts,
    created_at: Ts,
    /// Times it fired in the last 24 hours and the last 30 days, skips included.
    fired_today: i64,
    fired_month: i64,
}

#[derive(Serialize, FromRow)]
pub struct Firing {
    id: Uuid,
    play_id: Uuid,
    play_name: String,
    domain: Option<String>,
    lead_id: Option<Uuid>,
    what: String,
    outcome: String,
    note: String,
    run_id: Option<Uuid>,
    created_at: Ts,
}

#[derive(Serialize)]
pub struct Plays {
    plays: Vec<PlayRow>,
    /// The 50 latest firings across the project's plays.
    firings: Vec<Firing>,
}

const ROWS: &str = "select p.id, p.project_id, p.name, p.trigger, p.action, p.segment_id, s.name as segment_name, p.min_score, p.daily_cap, p.enabled,
       p.checked_at, p.created_at,
       (select count(*) from play_firings f where f.play_id = p.id and f.created_at > now() - interval '1 day') as fired_today,
       (select count(*) from play_firings f where f.play_id = p.id and f.created_at > now() - interval '30 days') as fired_month
     from plays p left join segments s on s.id = p.segment_id";

pub async fn plays(db: &PgPool, project_id: Uuid) -> sqlx::Result<Plays> {
    let plays = sqlx::query_as::<_, PlayRow>(&format!("{ROWS} where p.project_id = $1 order by p.created_at")).bind(project_id).fetch_all(db).await?;
    let firings = sqlx::query_as::<_, Firing>(
        "select f.id, f.play_id, p.name as play_name, f.domain::text as domain, f.lead_id, f.what, f.outcome, f.note, f.run_id, f.created_at
         from play_firings f join plays p on p.id = f.play_id where f.project_id = $1 order by f.created_at desc limit 50",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    Ok(Plays { plays, firings })
}

async fn one(db: &PgPool, id: Uuid) -> sqlx::Result<PlayRow> {
    sqlx::query_as::<_, PlayRow>(&format!("{ROWS} where p.id = $1")).bind(id).fetch_one(db).await
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Plays>> {
    auth.project(&state.db, id).await?;
    Ok(Json(plays(&state.db, id).await?))
}

#[derive(Deserialize, Default)]
pub struct PlayBody {
    name: Option<String>,
    trigger: Option<String>,
    action: Option<String>,
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    segment_id: Option<Uuid>,
    min_score: Option<i32>,
    daily_cap: Option<i32>,
    enabled: Option<bool>,
    /// On create: also act on what happened in the last this many days, up to 30. Only what happens from now on without it.
    backfill_days: Option<i32>,
}

/// A play as it would be saved.
#[derive(Clone, Copy)]
struct Rule<'a> {
    name: &'a str,
    trigger: &'a str,
    action: &'a str,
    segment_id: Option<Uuid>,
    min_score: i32,
    daily_cap: i32,
}

/// Checks the whole play as it would be saved: the trigger and action fit, a campaign play has a campaign of this project.
async fn check(db: &PgPool, project_id: Uuid, r: &Rule<'_>) -> AppResult<()> {
    let Rule { name, trigger, action, segment_id, min_score, daily_cap } = *r;
    if name.trim().is_empty() || name.chars().count() > 80 {
        return Err(AppError::BadRequest("Give the play a name of at most 80 characters".into()));
    }
    util_input::one_of("trigger", trigger, &TRIGGERS)?;
    util_input::one_of("action", action, &ACTIONS)?;
    if !fits(trigger, action) {
        let why = if action == "add_to_campaign" { "Only companies go into a campaign" } else { "Only leads get emails" };
        return Err(AppError::BadRequest(format!("{why}, so this trigger can't take that action")));
    }
    if !(0..=100).contains(&min_score) || !(1..=200).contains(&daily_cap) {
        return Err(AppError::BadRequest("min_score is 0 to 100 and daily_cap 1 to 200".into()));
    }
    match segment_id {
        Some(id) => {
            let ours: bool = sqlx::query_scalar("select exists (select 1 from segments where id = $1 and project_id = $2)").bind(id).bind(project_id).fetch_one(db).await?;
            if !ours {
                return Err(AppError::BadRequest("That campaign isn't part of this project".into()));
            }
        }
        None if action == "add_to_campaign" => return Err(AppError::BadRequest("Pick the campaign to add companies to".into())),
        None => {}
    }
    Ok(())
}

async fn create(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PlayBody>) -> AppResult<Json<PlayRow>> {
    auth.project(&state.db, id).await?;
    let (name, trigger, action) = (body.name.unwrap_or_default(), body.trigger.unwrap_or_default(), body.action.unwrap_or_default());
    let (min_score, daily_cap) = (body.min_score.unwrap_or(0), body.daily_cap.unwrap_or(20));
    check(&state.db, id, &Rule { name: &name, trigger: &trigger, action: &action, segment_id: body.segment_id, min_score, daily_cap }).await?;
    let count: i64 = sqlx::query_scalar("select count(*) from plays where project_id = $1").bind(id).fetch_one(&state.db).await?;
    if count >= MAX_PLAYS {
        return Err(AppError::BadRequest(format!("A project can have up to {MAX_PLAYS} plays")));
    }
    let backfill = body.backfill_days.unwrap_or(0).clamp(0, 30);
    let play: Uuid = sqlx::query_scalar(
        "insert into plays (project_id, name, trigger, action, segment_id, min_score, daily_cap, enabled, checked_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, now() - make_interval(days => $9)) returning id",
    )
    .bind(id)
    .bind(name.trim())
    .bind(&trigger)
    .bind(&action)
    .bind(body.segment_id)
    .bind(min_score)
    .bind(daily_cap)
    .bind(body.enabled.unwrap_or(true))
    .bind(backfill)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(one(&state.db, play).await?))
}

async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PlayBody>) -> AppResult<Json<PlayRow>> {
    let play: crate::plays::Play = super::util_scope::load(&state.db, "plays", id, auth.org_id).await?;
    let name = body.name.unwrap_or(play.name);
    let segment_id = body.segment_id.or(play.segment_id);
    let (min_score, daily_cap) = (body.min_score.unwrap_or(play.min_score), body.daily_cap.unwrap_or(play.daily_cap));
    // The trigger and action stay: a different rule is a different play, with its own history.
    check(&state.db, play.project_id, &Rule { name: &name, trigger: &play.trigger, action: &play.action, segment_id, min_score, daily_cap }).await?;
    // Turning a play back on starts from now, so it doesn't act on everything that happened while it was off.
    sqlx::query(
        "update plays set name = $2, segment_id = $3, min_score = $4, daily_cap = $5, enabled = coalesce($6, enabled),
                          checked_at = case when $6 and not enabled then now() else checked_at end
         where id = $1",
    )
    .bind(id)
    .bind(name.trim())
    .bind(segment_id)
    .bind(min_score)
    .bind(daily_cap)
    .bind(body.enabled)
    .execute(&state.db)
    .await?;
    Ok(Json(one(&state.db, id).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let play: crate::plays::Play = super::util_scope::load(&state.db, "plays", id, auth.org_id).await?;
    sqlx::query("delete from plays where id = $1").bind(play.id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{Duration, Utc};

    use super::*;
    use crate::config::Config;

    struct Seeded {
        state: AppState,
        org: Uuid,
        project: Uuid,
        segment: Uuid,
        /// Another org's project and campaign.
        other_org: Uuid,
        other_project: Uuid,
        other_segment: Uuid,
    }

    async fn seed(db: PgPool) -> Seeded {
        let mut ids = Vec::new();
        for name in ["Acme", "Other"] {
            let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap();
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(&db).await.unwrap();
            let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Funded') returning id").bind(project).fetch_one(&db).await.unwrap();
            ids.push((org, project, segment));
        }
        let [(org, project, segment), (other_org, other_project, other_segment)] = ids[..] else { unreachable!() };
        let state = AppState { db, cfg: Arc::new(Config::test()), events: tokio::sync::broadcast::channel(16).0, rate_limits: Default::default() };
        Seeded { state, org, project, segment, other_org, other_project, other_segment }
    }

    fn owner(org_id: Uuid) -> Auth {
        Auth { user_id: Uuid::new_v4(), org_id, role: "owner".into(), token_hash: String::new() }
    }

    fn body(value: Value) -> Json<PlayBody> {
        Json(serde_json::from_value(value).unwrap())
    }

    async fn make(s: &Seeded, org: Uuid, project: Uuid, value: Value) -> AppResult<PlayRow> {
        create(State(s.state.clone()), owner(org), Path(project), body(value)).await.map(|Json(row)| row)
    }

    async fn edit(s: &Seeded, org: Uuid, id: Uuid, value: Value) -> AppResult<PlayRow> {
        update(State(s.state.clone()), owner(org), Path(id), body(value)).await.map(|Json(row)| row)
    }

    async fn count(s: &Seeded) -> i64 {
        sqlx::query_scalar("select count(*) from plays").fetch_one(&s.state.db).await.unwrap()
    }

    /// Whether `at` is within a minute of `days` days ago.
    fn about(at: Ts, days: i64) -> bool {
        (at - (Utc::now() - Duration::days(days))).num_seconds().abs() < 60
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_play_is_saved_only_when_its_whole_rule_holds(db: PgPool) {
        let s = seed(db).await;
        let good = json!({ "name": "Raised money", "trigger": "funding", "action": "add_to_campaign", "segment_id": s.segment });
        let with = |change: Value| {
            let mut value = good.clone();
            value.as_object_mut().unwrap().extend(change.as_object().unwrap().clone());
            value
        };
        let refused = [
            with(json!({ "name": "  " })),
            with(json!({ "name": "x".repeat(81) })),
            json!({ "name": "Raised money", "action": "notify" }),
            with(json!({ "trigger": "rain" })),
            with(json!({ "action": "call" })),
            with(json!({ "action": "write_email" })),
            json!({ "name": "Hot lead", "trigger": "score", "action": "add_to_campaign", "segment_id": s.segment }),
            with(json!({ "min_score": -1 })),
            with(json!({ "min_score": 101 })),
            with(json!({ "daily_cap": 0 })),
            with(json!({ "daily_cap": 201 })),
            with(json!({ "segment_id": null })),
            with(json!({ "segment_id": "" })),
            with(json!({ "segment_id": s.other_segment })),
            with(json!({ "segment_id": Uuid::new_v4() })),
        ];
        for value in refused {
            let result = make(&s, s.org, s.project, value.clone()).await;
            assert!(matches!(result, Err(AppError::BadRequest(_))), "{value}: {:?}", result.map(|p| p.name));
        }
        assert!(matches!(make(&s, s.org, s.other_project, good.clone()).await, Err(AppError::NotFound)), "another org's project");
        assert_eq!(count(&s).await, 0);

        // Left out, a play is on, catches any score up to 20 a day and acts only on what happens from now on.
        let play = make(&s, s.org, s.project, with(json!({ "name": "  Raised money  " }))).await.unwrap();
        assert_eq!((play.name.as_str(), play.segment_name.as_deref(), play.project_id), ("Raised money", Some("Funded"), s.project));
        assert_eq!((play.min_score, play.daily_cap, play.enabled, play.fired_today, play.fired_month), (0, 20, true, 0, 0));
        assert!(about(play.checked_at, 0), "{}", play.checked_at);

        // A notify play needs no campaign, and both ends of each range are fine.
        let edges = [(0, 1, "n".repeat(80), 30, 30), (100, 200, "Visit".into(), 45, 30), (50, 5, "Old visit".into(), -3, 0)];
        for (min_score, daily_cap, name, backfill_days, days) in edges {
            let value = json!({ "name": name, "trigger": "visit", "action": "notify", "min_score": min_score, "daily_cap": daily_cap,
                                "backfill_days": backfill_days, "enabled": false });
            let play = make(&s, s.org, s.project, value).await.unwrap();
            assert_eq!((play.min_score, play.daily_cap, play.name, play.enabled, play.segment_id), (min_score, daily_cap, name, false, None));
            assert!(about(play.checked_at, days), "backfill {backfill_days} checked from {}", play.checked_at);
        }

        // Up to 20 plays a project.
        for n in count(&s).await..MAX_PLAYS {
            make(&s, s.org, s.project, with(json!({ "name": format!("Play {n}") }))).await.unwrap();
        }
        let over = make(&s, s.org, s.project, good.clone()).await;
        assert!(matches!(&over, Err(AppError::BadRequest(m)) if m == "A project can have up to 20 plays"), "{:?}", over.map(|p| p.name));
        assert_eq!(count(&s).await, MAX_PLAYS);
        assert!(matches!(make(&s, s.other_org, s.other_project, good.clone()).await, Err(AppError::BadRequest(_))), "needs its own project's campaign");
        assert!(make(&s, s.other_org, s.other_project, with(json!({ "segment_id": s.other_segment }))).await.is_ok(), "the limit is per project");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn editing_a_play_keeps_its_rule_and_turning_it_back_on_starts_from_now(db: PgPool) {
        let s = seed(db).await;
        let play = make(&s, s.org, s.project, json!({ "name": "Raised money", "trigger": "funding", "action": "add_to_campaign", "segment_id": s.segment, "backfill_days": 7 }))
            .await
            .unwrap();
        let other_play = make(&s, s.other_org, s.other_project, json!({ "name": "Visit", "trigger": "visit", "action": "notify" })).await.unwrap();

        for value in [json!({ "name": "" }), json!({ "min_score": 101 }), json!({ "daily_cap": 0 }), json!({ "segment_id": s.other_segment })] {
            let result = edit(&s, s.org, play.id, value.clone()).await;
            assert!(matches!(result, Err(AppError::BadRequest(_))), "{value}: {:?}", result.map(|p| p.name));
        }
        assert!(matches!(edit(&s, s.other_org, play.id, json!({ "name": "Mine" })).await, Err(AppError::NotFound)));
        assert!(matches!(edit(&s, s.org, other_play.id, json!({ "name": "Mine" })).await, Err(AppError::NotFound)));
        let unchanged = one(&s.state.db, play.id).await.unwrap();
        assert_eq!((unchanged.name.as_str(), unchanged.min_score, unchanged.daily_cap, unchanged.segment_id), ("Raised money", 0, 20, Some(s.segment)));
        assert!(about(unchanged.checked_at, 7));

        // The trigger and action stay, a blank campaign keeps the one it has, and the rest changes.
        let new_segment: Uuid =
            sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Hiring') returning id").bind(s.project).fetch_one(&s.state.db).await.unwrap();
        let edited = edit(&s, s.org, play.id, json!({ "name": " Funded ", "trigger": "score", "action": "notify", "min_score": 100, "daily_cap": 200, "segment_id": new_segment }))
            .await
            .unwrap();
        assert_eq!((edited.name.as_str(), edited.trigger.as_str(), edited.action.as_str()), ("Funded", "funding", "add_to_campaign"));
        assert_eq!((edited.min_score, edited.daily_cap, edited.segment_name.as_deref()), (100, 200, Some("Hiring")));
        assert_eq!(edit(&s, s.org, play.id, json!({ "segment_id": "" })).await.unwrap().segment_id, Some(new_segment));
        assert_eq!(edited.checked_at, unchanged.checked_at, "an edit that leaves it on keeps where it got to");

        // Off and on again, it starts from now. Turning on a play that's already on keeps where it got to.
        let off = edit(&s, s.org, play.id, json!({ "enabled": false })).await.unwrap();
        assert_eq!((off.enabled, off.checked_at), (false, unchanged.checked_at));
        let on = edit(&s, s.org, play.id, json!({ "enabled": true })).await.unwrap();
        assert!(on.enabled && about(on.checked_at, 0), "{}", on.checked_at);
        let earlier: Ts = sqlx::query_scalar("update plays set checked_at = now() - interval '3 days' where id = $1 returning checked_at")
            .bind(play.id)
            .fetch_one(&s.state.db)
            .await
            .unwrap();
        let again = edit(&s, s.org, play.id, json!({ "enabled": true })).await.unwrap();
        assert_eq!(again.checked_at, earlier);

        // Only the org's own play can be deleted.
        assert!(matches!(remove(State(s.state.clone()), owner(s.other_org), Path(play.id)).await, Err(AppError::NotFound)));
        assert_eq!(remove(State(s.state.clone()), owner(s.org), Path(play.id)).await.unwrap().0, json!({ "ok": true }));
        assert!(matches!(remove(State(s.state.clone()), owner(s.org), Path(play.id)).await, Err(AppError::NotFound)));
        assert_eq!(count(&s).await, 1);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_list_counts_each_plays_firings_for_a_day_and_a_month(db: PgPool) {
        let s = seed(db).await;
        let visit = make(&s, s.org, s.project, json!({ "name": "Visit", "trigger": "visit", "action": "notify" })).await.unwrap();
        let funding = make(&s, s.org, s.project, json!({ "name": "Funding", "trigger": "funding", "action": "notify" })).await.unwrap();
        let theirs = make(&s, s.other_org, s.other_project, json!({ "name": "Theirs", "trigger": "visit", "action": "notify" })).await.unwrap();
        sqlx::query("update plays set created_at = now() - interval '1 day' where id = $1").bind(funding.id).execute(&s.state.db).await.unwrap();
        let fire = |play: Uuid, project: Uuid, subject: String, hours_ago: i64| {
            let db = s.state.db.clone();
            async move {
                sqlx::query(
                    "insert into play_firings (play_id, project_id, subject, domain, what, outcome, created_at)
                     values ($1, $2, $3, $3 || '.example', 'Visited the pricing page', 'done', now() - make_interval(hours => $4::int))",
                )
                .bind(play)
                .bind(project)
                .bind(subject)
                .bind(hours_ago)
                .execute(&db)
                .await
                .unwrap();
            }
        };
        // Visit: 2 in the last day, one of them skipped, 3 more in the month, 50 older. Funding: 1 a week ago.
        for (n, hours) in [1, 23].into_iter().chain([25, 24 * 10, 24 * 29]).chain((0..50).map(|n| 24 * 31 + n)).enumerate() {
            fire(visit.id, s.project, format!("v{n}"), hours).await;
        }
        fire(funding.id, s.project, "f".into(), 24 * 7).await;
        sqlx::query("update play_firings set outcome = 'skipped' where subject = 'v1'").execute(&s.state.db).await.unwrap();
        fire(theirs.id, s.other_project, "t".into(), 0).await;

        assert!(matches!(list(State(s.state.clone()), owner(s.other_org), Path(s.project)).await, Err(AppError::NotFound)));
        let Json(page) = list(State(s.state.clone()), owner(s.org), Path(s.project)).await.unwrap();
        let counts: Vec<_> = page.plays.iter().map(|p| (p.name.as_str(), p.fired_today, p.fired_month)).collect();
        assert_eq!(counts, [("Funding", 0, 1), ("Visit", 2, 5)], "oldest play first");

        // The 50 latest firings, newest first, from this project only.
        assert_eq!(page.firings.len(), 50);
        assert!(page.firings.windows(2).all(|w| w[0].created_at >= w[1].created_at));
        let subjects: Vec<_> = page.firings.iter().map(|f| (f.play_name.clone(), f.domain.clone().unwrap_or_default())).collect();
        let visit_firing = |n: usize| ("Visit".to_string(), format!("v{n}.example"));
        let newest: Vec<_> = (0..3).map(visit_firing).chain([("Funding".into(), "f.example".into())]).chain((3..49).map(visit_firing)).collect();
        assert_eq!(subjects, newest);
        assert!(page.firings.iter().all(|f| f.play_id != theirs.id));
    }
}
