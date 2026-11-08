use axum::extract::{Path, State};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::util_input;
use crate::auth::{Auth, clear_session_cookie};
use crate::crypto::random_token;
use crate::error::{AppError, AppResult};
use crate::mail;
use crate::models::{Invite, Organization, Ts};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/organization", get(get_org).patch(update_org).delete(delete_org))
        .route("/organization/users", get(list_members))
        .route("/organization/users/{user_id}", patch(update_member).delete(remove_member))
        .route("/organization/invites", get(list_invites).post(create_invite))
        .route("/organization/invites/{id}", delete(delete_invite))
        .route("/organization/stop-campaigns", post(stop_campaigns))
        .route("/organization/deletion-eligibility", get(deletion_eligibility))
}

async fn get_org(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Organization>> {
    let org = sqlx::query_as::<_, Organization>("select * from organizations where id = $1")
        .bind(auth.org_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(org))
}

#[derive(Deserialize)]
struct OrgBody {
    name: String,
}

async fn update_org(State(state): State<AppState>, auth: Auth, Json(body): Json<OrgBody>) -> AppResult<Json<Organization>> {
    auth.require_admin()?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("Name can't be empty".into()));
    }
    let org = sqlx::query_as::<_, Organization>("update organizations set name = $2 where id = $1 returning *")
        .bind(auth.org_id)
        .bind(name)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(org))
}

#[derive(Serialize, FromRow)]
struct Member {
    user_id: Uuid,
    email: String,
    name: String,
    role: String,
    created_at: Ts,
}

const MEMBER_SELECT: &str = "select m.user_id, u.email::text as email, u.name, m.role, m.created_at
     from memberships m join users u on u.id = m.user_id where m.org_id = $1";

async fn list_members(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<Member>>> {
    let members = sqlx::query_as::<_, Member>(&format!("{MEMBER_SELECT} order by m.created_at"))
        .bind(auth.org_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(members))
}

/// Admins manage members and admins. Only owners can touch an owner or create one.
async fn check_can_manage(db: &PgPool, auth: &Auth, user_id: Uuid, new_role: Option<&str>) -> AppResult<String> {
    auth.require_admin()?;
    let current: String = sqlx::query_scalar("select role from memberships where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(user_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)?;
    if (current == "owner" || new_role == Some("owner")) && auth.role != "owner" {
        return Err(AppError::Forbidden("Only owners can change owners".into()));
    }
    if current == "owner" && new_role != Some("owner") {
        let owners: i64 = sqlx::query_scalar("select count(*) from memberships where org_id = $1 and role = 'owner'")
            .bind(auth.org_id)
            .fetch_one(db)
            .await?;
        if owners <= 1 {
            return Err(AppError::BadRequest("The organization needs at least one owner".into()));
        }
    }
    Ok(current)
}

#[derive(Deserialize)]
struct RoleBody {
    role: String,
}

async fn update_member(
    State(state): State<AppState>,
    auth: Auth,
    Path(user_id): Path<Uuid>,
    Json(body): Json<RoleBody>,
) -> AppResult<Json<Member>> {
    util_input::one_of("role", &body.role, &["owner", "admin", "member"])?;
    check_can_manage(&state.db, &auth, user_id, Some(&body.role)).await?;
    sqlx::query("update memberships set role = $3 where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(user_id)
        .bind(&body.role)
        .execute(&state.db)
        .await?;
    let member = sqlx::query_as::<_, Member>(&format!("{MEMBER_SELECT} and m.user_id = $2"))
        .bind(auth.org_id)
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(member))
}

async fn remove_member(State(state): State<AppState>, auth: Auth, Path(user_id): Path<Uuid>) -> AppResult<Json<Value>> {
    check_can_manage(&state.db, &auth, user_id, None).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("delete from memberships where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from sessions where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_invites(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<Invite>>> {
    let invites = sqlx::query_as::<_, Invite>(
        "select * from invites where org_id = $1 and accepted_at is null order by created_at desc",
    )
    .bind(auth.org_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(invites))
}

#[derive(Deserialize)]
struct InviteBody {
    email: String,
    role: String,
}

async fn create_invite(State(state): State<AppState>, auth: Auth, Json(body): Json<InviteBody>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    let email = util_input::email(&body.email).ok_or_else(|| AppError::BadRequest("Enter a valid email address".into()))?;
    util_input::one_of("role", &body.role, &["admin", "member"])?;
    let invite = sqlx::query_as::<_, Invite>(
        "insert into invites (org_id, email, role, token, invited_by) values ($1, $2, $3, $4, $5) returning *",
    )
    .bind(auth.org_id)
    .bind(&email)
    .bind(&body.role)
    .bind(random_token())
    .bind(auth.user_id)
    .fetch_one(&state.db)
    .await?;
    let link = format!("{}/invite/{}", state.cfg.public_url, invite.token);

    let org_name: String = sqlx::query_scalar("select name from organizations where id = $1")
        .bind(auth.org_id)
        .fetch_one(&state.db)
        .await?;
    let subject = format!("You're invited to join {org_name} on Splee");
    let text = format!("You've been invited to join {org_name} on Splee.\n\nAccept the invite: {link}\n");
    if let Err(e) = mail::send_system_email(&state, auth.org_id, &email, &subject, &text).await {
        tracing::warn!(error = ?e, "invite email failed, the link still works");
    }
    Ok(Json(json!({ "invite": invite, "link": link })))
}

async fn delete_invite(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    let deleted = sqlx::query("delete from invites where id = $1 and org_id = $2")
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

#[derive(Deserialize)]
struct StopBody {
    reason: String,
    #[serde(default)]
    details: String,
}

async fn stop_campaigns(State(state): State<AppState>, auth: Auth, Json(body): Json<StopBody>) -> AppResult<Json<Value>> {
    let reason = body.reason.trim();
    if reason.is_empty() {
        return Err(AppError::BadRequest("Pick a reason".into()));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "update project_settings set paused = true
         where project_id in (select id from projects where org_id = $1)",
    )
    .bind(auth.org_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("update projects set status = 'paused' where org_id = $1 and status = 'active'")
        .bind(auth.org_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("insert into stop_feedback (org_id, user_id, reason, details) values ($1, $2, $3, $4)")
        .bind(auth.org_id)
        .bind(auth.user_id)
        .bind(reason)
        .bind(body.details.trim())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Serialize)]
struct Eligibility {
    eligible: bool,
    reason: String,
}

// Paused projects never send, so their queued emails don't block deletion. Otherwise "stop campaigns" would never unblock it.
async fn eligibility(db: &PgPool, org_id: Uuid) -> AppResult<Eligibility> {
    let due: i64 = sqlx::query_scalar(
        "select count(*) from email_messages e
         join projects p on p.id = e.project_id
         join project_settings ps on ps.project_id = p.id
         where p.org_id = $1 and not ps.paused and e.status in ('approved', 'scheduled', 'sending')
           and coalesce(e.scheduled_for, now()) <= now() + interval '1 hour'",
    )
    .bind(org_id)
    .fetch_one(db)
    .await?;
    Ok(if due == 0 {
        Eligibility { eligible: true, reason: String::new() }
    } else {
        Eligibility {
            eligible: false,
            reason: format!(
                "{due} {} scheduled to go out within the next hour. Stop your campaigns first.",
                if due == 1 { "email is" } else { "emails are" }
            ),
        }
    })
}

async fn deletion_eligibility(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Eligibility>> {
    Ok(Json(eligibility(&state.db, auth.org_id).await?))
}

async fn delete_org(State(state): State<AppState>, auth: Auth, jar: CookieJar) -> AppResult<(CookieJar, Json<Value>)> {
    if auth.role != "owner" {
        return Err(AppError::Forbidden("Only owners can delete the organization".into()));
    }
    let check = eligibility(&state.db, auth.org_id).await?;
    if !check.eligible {
        return Err(AppError::Conflict(check.reason));
    }
    sqlx::query("delete from organizations where id = $1").bind(auth.org_id).execute(&state.db).await?;
    Ok((jar.add(clear_session_cookie(state.cfg.secure_cookies())), Json(json!({ "ok": true }))))
}
