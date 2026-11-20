use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use super::util_input::blank_as_none;
use super::util_pagination::{PageQuery, Paginated};
use crate::auth::Auth;
use crate::error::AppResult;
use crate::models::AuditEvent;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/organization/audit", get(list))
}

#[derive(Deserialize)]
struct AuditQuery {
    /// A member's user id or an API key's id.
    #[serde(default, deserialize_with = "blank_as_none")]
    actor: Option<Uuid>,
    action: Option<String>,
}

const FILTER: &str = "from audit_events e
     left join users u on u.id = e.actor_user_id
     left join api_keys k on k.id = e.actor_key_id
     where e.org_id = $1 and ($2::uuid is null or e.actor_user_id = $2 or e.actor_key_id = $2) and ($3::text is null or e.action = $3)";

async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<AuditQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<AuditEvent>>> {
    auth.require_admin()?;
    let action = q.action.as_deref().map(str::trim).filter(|a| !a.is_empty());
    let total: i64 = sqlx::query_scalar(&format!("select count(*) {FILTER}"))
        .bind(auth.org_id)
        .bind(q.actor)
        .bind(action)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, AuditEvent>(&format!(
        "select e.id, e.action, e.actor_user_id, e.actor_key_id, coalesce(nullif(u.name, ''), u.email::text, k.name, '') as actor_name,
                e.target_type, e.target_id, e.target_label, e.details, e.ip, e.created_at
         {FILTER} order by e.created_at desc, e.id desc limit $4 offset $5"
    ))
    .bind(auth.org_id)
    .bind(q.actor)
    .bind(action)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Paginated { items, total }))
}

#[cfg(test)]
mod tests {
    use axum::extract::Path;
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;
    use crate::audit::{self, Target};
    use crate::autopilot::tests::state;
    use crate::error::AppError;
    use crate::routes::{api_keys, org};

    async fn member(state: &AppState, org_id: Uuid, email: &str, role: &str) -> Auth {
        let user_id: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, '', 'x') returning id")
            .bind(email)
            .fetch_one(&state.db)
            .await
            .unwrap();
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, $3)").bind(org_id).bind(user_id).bind(role).execute(&state.db).await.unwrap();
        Auth { user_id, org_id, role: role.into(), token_hash: String::new() }
    }

    async fn org(state: &AppState, name: &str) -> Uuid {
        sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&state.db).await.unwrap()
    }

    async fn read(state: &AppState, auth: &Auth, query: &str) -> AppResult<Paginated<AuditEvent>> {
        let uri = format!("http://splee.test/?{query}").parse().unwrap();
        let (q, page) = (Query::try_from_uri(&uri).unwrap(), Query::try_from_uri(&uri).unwrap());
        Ok(list(State(state.clone()), auth.clone(), q, page).await?.0)
    }

    fn actions(page: &Paginated<AuditEvent>) -> Vec<&str> {
        page.items.iter().map(|e| e.action.as_str()).collect()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn changes_are_logged_with_who_made_them(db: PgPool) {
        let state = state(db);
        let acme = org(&state, "Acme").await;
        let ana = member(&state, acme, "ana@acme.test", "owner").await;
        let bob = member(&state, acme, "bob@acme.test", "member").await;

        let Json(created) = api_keys::create(State(state.clone()), ana.clone(), Json(serde_json::from_value(json!({ "name": "HubSpot sync" })).unwrap()))
            .await
            .unwrap();
        let key_id: Uuid = serde_json::from_value(created["api_key"]["id"].clone()).unwrap();
        let _ = api_keys::revoke(State(state.clone()), ana.clone(), Path(key_id)).await.unwrap();
        let _ = org::update_org(State(state.clone()), ana.clone(), Json(serde_json::from_value(json!({ "name": "Acme Inc" })).unwrap())).await.unwrap();
        let role = Json(serde_json::from_value(json!({ "role": "admin" })).unwrap());
        let _ = org::update_member(State(state.clone()), ana.clone(), Path(bob.user_id), role).await.unwrap();
        // A refused change leaves nothing behind.
        let blank = Json(serde_json::from_value(json!({ "name": " " })).unwrap());
        assert!(org::update_org(State(state.clone()), ana.clone(), blank).await.is_err());

        let log = read(&state, &ana, "").await.unwrap();
        assert_eq!(actions(&log), ["member.role_changed", "org.renamed", "api_key.revoked", "api_key.created"]);
        assert_eq!(log.total, 4);
        assert!(log.items.iter().all(|e| e.actor_user_id == Some(ana.user_id) && e.actor_name == "ana@acme.test"));
        assert_eq!((log.items[0].target_label.as_str(), &log.items[0].details), ("bob@acme.test", &json!({ "from": "member", "to": "admin" })));
        assert_eq!(log.items[1].details, json!({ "from": "Acme", "to": "Acme Inc" }));
        assert_eq!((log.items[3].target_id, log.items[3].target_label.as_str()), (Some(key_id), "HubSpot sync"));
        let secret = created["secret"].as_str().unwrap();
        assert!(!serde_json::to_string(&log.items).unwrap().contains(secret), "the key itself is never logged");

        // An API key is an actor too, named by the key.
        let key = crate::auth::KeyAuth { org_id: acme, key_id };
        audit::record(&state.db, &key, "integration.connected", Target::new("integration", None, "Zapier"), json!({})).await;
        let by_key = read(&state, &ana, &format!("actor={key_id}")).await.unwrap();
        assert_eq!((actions(&by_key), by_key.items[0].actor_name.as_str()), (vec!["integration.connected"], "HubSpot sync"));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn only_admins_read_the_log_and_never_another_orgs(db: PgPool) {
        let state = state(db);
        let acme = org(&state, "Acme").await;
        let ana = member(&state, acme, "ana@acme.test", "owner").await;
        let bob = member(&state, acme, "bob@acme.test", "admin").await;
        let cat = member(&state, acme, "cat@acme.test", "member").await;
        let globex = org(&state, "Globex").await;
        let gus = member(&state, globex, "gus@globex.test", "owner").await;
        let target = || Target::new("org", None, "");
        audit::record(&state.db, &ana, "org.renamed", target(), json!({})).await;
        audit::record(&state.db, &bob, "api_key.created", target(), json!({})).await;
        audit::record(&state.db, &gus, "org.renamed", target(), json!({})).await;

        assert!(matches!(read(&state, &cat, "").await, Err(AppError::Forbidden(_))));
        assert_eq!(actions(&read(&state, &bob, "").await.unwrap()), ["api_key.created", "org.renamed"]);
        assert_eq!(read(&state, &bob, &format!("actor={}", ana.user_id)).await.unwrap().total, 1);
        assert_eq!(actions(&read(&state, &bob, "action=org.renamed&actor=").await.unwrap()), ["org.renamed"]);
        assert_eq!(read(&state, &bob, &format!("actor={}", gus.user_id)).await.unwrap().total, 0, "Globex's owner did nothing in Acme");
        assert_eq!(read(&state, &bob, "per_page=1&page=2").await.unwrap().items[0].actor_user_id, Some(ana.user_id));
        assert_eq!(read(&state, &gus, "").await.unwrap().total, 1);

        // Bob's account events show in every organization he's in, and only there.
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(globex).bind(bob.user_id).execute(&state.db).await.unwrap();
        audit::record_account(&state.db, bob.user_id, "user.signed_in", json!({})).await;
        assert_eq!(read(&state, &ana, "action=user.signed_in").await.unwrap().items[0].target_label, "bob@acme.test");
        assert_eq!(read(&state, &gus, "action=user.signed_in").await.unwrap().total, 1);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn events_go_after_a_year(db: PgPool) {
        let state = state(db);
        let acme = org(&state, "Acme").await;
        let ana = member(&state, acme, "ana@acme.test", "owner").await;
        for days in [366, 364] {
            sqlx::query("insert into audit_events (org_id, action, created_at) values ($1, $2, now() - make_interval(days => $3))")
                .bind(acme)
                .bind(format!("test.{days}"))
                .bind(days)
                .execute(&state.db)
                .await
                .unwrap();
        }
        crate::agent::expire_visitor_data(&state.db).await.unwrap();
        assert_eq!(actions(&read(&state, &ana, "").await.unwrap()), ["test.364"]);
    }
}
