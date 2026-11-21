//! The organization's audit log: who made the security- and money-relevant changes, and from where. Routes call record
//! after a change succeeds. Admins read it through GET /organization/audit (routes/audit.rs).
//!
//! Actions: user.signed_in, user.sign_in_failed, user.password_changed, user.password_reset, user.identity_linked, user.identity_unlinked,
//! user.two_factor_enabled, user.two_factor_disabled, user.backup_codes_created, member.invited, member.joined,
//! member.removed, member.role_changed, api_key.created, api_key.revoked, mailbox.connected, mailbox.removed, integration.connected, integration.disconnected,
//! sending_domain.ordered, sending_domain.removed, campaign.launched, campaign.paused, project.started, project.paused,
//! export.saved, export.removed and org.renamed. The frontend names each in app/audit/page.tsx.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, Request};
use axum::middleware::Next;
use axum::response::Response;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::{Auth, KeyAuth};
use crate::routes::demo::client_ip;

/// Older events go with the hourly visitor data cleanup (agent::expire_visitor_data).
pub const RETENTION_DAYS: i32 = 365;

tokio::task_local! {
    static CLIENT_IP: Option<String>;
}

/// Keeps the caller's address for the length of the request, so record can save it without every route passing it on.
pub async fn keep_client_ip(req: Request, next: Next) -> Response {
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .and_then(|ConnectInfo(peer)| client_ip(req.headers(), *peer).parse::<IpAddr>().ok())
        .map(|ip| ip.to_string());
    CLIENT_IP.scope(ip, next.run(req)).await
}

/// Outside a request, like in a test that calls a route directly, there's no address.
fn current_ip() -> Option<String> {
    CLIENT_IP.try_with(Clone::clone).ok().flatten()
}

/// The organization an event goes to and who caused it: a member or an API key.
#[derive(Clone, Copy)]
pub struct Who {
    org_id: Uuid,
    user_id: Option<Uuid>,
    key_id: Option<Uuid>,
}

impl Who {
    /// A member acting in an organization their session isn't in yet, like when they accept an invite.
    pub fn member(org_id: Uuid, user_id: Uuid) -> Self {
        Who { org_id, user_id: Some(user_id), key_id: None }
    }

    pub fn org_id(&self) -> Uuid {
        self.org_id
    }
}

impl From<&Auth> for Who {
    fn from(auth: &Auth) -> Self {
        Who { org_id: auth.org_id, user_id: Some(auth.user_id), key_id: None }
    }
}

impl From<&KeyAuth> for Who {
    fn from(auth: &KeyAuth) -> Self {
        Who { org_id: auth.org_id, user_id: None, key_id: Some(auth.key_id) }
    }
}

/// What an event was about. The label is how the page names it, like a key's name or a mailbox's address.
pub struct Target<'a> {
    kind: &'a str,
    id: Option<Uuid>,
    label: &'a str,
}

impl<'a> Target<'a> {
    pub fn new(kind: &'a str, id: impl Into<Option<Uuid>>, label: &'a str) -> Self {
        Target { kind, id: id.into(), label }
    }
}

/// Saves an event. A failed write is logged and never fails the change it describes, which has already happened.
pub async fn record(db: &PgPool, who: impl Into<Who>, action: &str, target: Target<'_>, details: Value) {
    let who = who.into();
    let saved = sqlx::query(
        "insert into audit_events (org_id, actor_user_id, actor_key_id, action, target_type, target_id, target_label, details, ip)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(who.org_id)
    .bind(who.user_id)
    .bind(who.key_id)
    .bind(action)
    .bind(target.kind)
    .bind(target.id)
    .bind(target.label)
    .bind(&details)
    .bind(current_ip())
    .execute(db)
    .await;
    if let Err(e) = saved {
        tracing::error!(error = ?e, action, org_id = %who.org_id, "could not record audit event");
    }
}

/// An account's own security events, like signing in, go to every organization the member is in, since each one's admins
/// answer for who can get into it.
pub async fn record_account(db: &PgPool, user_id: Uuid, action: &str, details: Value) {
    let saved = sqlx::query(
        "insert into audit_events (org_id, actor_user_id, action, target_type, target_id, target_label, details, ip)
         select m.org_id, u.id, $2, 'user', u.id, u.email::text, $3, $4 from memberships m join users u on u.id = m.user_id
         where m.user_id = $1",
    )
    .bind(user_id)
    .bind(action)
    .bind(&details)
    .bind(current_ip())
    .execute(db)
    .await;
    if let Err(e) = saved {
        tracing::error!(error = ?e, action, user_id = %user_id, "could not record audit event");
    }
}

/// A connection to another service, named as the integrations page names it, in a project or for the whole organization.
pub async fn integration(db: &PgPool, who: impl Into<Who>, project_id: Option<Uuid>, name: &str, connected: bool) {
    let action = if connected { "integration.connected" } else { "integration.disconnected" };
    let details = match project_id {
        Some(id) => {
            let project: Option<String> =
                sqlx::query_scalar("select coalesce(nullif(name, ''), domain) from projects where id = $1").bind(id).fetch_optional(db).await.ok().flatten();
            json!({ "project_id": id, "project": project })
        }
        None => json!({}),
    };
    record(db, who, action, Target::new("integration", project_id, name), details).await;
}

pub async fn prune(db: &PgPool) -> sqlx::Result<()> {
    sqlx::query("delete from audit_events where created_at < now() - make_interval(days => $1)").bind(RETENTION_DAYS).execute(db).await?;
    Ok(())
}
