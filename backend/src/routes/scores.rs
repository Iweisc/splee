//! Lead scores (score.rs): which signals count, and the overview's hottest leads.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::leads::{LEAD_ROW_SELECT, LeadRow};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::score::{self, SIGNALS};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/score/signals", get(signals).put(set_signals))
        .route("/projects/{id}/leads/hottest", get(hottest))
}

#[derive(Serialize)]
struct SignalSetting {
    key: &'static str,
    label: &'static str,
    /// "fit" or "intent".
    part: &'static str,
    on: bool,
}

async fn settings(db: &PgPool, project_id: Uuid) -> AppResult<Vec<SignalSetting>> {
    let saved: Value = sqlx::query_scalar("select score_signals from project_settings where project_id = $1").bind(project_id).fetch_one(db).await?;
    Ok(SIGNALS
        .iter()
        .map(|s| SignalSetting { key: s.key, label: s.label, part: if s.window == 0 { "fit" } else { "intent" }, on: saved[s.key] != false })
        .collect())
}

async fn signals(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<SignalSetting>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(settings(&state.db, id).await?))
}

/// Takes {"visit": false, ...}. Only signals that are off are kept, so a new signal starts on.
async fn set_signals(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<Map<String, Value>>,
) -> AppResult<Json<Vec<SignalSetting>>> {
    auth.project(&state.db, id).await?;
    let mut off = Map::new();
    for (key, on) in body {
        if !SIGNALS.iter().any(|s| s.key == key) {
            return Err(AppError::BadRequest(format!("There's no score signal called {key}")));
        }
        match on {
            Value::Bool(true) => {}
            Value::Bool(false) => {
                off.insert(key, Value::Bool(false));
            }
            _ => return Err(AppError::BadRequest("Turn each signal on or off with true or false".into())),
        }
    }
    sqlx::query("update project_settings set score_signals = $2 where project_id = $1").bind(id).bind(Value::Object(off)).execute(&state.db).await?;
    score::touch(&state.db, id).await?;
    Ok(Json(settings(&state.db, id).await?))
}

/// The five leads with the most intent right now, for the overview. Leads with none are left out.
async fn hottest(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<LeadRow>>> {
    auth.project(&state.db, id).await?;
    let leads = sqlx::query_as::<_, LeadRow>(&format!(
        "{LEAD_ROW_SELECT} where l.project_id = $1 and l.score_intent > 0 order by l.score_intent desc, l.score desc, l.id limit 5"
    ))
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(leads))
}

#[cfg(test)]
mod tests {
    use axum::extract::{Path, State};
    use axum::Json;
    use serde_json::json;
    use sqlx::PgPool;
    use uuid::Uuid;

    use crate::auth::Auth;
    use crate::error::AppError;

    async fn org_project(db: &PgPool) -> (Auth, Uuid) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values (gen_random_uuid()::text || '@x.com', 'x') returning id")
            .fetch_one(db)
            .await
            .unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id) values ($1) returning id").bind(org).fetch_one(db).await.unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(db).await.unwrap();
        (Auth { user_id: user, org_id: org, role: "owner".into(), token_hash: String::new() }, project)
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn signals_turn_off_and_hottest_lists_intent_for_the_org_only(db: PgPool) {
        let (auth, project) = org_project(&db).await;
        let (other, other_project) = org_project(&db).await;
        let state = || State(crate::autopilot::tests::state(db.clone()));

        let Json(saved) = super::set_signals(state(), auth.clone(), Path(project), Json(json!({"visit": false, "deal": true}).as_object().unwrap().clone()))
            .await
            .unwrap();
        assert!(saved.iter().all(|s| s.on == (s.key != "visit")));
        let stored: serde_json::Value = sqlx::query_scalar("select score_signals from project_settings where project_id = $1").bind(project).fetch_one(&db).await.unwrap();
        assert_eq!(stored, json!({"visit": false}));
        let bad = super::set_signals(state(), auth.clone(), Path(project), Json(json!({"vibes": false}).as_object().unwrap().clone())).await;
        assert!(matches!(bad, Err(AppError::BadRequest(_))));
        let theirs = super::set_signals(state(), other.clone(), Path(project), Json(serde_json::Map::new())).await;
        assert!(matches!(theirs, Err(AppError::NotFound)));

        for (p, name, intent) in [(project, "Ana", 40), (project, "Bo", 0), (project, "Cy", 12), (other_project, "Di", 50)] {
            sqlx::query("insert into leads (project_id, full_name, email, score_intent, score) values ($1, $2, $2 || '@x.com', $3, $3)")
                .bind(p)
                .bind(name)
                .bind(intent as i16)
                .execute(&db)
                .await
                .unwrap();
        }
        let Json(hot) = super::hottest(state(), auth.clone(), Path(project)).await.unwrap();
        assert_eq!(hot.iter().map(|l| l.lead.full_name.as_str()).collect::<Vec<_>>(), ["Ana", "Cy"]);
        assert!(matches!(super::hottest(state(), other, Path(project)).await, Err(AppError::NotFound)));
    }
}
