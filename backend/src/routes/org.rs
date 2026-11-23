use axum::extract::{Path, State};
use axum::routing::{delete, get, patch};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use super::util_input;
use crate::audit::{self, Target};
use crate::auth::{Auth, clear_session_cookie};
use crate::crypto::random_token;
use crate::error::{AppError, AppResult};
use crate::mail;
use crate::models::{INVITE_TTL, Invite, Organization, Ts};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/organization", get(get_org).patch(update_org).delete(delete_org))
        .route("/organization/users", get(list_members))
        .route("/organization/users/{user_id}", patch(update_member).delete(remove_member))
        .route("/organization/invites", get(list_invites).post(create_invite))
        .route("/organization/invites/{id}", delete(delete_invite))
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
pub(super) struct OrgBody {
    name: String,
}

pub(super) async fn update_org(State(state): State<AppState>, auth: Auth, Json(body): Json<OrgBody>) -> AppResult<Json<Organization>> {
    auth.require_admin()?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("Name can't be empty".into()));
    }
    let old: String = sqlx::query_scalar("select name from organizations where id = $1").bind(auth.org_id).fetch_one(&state.db).await?;
    let org = sqlx::query_as::<_, Organization>("update organizations set name = $2 where id = $1 returning *")
        .bind(auth.org_id)
        .bind(name)
        .fetch_one(&state.db)
        .await?;
    if old != org.name {
        audit::record(&state.db, &auth, "org.renamed", Target::new("org", org.id, &org.name), json!({ "from": old, "to": org.name })).await;
    }
    Ok(Json(org))
}

#[derive(Serialize, FromRow)]
pub(super) struct Member {
    user_id: Uuid,
    email: String,
    name: String,
    role: String,
    /// Round robin skips them while they're away. They set it on their account page.
    away: bool,
    /// Whether they sign in with a code from an authenticator app. Only admins see it.
    #[serde(skip_serializing_if = "Option::is_none")]
    two_factor: Option<bool>,
    created_at: Ts,
}

const MEMBER_SELECT: &str = "select m.user_id, u.email::text as email, u.name, m.role, m.away, m.created_at,
     exists (select 1 from two_factor t where t.user_id = m.user_id and t.enabled_at is not null) as two_factor
     from memberships m join users u on u.id = m.user_id where m.org_id = $1";

async fn list_members(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<Member>>> {
    let mut members = sqlx::query_as::<_, Member>(&format!("{MEMBER_SELECT} order by m.created_at"))
        .bind(auth.org_id)
        .fetch_all(&state.db)
        .await?;
    if auth.require_admin().is_err() {
        members.iter_mut().for_each(|m| m.two_factor = None);
    }
    Ok(Json(members))
}

/// Admins manage members and admins. Only owners can touch an owner or create one.
///
/// Locks the organization's row until `tx` ends, so two owners demoting or removing each other at once can't both pass the
/// last owner check. The caller's role is read again under the lock, as a change that went first may have taken it away.
async fn check_can_manage(tx: &mut PgConnection, auth: &Auth, user_id: Uuid, new_role: Option<&str>) -> AppResult<String> {
    sqlx::query("select 1 from organizations where id = $1 for update").bind(auth.org_id).execute(&mut *tx).await?;
    let role: Option<String> = sqlx::query_scalar("select role from memberships where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(auth.user_id)
        .fetch_optional(&mut *tx)
        .await?;
    let auth = &Auth { role: role.unwrap_or_default(), ..auth.clone() };
    auth.require_admin()?;
    let current: String = sqlx::query_scalar("select role from memberships where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
    if (current == "owner" || new_role == Some("owner")) && auth.role != "owner" {
        return Err(AppError::Forbidden("Only owners can change owners".into()));
    }
    if current == "owner" && new_role != Some("owner") {
        let owners: i64 = sqlx::query_scalar("select count(*) from memberships where org_id = $1 and role = 'owner'")
            .bind(auth.org_id)
            .fetch_one(&mut *tx)
            .await?;
        if owners <= 1 {
            return Err(AppError::BadRequest("The organization needs at least one owner".into()));
        }
    }
    Ok(current)
}

#[derive(Deserialize)]
pub(super) struct RoleBody {
    role: String,
}

pub(super) async fn update_member(
    State(state): State<AppState>,
    auth: Auth,
    Path(user_id): Path<Uuid>,
    Json(body): Json<RoleBody>,
) -> AppResult<Json<Member>> {
    util_input::one_of("role", &body.role, &["owner", "admin", "member"])?;
    let mut tx = state.db.begin().await?;
    let old_role = check_can_manage(&mut tx, &auth, user_id, Some(&body.role)).await?;
    sqlx::query("update memberships set role = $3 where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(user_id)
        .bind(&body.role)
        .execute(&mut *tx)
        .await?;
    let member = sqlx::query_as::<_, Member>(&format!("{MEMBER_SELECT} and m.user_id = $2"))
        .bind(auth.org_id)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    if old_role != member.role {
        let details = json!({ "from": old_role, "to": member.role });
        audit::record(&state.db, &auth, "member.role_changed", Target::new("user", user_id, &member.email), details).await;
    }
    Ok(Json(member))
}

async fn remove_member(State(state): State<AppState>, auth: Auth, Path(user_id): Path<Uuid>) -> AppResult<Json<Value>> {
    let mut tx = state.db.begin().await?;
    let role = check_can_manage(&mut tx, &auth, user_id, None).await?;
    let email: String = sqlx::query_scalar("select email::text from users where id = $1").bind(user_id).fetch_one(&mut *tx).await?;
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
    // Their leads go back to nobody, so the rule or a teammate can pick them up, and so do their mailboxes.
    sqlx::query("update leads set owner_id = null where owner_id = $2 and project_id in (select id from projects where org_id = $1)")
        .bind(auth.org_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update mailboxes set owner_id = null where org_id = $1 and owner_id = $2").bind(auth.org_id).bind(user_id).execute(&mut *tx).await?;
    tx.commit().await?;
    audit::record(&state.db, &auth, "member.removed", Target::new("user", user_id, &email), json!({ "role": role })).await;
    Ok(Json(json!({ "ok": true })))
}

async fn list_invites(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<Invite>>> {
    let invites = sqlx::query_as::<_, Invite>(&format!(
        "select *, created_at + interval '{INVITE_TTL}' as expires_at from invites
         where org_id = $1 and accepted_at is null order by created_at desc"
    ))
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
    let invite = sqlx::query_as::<_, Invite>(&format!(
        "insert into invites (org_id, email, role, token, invited_by) values ($1, $2, $3, $4, $5)
         returning *, created_at + interval '{INVITE_TTL}' as expires_at"
    ))
    .bind(auth.org_id)
    .bind(&email)
    .bind(&body.role)
    .bind(random_token())
    .bind(auth.user_id)
    .fetch_one(&state.db)
    .await?;
    let link = format!("{}/invite/{}", state.cfg.public_url, invite.token);
    audit::record(&state.db, &auth, "member.invited", Target::new("invite", invite.id, &email), json!({ "role": invite.role })).await;

    let org_name: String = sqlx::query_scalar("select name from organizations where id = $1")
        .bind(auth.org_id)
        .fetch_one(&state.db)
        .await?;
    let subject = format!("You're invited to join {org_name} on Splee");
    let text = format!("You've been invited to join {org_name} on Splee.\n\nAccept the invite: {link}\n\nThe link works for {INVITE_TTL}.\n");
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

#[derive(Serialize)]
struct Eligibility {
    eligible: bool,
    reason: String,
}

// Paused projects never send, so their queued emails don't block deletion. Otherwise "Pause sending" would never unblock it.
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
                "{due} {} scheduled to go out within the next hour. Pause sending on your projects first.",
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::auth::SESSION_COOKIE;
    use crate::config::Config;

    /// An org with an owner, an admin and a member, another org the member owns, and a stranger who is only in that one.
    struct Team {
        state: AppState,
        org: Uuid,
        other: Uuid,
        owner: Uuid,
        admin: Uuid,
        member: Uuid,
        stranger: Uuid,
    }

    async fn team(db: PgPool) -> Team {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let other: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Other') returning id").fetch_one(&db).await.unwrap();
        let mut users = Vec::new();
        // The roster lists people by when they joined, which isn't the order they're inserted in here.
        for (name, joined_hours_ago, org, role) in [("olive", 3, org, "owner"), ("adam", 1, org, "admin"), ("mia", 2, org, "member"), ("sam", 4, other, "member")] {
            let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1 || '@acme.example', $1, 'x') returning id")
                .bind(name)
                .fetch_one(&db)
                .await
                .unwrap();
            sqlx::query("insert into memberships (org_id, user_id, role, created_at) values ($1, $2, $3, now() - make_interval(hours => $4))")
                .bind(org)
                .bind(user)
                .bind(role)
                .bind(joined_hours_ago)
                .execute(&db)
                .await
                .unwrap();
            users.push(user);
        }
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(other).bind(users[2]).execute(&db).await.unwrap();
        let cfg = Config { public_url: "https://app.example".into(), ..Config::test() };
        let state = AppState { db, cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(16).0, rate_limits: Default::default() };
        Team { state, org, other, owner: users[0], admin: users[1], member: users[2], stranger: users[3] }
    }

    impl Team {
        fn auth(&self, user: Uuid) -> Auth {
            let role = if user == self.owner { "owner" } else if user == self.admin { "admin" } else { "member" };
            Auth { user_id: user, org_id: self.org, role: role.into(), token_hash: String::new() }
        }

        async fn role_of(&self, org: Uuid, user: Uuid) -> Option<String> {
            sqlx::query_scalar("select role from memberships where org_id = $1 and user_id = $2").bind(org).bind(user).fetch_optional(&self.state.db).await.unwrap()
        }

        async fn set_role(&self, by: Uuid, user: Uuid, role: &str) -> AppResult<Member> {
            self.set_role_as(self.auth(by), user, role).await
        }

        async fn set_role_as(&self, auth: Auth, user: Uuid, role: &str) -> AppResult<Member> {
            update_member(State(self.state.clone()), auth, Path(user), Json(RoleBody { role: role.into() })).await.map(|Json(m)| m)
        }

        async fn audit(&self, action: &str) -> Vec<(String, Value)> {
            sqlx::query_as("select target_label, details from audit_events where org_id = $1 and action = $2 order by created_at")
                .bind(self.org)
                .bind(action)
                .fetch_all(&self.state.db)
                .await
                .unwrap()
        }

        async fn orgs(&self) -> Vec<Uuid> {
            sqlx::query_scalar("select id from organizations order by name").fetch_all(&self.state.db).await.unwrap()
        }
    }

    fn forbidden<T>(result: AppResult<T>) -> String {
        match result {
            Err(AppError::Forbidden(m)) => m,
            Err(e) => panic!("{e:?}"),
            Ok(_) => panic!("allowed"),
        }
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn renaming_takes_an_admin_and_a_name_and_is_audited_when_it_changes(db: PgPool) {
        let t = team(db).await;
        let rename = |by: Uuid, name: &str| update_org(State(t.state.clone()), t.auth(by), Json(OrgBody { name: name.into() }));
        assert_eq!(forbidden(rename(t.member, "Mine").await), "Only admins can do this");
        assert!(matches!(rename(t.admin, "   ").await, Err(AppError::BadRequest(m)) if m == "Name can't be empty"));
        assert_eq!(rename(t.admin, "  Acme Ltd  ").await.unwrap().0.name, "Acme Ltd");
        assert_eq!(rename(t.owner, "Acme Ltd").await.unwrap().0.name, "Acme Ltd");
        assert_eq!(t.audit("org.renamed").await, [("Acme Ltd".to_string(), json!({ "from": "Acme", "to": "Acme Ltd" }))], "a save that changes nothing isn't audited");
        let Json(org) = get_org(State(t.state.clone()), t.auth(t.member)).await.unwrap();
        assert_eq!((org.id, org.name.as_str()), (t.org, "Acme Ltd"));
        let other: String = sqlx::query_scalar("select name from organizations where id = $1").bind(t.other).fetch_one(&t.state.db).await.unwrap();
        assert_eq!(other, "Other");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn only_owners_touch_owners_and_the_last_one_stays(db: PgPool) {
        let t = team(db).await;
        sqlx::query("insert into two_factor (user_id, secret_enc, enabled_at) values ($1, 'x', now())").bind(t.admin).execute(&t.state.db).await.unwrap();
        let roster = |by: Uuid| {
            let (state, auth) = (t.state.clone(), t.auth(by));
            async move { serde_json::to_value(list_members(State(state), auth).await.unwrap().0).unwrap() }
        };
        let shown = |roster: Value| -> Vec<(String, String, Option<bool>)> {
            let people = roster.as_array().unwrap().iter();
            people.map(|m| (m["email"].as_str().unwrap().into(), m["role"].as_str().unwrap().into(), m.get("two_factor").map(|f| f.as_bool().unwrap()))).collect()
        };
        let person = |email: &str, role: &str, two_factor: Option<bool>| (email.to_string(), role.to_string(), two_factor);
        assert_eq!(shown(roster(t.admin).await), [
            person("olive@acme.example", "owner", Some(false)),
            person("mia@acme.example", "member", Some(false)),
            person("adam@acme.example", "admin", Some(true)),
        ]);
        assert_eq!(
            shown(roster(t.member).await),
            [person("olive@acme.example", "owner", None), person("mia@acme.example", "member", None), person("adam@acme.example", "admin", None)],
            "only admins see who has two-factor, and for others it's left out rather than null"
        );

        assert!(matches!(t.set_role(t.admin, t.member, "boss").await, Err(AppError::BadRequest(_))));
        assert_eq!(forbidden(t.set_role(t.member, t.admin, "member").await), "Only admins can do this");
        assert_eq!(forbidden(t.set_role(t.admin, t.member, "owner").await), "Only owners can change owners");
        assert_eq!(forbidden(t.set_role(t.admin, t.owner, "member").await), "Only owners can change owners");
        assert!(matches!(t.set_role(t.owner, t.stranger, "admin").await, Err(AppError::NotFound)), "they're only in the other org");
        assert!(matches!(t.set_role(t.owner, t.owner, "admin").await, Err(AppError::BadRequest(m)) if m == "The organization needs at least one owner"));
        assert_eq!(
            (t.role_of(t.org, t.owner).await, t.role_of(t.org, t.admin).await, t.role_of(t.org, t.member).await, t.role_of(t.other, t.stranger).await),
            (Some("owner".into()), Some("admin".into()), Some("member".into()), Some("member".into())),
            "nothing changed"
        );

        // A request that started while its sender was still an owner, or still in the org, goes by their role now.
        let was_owner = Auth { role: "owner".into(), ..t.auth(t.admin) };
        assert_eq!(forbidden(t.set_role_as(was_owner.clone(), t.member, "owner").await), "Only owners can change owners");
        assert_eq!(forbidden(remove_member(State(t.state.clone()), was_owner, Path(t.owner)).await), "Only owners can change owners");
        let gone = Auth { role: "owner".into(), ..t.auth(t.stranger) };
        assert_eq!(forbidden(t.set_role_as(gone, t.member, "admin").await), "Only admins can do this");

        // An admin promotes a member, and an owner makes them an owner, after which the first owner can step down.
        assert_eq!(t.set_role(t.admin, t.member, "admin").await.unwrap().role, "admin");
        assert_eq!(t.set_role(t.owner, t.member, "owner").await.unwrap().role, "owner");
        assert_eq!(t.set_role(t.owner, t.owner, "member").await.unwrap().role, "member");
        let now_owner = Auth { role: "owner".into(), ..t.auth(t.member) };
        assert_eq!(t.set_role_as(now_owner, t.member, "owner").await.unwrap().role, "owner", "setting the role it has is fine and isn't audited");
        assert_eq!(t.role_of(t.other, t.member).await.as_deref(), Some("owner"), "their role elsewhere is untouched");
        let mut changes: Vec<(String, Value)> = t.audit("member.role_changed").await;
        changes.sort_by_key(|(who, d)| (who.clone(), d.to_string()));
        assert_eq!(changes, [
            ("mia@acme.example".to_string(), json!({ "from": "admin", "to": "owner" })),
            ("mia@acme.example".to_string(), json!({ "from": "member", "to": "admin" })),
            ("olive@acme.example".to_string(), json!({ "from": "owner", "to": "member" })),
        ]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn two_owners_stepping_down_at_once_leave_one_owner(db: PgPool) {
        let t = team(db).await;
        let (olive, adam) = (Auth { role: "owner".into(), ..t.auth(t.owner) }, Auth { role: "owner".into(), ..t.auth(t.admin) });
        // Racing them once may not overlap, so race them a few times.
        for _ in 0..10 {
            sqlx::query("update memberships set role = 'owner' where org_id = $1 and user_id = any($2)").bind(t.org).bind(&[t.owner, t.admin][..]).execute(&t.state.db).await.unwrap();
            let (first, second) = tokio::join!(t.set_role_as(olive.clone(), t.owner, "admin"), t.set_role_as(adam.clone(), t.admin, "admin"));
            let refused = match (first, second) {
                (Ok(_), Err(e)) | (Err(e), Ok(_)) => e,
                (first, second) => panic!("one should go through: {:?} {:?}", first.err(), second.err()),
            };
            assert!(matches!(refused, AppError::BadRequest(m) if m == "The organization needs at least one owner"));
            let owners: i64 = sqlx::query_scalar("select count(*) from memberships where org_id = $1 and role = 'owner'").bind(t.org).fetch_one(&t.state.db).await.unwrap();
            assert_eq!(owners, 1);
        }
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn removing_someone_signs_them_out_and_frees_their_leads_and_mailboxes_here_only(db: PgPool) {
        let t = team(db).await;
        let db = &t.state.db;
        let project_in = |org: Uuid| async move {
            sqlx::query_scalar::<_, Uuid>("insert into projects (org_id, name) values ($1, 'P') returning id").bind(org).fetch_one(db).await.unwrap()
        };
        let (here, there) = (project_in(t.org).await, project_in(t.other).await);
        for (project, org, n) in [(here, t.org, 1), (there, t.other, 2)] {
            sqlx::query("insert into leads (project_id, email, owner_id) values ($1, $2, $3)").bind(project).bind(format!("lead{n}@lead.example")).bind(t.member).execute(db).await.unwrap();
            sqlx::query("insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, owner_id) values ($1, $2, 'smtp.example', '', '', $3)")
                .bind(org)
                .bind(format!("mia{n}@mail.example"))
                .bind(t.member)
                .execute(db)
                .await
                .unwrap();
            sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                .bind(format!("session-{n}"))
                .bind(t.member)
                .bind(org)
                .execute(db)
                .await
                .unwrap();
        }
        let remove_as = |by: Uuid, user: Uuid| remove_member(State(t.state.clone()), t.auth(by), Path(user));

        assert_eq!(forbidden(remove_as(t.member, t.admin).await), "Only admins can do this");
        assert_eq!(forbidden(remove_as(t.admin, t.owner).await), "Only owners can change owners");
        assert!(matches!(remove_as(t.owner, t.owner).await, Err(AppError::BadRequest(m)) if m == "The organization needs at least one owner"));
        assert!(matches!(remove_as(t.owner, t.stranger).await, Err(AppError::NotFound)));
        assert_eq!(t.role_of(t.other, t.stranger).await.as_deref(), Some("member"));
        assert_eq!(remove_as(t.admin, t.member).await.unwrap().0, json!({ "ok": true }));

        let owners = |sql: &'static str| async move { sqlx::query_scalar::<_, Option<Uuid>>(sql).fetch_all(db).await.unwrap() };
        assert_eq!(t.role_of(t.org, t.member).await, None);
        assert_eq!(t.role_of(t.other, t.member).await.as_deref(), Some("owner"));
        assert_eq!(owners("select owner_id from leads order by email").await, [None, Some(t.member)]);
        assert_eq!(owners("select owner_id from mailboxes order by email").await, [None, Some(t.member)]);
        let sessions: Vec<String> = sqlx::query_scalar("select token_hash from sessions").fetch_all(db).await.unwrap();
        assert_eq!(sessions, ["session-2"], "signed out here, still signed in to their own org");
        assert_eq!(t.audit("member.removed").await, [("mia@acme.example".to_string(), json!({ "role": "member" }))]);
        assert!(matches!(remove_as(t.admin, t.member).await, Err(AppError::NotFound)));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn invites_are_for_admins_and_never_make_an_owner(db: PgPool) {
        let t = team(db).await;
        let db = &t.state.db;
        let invite = |by: Uuid, email: &str, role: &str| {
            let (state, auth) = (t.state.clone(), t.auth(by));
            let body = InviteBody { email: email.into(), role: role.into() };
            async move {
                let Json(sent) = create_invite(State(state), auth, Json(body)).await?;
                Ok::<_, AppError>((Uuid::parse_str(sent["invite"]["id"].as_str().unwrap()).unwrap(), sent))
            }
        };
        assert_eq!(forbidden(invite(t.member, "new@acme.example", "member").await), "Only admins can do this");
        assert!(matches!(invite(t.admin, "not an address", "member").await, Err(AppError::BadRequest(m)) if m == "Enter a valid email address"));
        assert!(matches!(invite(t.owner, "new@acme.example", "owner").await, Err(AppError::BadRequest(m)) if m == "role must be one of: admin, member"));

        let (first, sent) = invite(t.admin, " New@Acme.example ", "member").await.unwrap();
        let (second, _) = invite(t.owner, "lead@acme.example", "admin").await.unwrap();
        let (accepted, _) = invite(t.owner, "joined@acme.example", "member").await.unwrap();
        let (expired, _) = invite(t.owner, "late@acme.example", "member").await.unwrap();
        let elsewhere = Auth { org_id: t.other, role: "owner".into(), ..t.auth(t.member) };
        let Json(theirs) = create_invite(State(t.state.clone()), elsewhere.clone(), Json(InviteBody { email: "x@other.example".into(), role: "member".into() })).await.unwrap();
        let saved: Vec<(Uuid, String, String)> = sqlx::query_as("select id, role, token from invites where org_id = $1 order by array_position($2, id)")
            .bind(t.org)
            .bind(&[first, second, accepted, expired][..])
            .fetch_all(db)
            .await
            .unwrap();
        assert_eq!(saved.iter().map(|(id, role, _)| (*id, role.as_str())).collect::<Vec<_>>(), [
            (first, "member"),
            (second, "admin"),
            (accepted, "member"),
            (expired, "member")
        ]);
        let tokens: std::collections::HashSet<&str> = saved.iter().map(|(_, _, token)| token.as_str()).filter(|token| !token.is_empty()).collect();
        assert_eq!(tokens.len(), 4, "each invite has its own token");
        assert_eq!(sent["link"], format!("https://app.example/invite/{}", saved[0].2));
        assert_eq!((sent["invite"]["email"].as_str(), sent["invite"]["role"].as_str()), (Some("new@acme.example"), Some("member")));
        assert!(sent["invite"].get("token").is_none(), "the token is only in the link");

        sqlx::query("update invites set accepted_at = now() where id = $1").bind(accepted).execute(db).await.unwrap();
        sqlx::query("update invites set created_at = now() - interval '1 hour' where id = $1").bind(first).execute(db).await.unwrap();
        sqlx::query("update invites set created_at = now() - interval '8 days' where id = $1").bind(expired).execute(db).await.unwrap();
        let Json(open) = list_invites(State(t.state.clone()), t.auth(t.admin)).await.unwrap();
        let open: Vec<(Uuid, chrono::Duration)> = open.into_iter().map(|i| (i.id, i.expires_at - i.created_at)).collect();
        let week = chrono::Duration::days(7);
        assert_eq!(open, [(second, week), (first, week), (expired, week)], "newest first, an expired one stays until it's revoked, accepted ones and the other org's are left out");
        assert_eq!(t.audit("member.invited").await.len(), 4);

        let theirs = Uuid::parse_str(theirs["invite"]["id"].as_str().unwrap()).unwrap();
        assert!(matches!(delete_invite(State(t.state.clone()), elsewhere, Path(second)).await, Err(AppError::NotFound)));
        assert!(matches!(delete_invite(State(t.state.clone()), t.auth(t.owner), Path(theirs)).await, Err(AppError::NotFound)));
        assert_eq!(forbidden(delete_invite(State(t.state.clone()), t.auth(t.member), Path(second)).await), "Only admins can do this");
        assert_eq!(delete_invite(State(t.state.clone()), t.auth(t.admin), Path(second)).await.unwrap().0, json!({ "ok": true }));
        assert!(matches!(delete_invite(State(t.state.clone()), t.auth(t.admin), Path(second)).await, Err(AppError::NotFound)));
        let left: i64 = sqlx::query_scalar("select count(*) from invites where id = any($1)").bind(&[first, theirs][..]).fetch_one(db).await.unwrap();
        assert_eq!(left, 2);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn only_an_owner_deletes_the_org_and_not_while_emails_are_about_to_go(db: PgPool) {
        let t = team(db).await;
        let db = &t.state.db;
        let mut projects = Vec::new();
        for (org, paused) in [(t.org, false), (t.org, true), (t.other, false)] {
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'P') returning id").bind(org).fetch_one(db).await.unwrap();
            sqlx::query("insert into project_settings (project_id, paused) values ($1, $2)").bind(project).bind(paused).execute(db).await.unwrap();
            let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, email) values ($1, 'ana@lead.example') returning id").bind(project).fetch_one(db).await.unwrap();
            projects.push((project, lead));
        }
        let (active, paused, theirs) = (projects[0], projects[1], projects[2]);
        let email = |(project, lead): (Uuid, Uuid), status: &'static str, in_minutes: Option<i32>| async move {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, direction, kind, status, scheduled_for)
                 values ($1, $2, 'outbound', 'initial', $3, now() + make_interval(mins => $4))",
            )
            .bind(project)
            .bind(lead)
            .bind(status)
            .bind(in_minutes)
            .execute(db)
            .await
            .unwrap();
        };
        let eligible = || async { deletion_eligibility(State(t.state.clone()), t.auth(t.member)).await.unwrap().0 };

        // Due later than the hour, a draft, a sent one, anything in a paused project and the other org's emails don't block it.
        email(active, "scheduled", Some(120)).await;
        email(active, "draft", Some(5)).await;
        email(active, "sent", Some(-5)).await;
        email(paused, "approved", Some(5)).await;
        email(theirs, "scheduled", Some(5)).await;
        let check = eligible().await;
        assert!(check.eligible && check.reason.is_empty(), "{}", check.reason);

        email(active, "scheduled", Some(10)).await;
        assert_eq!(eligible().await.reason, "1 email is scheduled to go out within the next hour. Pause sending on your projects first.");
        email(active, "approved", Some(30)).await;
        email(active, "sending", None).await;
        let check = eligible().await;
        assert_eq!((check.eligible, check.reason.as_str()), (false, "3 emails are scheduled to go out within the next hour. Pause sending on your projects first."));

        let delete_as = |by: Uuid| delete_org(State(t.state.clone()), t.auth(by), CookieJar::new());
        assert!(matches!(delete_as(t.owner).await, Err(AppError::Conflict(m)) if m == check.reason));
        sqlx::query("update project_settings set paused = true where project_id = $1").bind(active.0).execute(db).await.unwrap();
        assert!(eligible().await.eligible);
        assert_eq!(forbidden(delete_as(t.admin).await), "Only owners can delete the organization");
        assert_eq!(forbidden(delete_as(t.member).await), "Only owners can delete the organization");
        assert_eq!(t.orgs().await, [t.org, t.other]);

        let (jar, Json(done)) = delete_as(t.owner).await.unwrap();
        assert_eq!(done, json!({ "ok": true }));
        let cookie = jar.get(SESSION_COOKIE).expect("the session cookie is cleared");
        assert_eq!((cookie.value(), cookie.path(), cookie.max_age()), ("", Some("/"), Some(time::Duration::ZERO)));
        assert_eq!(t.orgs().await, [t.other]);
    }
}
