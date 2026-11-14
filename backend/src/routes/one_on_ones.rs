//! A teammate's 1:1 prep: the latest, asking for a new one, and assigning its practice call. Owners and admins prep anyone's,
//! everyone else only their own. See agent::one_on_one.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use crate::agent::one_on_one::{self, Prep, Saved};
use crate::agent::practice::{ASSIGNED, Assigned};
use crate::auth::Auth;
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/one-on-ones/{user_id}", get(show).post(ask)).route("/one-on-ones/{user_id}/practice", post(assign))
}

#[derive(Serialize)]
struct View {
    latest: Option<Saved>,
    /// The latest prep's practice call, once it's assigned.
    assignment: Option<Assigned>,
}

async fn view(state: &AppState, auth: &Auth, user_id: Uuid) -> AppResult<Json<View>> {
    let latest = one_on_one::latest(&state.db, auth.org_id, user_id).await?;
    let assignment = match &latest {
        Some(l) => sqlx::query_as(&format!("{ASSIGNED} where a.one_on_one_id = $1")).bind(l.id).fetch_optional(&state.db).await?,
        None => None,
    };
    Ok(Json(View { latest, assignment }))
}

/// Their own, or anyone's in the organization for an owner or admin.
async fn allowed(state: &AppState, auth: &Auth, user_id: Uuid) -> AppResult<()> {
    if user_id != auth.user_id {
        auth.require_admin()?;
    }
    let member: bool = sqlx::query_scalar("select exists (select 1 from memberships where org_id = $1 and user_id = $2)")
        .bind(auth.org_id)
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;
    if member { Ok(()) } else { Err(AppError::NotFound) }
}

async fn show(State(state): State<AppState>, auth: Auth, Path(user_id): Path<Uuid>) -> AppResult<Json<View>> {
    allowed(&state, &auth, user_id).await?;
    view(&state, &auth, user_id).await
}

async fn ask(State(state): State<AppState>, auth: Auth, Path(user_id): Path<Uuid>) -> AppResult<Json<View>> {
    allowed(&state, &auth, user_id).await?;
    if one_on_one::latest(&state.db, auth.org_id, user_id).await?.is_some_and(|l| l.status == "writing") {
        return Err(AppError::BadRequest("The agent is already writing this 1:1 prep".into()));
    }
    let run = one_on_one::request(&state.db, auth.org_id, user_id, auth.user_id).await?;
    crate::agent::start_now(&state, run).await?;
    view(&state, &auth, user_id).await
}

/// Puts the latest prep's practice call on the teammate's Today. Assigning it again changes nothing.
async fn assign(State(state): State<AppState>, auth: Auth, Path(user_id): Path<Uuid>) -> AppResult<Json<View>> {
    allowed(&state, &auth, user_id).await?;
    let latest = one_on_one::latest(&state.db, auth.org_id, user_id).await?.ok_or(AppError::NotFound)?;
    let prep: Option<Prep> = latest.prep.clone().map(serde_json::from_value).transpose().map_err(anyhow::Error::from)?;
    let Some((skill, drill)) = prep.and_then(|p| p.skill.practice.clone().map(|d| (p.skill, d))) else {
        return Err(AppError::BadRequest("This prep's drill isn't a practice call".into()));
    };
    if deals::get(&state.db, auth.org_id, drill.deal_id).await?.status != "open" {
        return Err(AppError::BadRequest(format!("The {} deal is closed now, so write a new prep", drill.company)));
    }
    sqlx::query(
        "insert into practice_assignments (one_on_one_id, deal_id, user_id, assigned_by, scenario, skill, drill) values ($1, $2, $3, $4, $5, $6, $7)
         on conflict (one_on_one_id) do nothing",
    )
    .bind(latest.id)
    .bind(drill.deal_id)
    .bind(user_id)
    .bind(auth.user_id)
    .bind(&drill.scenario)
    .bind(&skill.name)
    .bind(&skill.drill)
    .execute(&state.db)
    .await?;
    view(&state, &auth, user_id).await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn members_see_only_their_own_prep_and_admins_only_their_organizations(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let mut users = vec![];
        for (name, role) in [("Dana", "owner"), ("Sam", "member"), ("Kai", "member")] {
            let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values (lower($1) || '@acme.test', $1, 'x') returning id")
                .bind(name)
                .fetch_one(&db)
                .await
                .unwrap();
            // Kai belongs to another organization.
            let member_of: Uuid = if name == "Kai" {
                sqlx::query_scalar("insert into organizations (name) values ('Other') returning id").fetch_one(&db).await.unwrap()
            } else {
                org
            };
            sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, $3)").bind(member_of).bind(user).bind(role).execute(&db).await.unwrap();
            users.push(user);
        }
        let (dana, sam, kai) = (users[0], users[1], users[2]);
        let state = crate::autopilot::tests::state(db);
        let auth = |user: Uuid, role: &str| Auth { user_id: user, org_id: org, role: role.into(), token_hash: String::new() };
        assert!(show(State(state.clone()), auth(sam, "member"), Path(sam)).await.unwrap().0.latest.is_none());
        assert!(matches!(show(State(state.clone()), auth(sam, "member"), Path(dana)).await, Err(AppError::Forbidden(_))));
        assert!(show(State(state.clone()), auth(dana, "owner"), Path(sam)).await.is_ok());
        assert!(matches!(show(State(state), auth(dana, "owner"), Path(kai)).await, Err(AppError::NotFound)));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_preps_practice_call_is_assigned_once_and_only_while_its_deal_is_open(db: PgPool) {
        use crate::agent::practice::fixture;
        let t = fixture::team(&db).await;
        let state = crate::autopilot::tests::state(db.clone());
        let dana = || Auth { user_id: t.dana, org_id: t.org, role: "owner".into(), token_hash: String::new() };
        let assigned = || sqlx::query_scalar::<_, i64>("select count(*) from practice_assignments").fetch_one(&db);

        fixture::prep(&db, &t, false).await;
        assert!(matches!(assign(State(state.clone()), dana(), Path(t.sam)).await, Err(AppError::BadRequest(_))), "a drill that isn't a practice call");

        let prep = fixture::prep(&db, &t, true).await;
        let first = assign(State(state.clone()), dana(), Path(t.sam)).await.unwrap().0;
        let view = assign(State(state.clone()), dana(), Path(t.sam)).await.unwrap().0;
        assert_eq!(assigned().await.unwrap(), 1, "assigning again changes nothing");
        assert_eq!(first.assignment.map(|a| a.id), view.assignment.as_ref().map(|a| a.id));
        let a = view.assignment.unwrap();
        assert_eq!((a.deal_id, a.scenario.as_str(), a.skill.as_str(), a.assigned_by.as_deref()), (t.deal, "pricing", "Holding price", Some("Dana Reyes")));
        assert_eq!((a.session_id, a.score), (None, None));

        // A newer prep on a deal that's closed since can't be assigned.
        sqlx::query("delete from practice_assignments where one_on_one_id = $1").bind(prep).execute(&db).await.unwrap();
        fixture::prep(&db, &t, true).await;
        sqlx::query("update deals set status = 'lost', closed_at = now() where id = $1").bind(t.deal).execute(&db).await.unwrap();
        assert!(matches!(assign(State(state), dana(), Path(t.sam)).await, Err(AppError::BadRequest(_))));
        assert_eq!(assigned().await.unwrap(), 0);
    }
}
