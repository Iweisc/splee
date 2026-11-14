//! A teammate's 1:1 prep: the latest, and asking for a new one. Owners and admins prep anyone's, everyone else only their own.
//! See agent::one_on_one.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use crate::agent::one_on_one::{self, Saved};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/one-on-ones/{user_id}", get(show).post(ask))
}

#[derive(Serialize)]
struct View {
    latest: Option<Saved>,
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
    Ok(Json(View { latest: one_on_one::latest(&state.db, auth.org_id, user_id).await? }))
}

async fn ask(State(state): State<AppState>, auth: Auth, Path(user_id): Path<Uuid>) -> AppResult<Json<View>> {
    allowed(&state, &auth, user_id).await?;
    if one_on_one::latest(&state.db, auth.org_id, user_id).await?.is_some_and(|l| l.status == "writing") {
        return Err(AppError::BadRequest("The agent is already writing this 1:1 prep".into()));
    }
    let run = one_on_one::request(&state.db, auth.org_id, user_id, auth.user_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(View { latest: one_on_one::latest(&state.db, auth.org_id, user_id).await? }))
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
}
