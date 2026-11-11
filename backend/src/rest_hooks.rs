//! REST hook subscriptions, the way Zapier, Make and n8n hear about lead events: each subscribes a URL to one event with an
//! API key (routes/rest_hooks.rs) and gets the same signed JSON as the project's webhooks, through hooks::send_lead_event.
//! New leads are made in too many places to announce from each, so lead_added is swept from the leads table every minute.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::hooks;
use crate::models::{Lead, Ts};
use crate::state::AppState;

/// The events a subscription can be for. Keep in sync with HOOK_EVENTS on the integrations page and the apps in integrations/.
pub const EVENTS: [&str; 6] = ["hot_lead", "lead_replied", "meeting_booked", "status_changed", "deal.won", "lead_added"];

pub const LEAD_ADDED: &str = "lead_added";

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Subscription {
    pub id: Uuid,
    pub org_id: Uuid,
    pub api_key_id: Uuid,
    pub project_id: Option<Uuid>,
    pub event: String,
    pub target_url: String,
    pub error: Option<String>,
    pub failed_at: Option<Ts>,
    pub disabled_at: Option<Ts>,
    pub created_at: Ts,
}

/// A subscription an event goes to.
#[derive(Debug, Clone, FromRow)]
pub struct Target {
    pub id: Uuid,
    pub target_url: String,
}

/// Live subscriptions in the project's organization: on, with a key that isn't revoked, and for this project or all of them.
const LIVE: &str = "s.disabled_at is null and exists (select 1 from api_keys k where k.id = s.api_key_id and k.revoked_at is null)";

/// The subscriptions a project's event goes to, oldest first.
pub async fn targets(db: &PgPool, project_id: Uuid, event: &str) -> sqlx::Result<Vec<Target>> {
    sqlx::query_as(&format!(
        "select s.id, s.target_url from hook_subscriptions s join projects p on p.id = $1 and p.org_id = s.org_id
         where s.event = $2 and (s.project_id is null or s.project_id = $1) and {LIVE} order by s.created_at"
    ))
    .bind(project_id)
    .bind(event)
    .fetch_all(db)
    .await
}

/// Posts one event to a subscription, with the webhooks' retry, and keeps how it went: the error for the integrations page, and
/// the subscription turned off when the receiver answered 410 Gone. Returns false once it's off.
pub async fn deliver(db: &PgPool, project_id: Uuid, event: &str, target: &Target, payload: &Value, secret: &str) -> bool {
    let result = hooks::post_json_retrying(&target.target_url, payload, Some(secret)).await;
    if let Err(e) = &result {
        tracing::warn!(error = %e, event, "rest hook failed");
    }
    let error = result.as_ref().err().map(|e| format!("{e:#}").chars().take(500).collect::<String>());
    let gone = result.as_ref().err().is_some_and(hooks::gone);
    let saved = sqlx::query(
        "update hook_subscriptions set error = $2, failed_at = case when $2::text is null then null else now() end,
           disabled_at = case when $3 then now() else disabled_at end
         where id = $1",
    )
    .bind(target.id)
    .bind(error)
    .bind(gone)
    .execute(db)
    .await;
    if let Err(e) = saved {
        tracing::warn!(error = %e, "couldn't save the rest hook outcome");
    }
    if let Err(e) = hooks::log_delivery(db, project_id, event, &target.target_url, payload, &result).await {
        tracing::warn!(error = %e, "couldn't log the rest hook delivery");
    }
    !gone
}

/// How many new leads one subscription gets a minute. A big import reaches it over the following minutes.
const BATCH: i64 = 100;

/// Leads younger than this wait for the next sweep, so a lead an import is still saving isn't skipped past.
const SETTLE: &str = "30 seconds";

/// Posts lead_added for the leads made since each subscription's last sweep, oldest first.
pub async fn post_new_leads(state: &AppState) -> anyhow::Result<()> {
    let subscriptions: Vec<Cursor> = sqlx::query_as(&format!(
        "select s.id, s.org_id, s.project_id, s.target_url, s.cursor_at, s.cursor_id from hook_subscriptions s where s.event = $1 and {LIVE}"
    ))
    .bind(LEAD_ADDED)
    .fetch_all(&state.db)
    .await?;
    for result in futures::future::join_all(subscriptions.iter().map(|s| post_batch(state, s))).await {
        if let Err(e) = result {
            tracing::warn!(error = %e, "couldn't post new leads to a rest hook");
        }
    }
    Ok(())
}

#[derive(FromRow)]
struct Cursor {
    id: Uuid,
    org_id: Uuid,
    project_id: Option<Uuid>,
    target_url: String,
    cursor_at: Ts,
    cursor_id: Uuid,
}

async fn post_batch(state: &AppState, s: &Cursor) -> anyhow::Result<()> {
    let leads: Vec<Lead> = sqlx::query_as(&format!(
        "select l.* from leads l join projects p on p.id = l.project_id
         where p.org_id = $1 and ($2::uuid is null or l.project_id = $2) and (l.created_at, l.id) > ($3, $4)
           and l.created_at < now() - interval '{SETTLE}'
         order by l.created_at, l.id limit $5"
    ))
    .bind(s.org_id)
    .bind(s.project_id)
    .bind(s.cursor_at)
    .bind(s.cursor_id)
    .bind(BATCH)
    .fetch_all(&state.db)
    .await?;
    let Some(last) = leads.last() else { return Ok(()) };
    // Claimed by moving the cursor before posting, so a second server sweeping at the same time doesn't post them again.
    let claimed = sqlx::query("update hook_subscriptions set cursor_at = $2, cursor_id = $3 where id = $1 and cursor_at = $4 and cursor_id = $5")
        .bind(s.id)
        .bind(last.created_at)
        .bind(last.id)
        .bind(s.cursor_at)
        .bind(s.cursor_id)
        .execute(&state.db)
        .await?
        .rows_affected();
    if claimed == 0 {
        return Ok(());
    }
    let target = Target { id: s.id, target_url: s.target_url.clone() };
    let mut projects = HashMap::new();
    for lead in &leads {
        if let Entry::Vacant(e) = projects.entry(lead.project_id) {
            e.insert(project_secret(&state.db, lead.project_id).await?);
        }
        let (name, secret) = &projects[&lead.project_id];
        let payload = hooks::payload(&state.cfg.public_url, lead.project_id, name, LEAD_ADDED, lead, None);
        if !deliver(&state.db, lead.project_id, LEAD_ADDED, &target, &payload, secret).await {
            break;
        }
    }
    Ok(())
}

/// The project's name and webhook signing secret, making its settings if an older project has none yet.
async fn project_secret(db: &PgPool, project_id: Uuid) -> sqlx::Result<(String, String)> {
    sqlx::query("insert into project_settings (project_id) values ($1) on conflict do nothing").bind(project_id).execute(db).await?;
    sqlx::query_as("select p.name, s.webhook_secret from projects p join project_settings s on s.project_id = p.id where p.id = $1")
        .bind(project_id)
        .fetch_one(db)
        .await
}

/// How many samples the trigger setup screens get.
const SAMPLES: i64 = 3;

/// Recent payloads of an event in the organization, or in one of its projects, the newest first, for the setup screens of
/// Zapier, Make and n8n: what webhooks and subscriptions were sent, else for lead_added the newest leads, else one example.
pub async fn samples(db: &PgPool, public_url: &str, org_id: Uuid, project_id: Option<Uuid>, event: &str) -> sqlx::Result<Vec<Value>> {
    let sent: Vec<Value> = sqlx::query_scalar(
        "select d.payload from webhook_deliveries d join projects p on p.id = d.project_id
         where p.org_id = $1 and ($2::uuid is null or d.project_id = $2) and d.event = $3 order by d.created_at desc limit $4",
    )
    .bind(org_id)
    .bind(project_id)
    .bind(event)
    .bind(SAMPLES)
    .fetch_all(db)
    .await?;
    if !sent.is_empty() {
        return Ok(sent);
    }
    if event == LEAD_ADDED {
        let leads: Vec<Lead> = sqlx::query_as(
            "select l.* from leads l join projects p on p.id = l.project_id
             where p.org_id = $1 and ($2::uuid is null or l.project_id = $2) order by l.created_at desc limit $3",
        )
        .bind(org_id)
        .bind(project_id)
        .bind(SAMPLES)
        .fetch_all(db)
        .await?;
        if !leads.is_empty() {
            let names: HashMap<Uuid, String> = sqlx::query_as::<_, (Uuid, String)>("select id, name from projects where org_id = $1")
                .bind(org_id)
                .fetch_all(db)
                .await?
                .into_iter()
                .collect();
            return Ok(leads.iter().map(|l| hooks::payload(public_url, l.project_id, &names[&l.project_id], event, l, None)).collect());
        }
    }
    Ok(vec![example(public_url, event)])
}

/// What an event looks like when the organization has none yet. Keep in sync with PAYLOAD on the integrations page.
fn example(public_url: &str, event: &str) -> Value {
    let (project, lead) = ("9b1f6c2e-0000-4000-8000-000000000001", "c42e7a10-0000-4000-8000-000000000002");
    let status = match event {
        "hot_lead" | "deal.won" => "hot",
        "lead_replied" => "replied",
        "meeting_booked" => "meeting_booked",
        "status_changed" => "contacted",
        _ => "new",
    };
    let mut payload = json!({
        "event": event,
        "project": {"id": project, "name": "Acme"},
        "lead": {
            "id": lead, "full_name": "Sarah Chen", "email": "sarah@northwind.example", "title": "Founder", "status": status,
            "company_name": "Northwind Studio", "company_domain": "northwind.example",
            "linkedin_url": "https://www.linkedin.com/in/sarah-chen", "x_url": "", "meeting_at": null,
        },
        "reply": null,
        "url": hooks::lead_url(public_url, project.parse().unwrap_or_default(), lead.parse().unwrap_or_default()),
    });
    let classification = if event == "hot_lead" { "hot" } else { "question" };
    match event {
        "hot_lead" | "lead_replied" => {
            payload["reply"] = json!({
                "id": "71aa3d4b-0000-4000-8000-000000000003", "subject": "Re: Northwind Studio x Acme",
                "body": "Sounds interesting, can you send times for next week?", "classification": classification, "reason": "Asks to schedule a call",
            });
        }
        "meeting_booked" => payload["lead"]["meeting_at"] = json!("2026-11-17T15:00:00Z"),
        "status_changed" => payload["previous_status"] = json!("new"),
        "deal.won" => {
            payload["deal"] = json!({
                "id": "5e2d8f61-0000-4000-8000-000000000004", "stage": "Won", "previous_stage": "Proposal", "status": "won", "value": 12000.0,
                "currency": "USD", "close_date": "2026-11-20", "owner": "Alex Kim", "lost_reason": null,
                "url": format!("{public_url}/app/pipeline?deal=5e2d8f61-0000-4000-8000-000000000004"),
            });
        }
        _ => {}
    }
    payload
}
