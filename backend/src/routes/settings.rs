use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::util_input;
use crate::audit;
use crate::auth::Auth;
use crate::crm::Crm;
use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::models::{ProjectSettings, WebhookDelivery};
use crate::monthly_cap::{self, Cap};
use crate::state::AppState;
use crate::unipile;
use crate::verify::Verifier;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/settings", get(get_settings).put(update_settings))
        .route("/projects/{id}/monthly-cap", get(monthly_cap))
        .route("/projects/{id}/integrations/test", post(test_integration))
        .route("/projects/{id}/integrations/webhook-secret", post(replace_webhook_secret))
        .route("/projects/{id}/integrations/webhook-deliveries", get(list_deliveries))
        .route("/projects/{id}/integrations/webhook-deliveries/{delivery_id}/resend", post(resend_delivery))
        .route("/projects/{id}/integrations/{crm}", put(connect_crm).delete(disconnect_crm))
        // Static, so it wins over {crm} above.
        .route("/projects/{id}/integrations/verifier", put(connect_verifier).delete(disconnect_verifier))
        .route("/projects/{id}/integrations/unipile", put(connect_unipile).delete(disconnect_unipile))
}

/// Settings rows are created with the project, but older demo projects may not have one yet. In a transaction this locks
/// the row until it ends.
pub async fn load_or_create<'e>(db: impl sqlx::PgExecutor<'e>, project_id: Uuid) -> AppResult<ProjectSettings> {
    Ok(sqlx::query_as::<_, ProjectSettings>(
        "insert into project_settings (project_id) values ($1)
         on conflict (project_id) do update set project_id = excluded.project_id returning *",
    )
    .bind(project_id)
    .fetch_one(db)
    .await?)
}

async fn get_settings(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    Ok(Json(load_or_create(&state.db, id).await?))
}

/// The cap from the settings and what the month's emails cost so far, for the Daily limit page.
async fn monthly_cap(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Cap>> {
    auth.project(&state.db, id).await?;
    Ok(Json(monthly_cap::status(&state.db, id, Utc::now()).await?))
}

// `paused` is left out on purpose: unpausing goes through POST /projects/:id/start, which checks for an active mailbox.
#[derive(Deserialize, Default)]
pub(super) struct SettingsBody {
    daily_email_limit: Option<i32>,
    timezone: Option<String>,
    send_days: Option<Vec<i32>>,
    send_start_hour: Option<i32>,
    send_end_hour: Option<i32>,
    follow_ups: Option<Value>,
    booking_link: Option<String>,
    auto_reply_mode: Option<String>,
    auto_reply_instructions: Option<String>,
    sender_name: Option<String>,
    signature: Option<String>,
    tone: Option<String>,
    email_language: Option<String>,
    email_instructions: Option<String>,
    cta: Option<String>,
    max_words: Option<i32>,
    channels: Option<Value>,
    targeting: Option<Value>,
    webhook_events: Option<Vec<String>>,
    chat_weekly_report: Option<bool>,
    meeting_briefs: Option<bool>,
    nudge_quiet_deals: Option<bool>,
    deal_health: Option<bool>,
    qualify_deals: Option<bool>,
    pipeline_forecast: Option<bool>,
    coach_calls: Option<bool>,
    prep_calls: Option<bool>,
    win_back_lost_deals: Option<bool>,
    write_battlecards: Option<bool>,
    grow_accounts: Option<bool>,
    ask_for_referrals: Option<bool>,
    watch_churn: Option<bool>,
    prep_renewals: Option<bool>,
    /// Empty turns it off.
    bcc_email: Option<String>,
    autopilot: Option<bool>,
    autopilot_budget_micros: Option<i64>,
    autopilot_lead_cap: Option<i32>,
    autopilot_auto_send: Option<bool>,
    /// A present null turns the review window off.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    review_window_hours: Option<Option<i32>>,
    /// A present null removes the cap.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    monthly_cap_micros: Option<Option<i64>>,
    match_provider: Option<bool>,
    lead_time_zone: Option<bool>,
    /// Sorted and deduplicated on save.
    days_off: Option<Vec<NaiveDate>>,
    stop_company_on_reply: Option<bool>,
    /// A present null removes the limit.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    company_daily_limit: Option<Option<i32>>,
    /// Deduplicated on save. A present null for all of the org's mailboxes.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    mailbox_ids: Option<Option<Vec<Uuid>>>,
    text_steps: Option<Value>,
    daily_text_limit: Option<i32>,
    assign_rule: Option<String>,
    /// Round robin's members, in the order it goes through them.
    assign_members: Option<Vec<Uuid>>,
    assign_territories: Option<Vec<crate::ownership::Territory>>,
}

/// Send days, 1 (Monday) to 7, and the hours between, a project's or a campaign's.
pub(super) fn check_send_window(days: &[i32], start: i32, end: i32) -> Result<(), &'static str> {
    if days.iter().any(|d| !(1..=7).contains(d)) {
        return Err("Send days must be between 1 (Monday) and 7 (Sunday)");
    }
    if !(0..=23).contains(&start) || !(1..=24).contains(&end) || start >= end {
        return Err("Send window must start before it ends, within 0-24h");
    }
    Ok(())
}

/// Up to 5 follow-ups, a project's or a campaign's, each an email or a call 1 to 30 days after the step before, switched on or
/// off. A call step opens a call for the lead's owner instead of sending anything. The writer and the schedulers read this shape,
/// so a step missing `enabled` or `delay_days` would quietly stop the sequence.
pub(super) fn check_follow_ups(steps: &Value) -> Result<(), &'static str> {
    let steps = steps.as_array().ok_or("follow_ups must be a list")?;
    if steps.len() > 5 {
        return Err("A sequence can have up to 5 follow-ups");
    }
    for step in steps {
        if !matches!(step.get("kind").map(|k| k.as_str()), None | Some(Some("email" | "call"))) {
            return Err("Each follow-up is an email or a call");
        }
        if !step["delay_days"].as_i64().is_some_and(|d| (1..=30).contains(&d)) {
            return Err("Each follow-up's delay must be 1 to 30 days");
        }
        if !step["enabled"].is_boolean() {
            return Err("Each follow-up needs enabled, true or false");
        }
        if !matches!(step.get("instructions"), None | Some(Value::String(_))) {
            return Err("A follow-up's instructions must be text");
        }
    }
    Ok(())
}

/// Up to 5 texts, each SMS or WhatsApp, 1 to 30 days after the one before.
fn check_text_steps(steps: &Value) -> Result<(), &'static str> {
    let steps = steps.as_array().ok_or("text_steps must be a list")?;
    if steps.len() > 5 {
        return Err("A sequence can have up to 5 texts");
    }
    for step in steps {
        if !matches!(step["channel"].as_str(), Some("sms" | "whatsapp")) {
            return Err("Each text is SMS or WhatsApp");
        }
        if !step["delay_days"].as_i64().is_some_and(|d| (1..=30).contains(&d)) {
            return Err("Each text's delay must be 1 to 30 days");
        }
        if step["instructions"].as_str().is_some_and(|i| i.chars().count() > 1000) {
            return Err("Keep each text's instructions under 1,000 characters");
        }
    }
    Ok(())
}

/// The sending controls PATCH /v1/projects/{id} changes. A field it doesn't know is a 422, so a typo never passes as a change.
/// `sending` is the project's Start and Pause sending buttons, which routes/projects.rs runs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Controls {
    pub(super) sending: Option<bool>,
    daily_email_limit: Option<i32>,
    daily_text_limit: Option<i32>,
    timezone: Option<String>,
    send_days: Option<Vec<i32>>,
    send_start_hour: Option<i32>,
    send_end_hour: Option<i32>,
    autopilot: Option<bool>,
    autopilot_budget_micros: Option<i64>,
    autopilot_lead_cap: Option<i32>,
    autopilot_auto_send: Option<bool>,
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    review_window_hours: Option<Option<i32>>,
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    monthly_cap_micros: Option<Option<i64>>,
    match_provider: Option<bool>,
    lead_time_zone: Option<bool>,
    days_off: Option<Vec<NaiveDate>>,
    stop_company_on_reply: Option<bool>,
    /// A present null removes the limit.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    company_daily_limit: Option<Option<i32>>,
    sender_name: Option<String>,
}

impl From<Controls> for SettingsBody {
    fn from(c: Controls) -> Self {
        Self {
            daily_email_limit: c.daily_email_limit,
            daily_text_limit: c.daily_text_limit,
            timezone: c.timezone,
            send_days: c.send_days,
            send_start_hour: c.send_start_hour,
            send_end_hour: c.send_end_hour,
            autopilot: c.autopilot,
            autopilot_budget_micros: c.autopilot_budget_micros,
            autopilot_lead_cap: c.autopilot_lead_cap,
            autopilot_auto_send: c.autopilot_auto_send,
            review_window_hours: c.review_window_hours,
            monthly_cap_micros: c.monthly_cap_micros,
            match_provider: c.match_provider,
            lead_time_zone: c.lead_time_zone,
            days_off: c.days_off,
            stop_company_on_reply: c.stop_company_on_reply,
            company_daily_limit: c.company_daily_limit,
            sender_name: c.sender_name,
            ..Default::default()
        }
    }
}

async fn update_settings(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<SettingsBody>,
) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    Ok(Json(apply(&state.db, auth.org_id, id, body).await?))
}

/// Each one stretches how far ahead mail::SendHours::opens looks, so keep it to a few years of holidays.
const MAX_DAYS_OFF: usize = 100;

/// Saves the fields the body fills in, with the settings page's checks, for a project the caller already checked.
pub(super) async fn apply(db: &PgPool, org_id: Uuid, id: Uuid, body: SettingsBody) -> AppResult<ProjectSettings> {
    // The row stays locked until the save, so two saves that each send one end of the send window are checked against what
    // the other saved and neither undoes the other. Every query here goes through `tx`: waiting on another pool connection
    // while holding the lock could leave the pool full of saves waiting on it.
    let mut tx = db.begin().await?;
    let current = load_or_create(&mut *tx, id).await?;
    let bad = |m: &str| Err(AppError::BadRequest(m.into()));

    if body.daily_email_limit.is_some_and(|n| !(1..=5000).contains(&n)) {
        return bad("Daily email limit must be between 1 and 5,000");
    }
    if let Some(tz) = &body.timezone {
        let known: bool = sqlx::query_scalar("select exists(select 1 from pg_timezone_names where name = $1)")
            .bind(tz)
            .fetch_one(&mut *tx)
            .await?;
        if !known {
            return bad("Unknown timezone");
        }
    }
    let days = body.send_days.as_deref().unwrap_or(&current.send_days[..]);
    let start = body.send_start_hour.unwrap_or(current.send_start_hour);
    let end = body.send_end_hour.unwrap_or(current.send_end_hour);
    if let Err(m) = check_send_window(days, start, end) {
        return bad(m);
    }
    // Every settings page sends the whole form back, so only a change is checked. Steps saved before the check got stricter
    // don't block saving something else.
    if let Some(Err(m)) = body.follow_ups.as_ref().filter(|f| **f != current.follow_ups).map(check_follow_ups) {
        return bad(m);
    }
    if let Some(Err(m)) = body.text_steps.as_ref().map(check_text_steps) {
        return bad(m);
    }
    if body.daily_text_limit.is_some_and(|n| !(1..=1000).contains(&n)) {
        return bad("Daily text limit must be between 1 and 1,000");
    }
    if body.channels.as_ref().is_some_and(|c| !c.is_object()) || body.targeting.as_ref().is_some_and(|t| !t.is_object()) {
        return bad("channels and targeting must be objects");
    }
    if let Some(mode) = &body.auto_reply_mode {
        util_input::one_of("auto_reply_mode", mode, &["off", "approve", "auto"])?;
    }
    if body.max_words.is_some_and(|w| !(20..=500).contains(&w)) {
        return bad("Max words must be between 20 and 500");
    }
    for event in body.webhook_events.iter().flatten() {
        util_input::one_of("webhook_events", event, &hooks::EVENTS)?;
    }
    if body.autopilot_budget_micros.is_some_and(|m| !(0..=100_000_000_000).contains(&m)) {
        return bad("Autopilot budget must be between $0 and $100,000 a month");
    }
    if body.autopilot_lead_cap.is_some_and(|n| !(0..=100_000).contains(&n)) {
        return bad("Autopilot lead cap must be between 0 and 100,000 a month");
    }
    if body.review_window_hours.flatten().is_some_and(|h| !(1..=72).contains(&h)) {
        return bad("The review window must be between 1 and 72 hours");
    }
    if body.monthly_cap_micros.flatten().is_some_and(|m| !(1_000_000..=1_000_000_000_000).contains(&m)) {
        return bad("The monthly cap must be between $1 and $1,000,000");
    }
    if body.company_daily_limit.flatten().is_some_and(|n| !(1..=50).contains(&n)) {
        return bad("First emails per company must be between 1 and 50 a day");
    }
    let days_off = body.days_off.clone().map(|mut days| {
        days.sort();
        days.dedup();
        days
    });
    if days_off.as_ref().is_some_and(|days| days.len() > MAX_DAYS_OFF) {
        return bad("A project can have up to 100 days off");
    }
    let sender_name = body.sender_name.as_deref().map(|n| n.split_whitespace().collect::<Vec<_>>().join(" "));
    // Checked on what was sent, since collapsing the spaces would hide a newline or a tab.
    let control = body.sender_name.as_deref().is_some_and(|n| n.chars().any(char::is_control));
    if control || sender_name.as_deref().is_some_and(|n| n.chars().count() > 100 || n.contains('@')) {
        return bad("The sender name must be a person's or team's name of up to 100 characters, not an email address");
    }
    let mailbox_ids = body.mailbox_ids.clone().flatten().map(|mut ids| {
        ids.sort();
        ids.dedup();
        ids
    });
    if let Some(ids) = &mailbox_ids {
        let theirs: i64 =
            sqlx::query_scalar("select count(*) from mailboxes where org_id = $1 and id = any($2)").bind(org_id).bind(ids).fetch_one(&mut *tx).await?;
        if theirs != ids.len() as i64 {
            return bad("Pick mailboxes connected to this organization");
        }
    }
    if let Some(rule) = &body.assign_rule {
        util_input::one_of("assign_rule", rule, crate::ownership::RULES)?;
    }
    let (assign_members, assign_territories) = match (body.assign_members, body.assign_territories) {
        (None, None) => (None, None),
        (members, territories) => {
            let saved: crate::ownership::Assignment =
                sqlx::query_as("select assign_members, assign_territories from project_settings where project_id = $1").bind(id).fetch_one(&mut *tx).await?;
            let members = match members {
                Some(members) => Some(crate::ownership::check_members(&mut tx, org_id, members, &saved).await?),
                None => None,
            };
            let territories = match territories {
                Some(territories) => Some(sqlx::types::Json(crate::ownership::check_territories(&mut tx, org_id, territories, &saved).await?)),
                None => None,
            };
            (members, territories)
        }
    };
    let bcc_email = match body.bcc_email.as_deref().map(str::trim) {
        Some("") => Some(String::new()),
        Some(raw) => match util_input::email(raw) {
            Some(email) => Some(email),
            None => return bad("The BCC address isn't a valid email address"),
        },
        None => None,
    };

    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set
           daily_email_limit = coalesce($2, daily_email_limit),
           timezone = coalesce($3, timezone),
           send_days = coalesce($4, send_days),
           send_start_hour = $5,
           send_end_hour = $6,
           follow_ups = coalesce($7, follow_ups),
           booking_link = coalesce($8, booking_link),
           auto_reply_mode = coalesce($9, auto_reply_mode),
           auto_reply_instructions = coalesce($10, auto_reply_instructions),
           sender_name = coalesce($11, sender_name),
           signature = coalesce($12, signature),
           tone = coalesce($13, tone),
           email_language = coalesce($14, email_language),
           email_instructions = coalesce($15, email_instructions),
           cta = coalesce($16, cta),
           max_words = coalesce($17, max_words),
           channels = coalesce($18, channels),
           targeting = coalesce($19, targeting),
           score_inputs_at = case when $19 is distinct from targeting and $19 is not null then now() else score_inputs_at end,
           webhook_events = coalesce($20, webhook_events),
           chat_weekly_report = coalesce($21, chat_weekly_report),
           bcc_error = case when $22::text <> bcc_email then null else bcc_error end,
           bcc_failed_at = case when $22::text <> bcc_email then null else bcc_failed_at end,
           bcc_email = coalesce($22, bcc_email),
           autopilot = coalesce($23, autopilot),
           autopilot_budget_micros = coalesce($24, autopilot_budget_micros),
           autopilot_lead_cap = coalesce($25, autopilot_lead_cap),
           autopilot_auto_send = coalesce($26, autopilot_auto_send),
           text_steps = coalesce($27, text_steps),
           daily_text_limit = coalesce($28, daily_text_limit),
           assign_rule = coalesce($29, assign_rule),
           assign_members = coalesce($30, assign_members),
           meeting_briefs = coalesce($31, meeting_briefs),
           nudge_quiet_deals = coalesce($32, nudge_quiet_deals),
           deal_health = coalesce($33, deal_health),
           qualify_deals = coalesce($34, qualify_deals),
           pipeline_forecast = coalesce($35, pipeline_forecast),
           coach_calls = coalesce($36, coach_calls),
           prep_calls = coalesce($37, prep_calls),
           win_back_lost_deals = coalesce($38, win_back_lost_deals),
           write_battlecards = coalesce($39, write_battlecards),
           grow_accounts = coalesce($40, grow_accounts),
           ask_for_referrals = coalesce($41, ask_for_referrals),
           watch_churn = coalesce($42, watch_churn),
           prep_renewals = coalesce($43, prep_renewals),
           review_window_hours = case when $44 then $45 else review_window_hours end,
           monthly_cap_micros = case when $46 then $47 else monthly_cap_micros end,
           assign_territories = coalesce($48, assign_territories),
           match_provider = coalesce($49, match_provider),
           lead_time_zone = coalesce($50, lead_time_zone),
           days_off = coalesce($51, days_off),
           stop_company_on_reply = coalesce($52, stop_company_on_reply),
           mailbox_ids = case when $53 then $54 else mailbox_ids end,
           company_daily_limit = case when $55 then $56 else company_daily_limit end
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(body.daily_email_limit)
    .bind(body.timezone)
    .bind(body.send_days)
    .bind(start)
    .bind(end)
    .bind(body.follow_ups)
    .bind(body.booking_link.as_deref().map(str::trim))
    .bind(body.auto_reply_mode)
    .bind(body.auto_reply_instructions)
    .bind(sender_name)
    .bind(body.signature)
    .bind(body.tone)
    .bind(body.email_language)
    .bind(body.email_instructions)
    .bind(body.cta)
    .bind(body.max_words)
    .bind(body.channels)
    .bind(body.targeting)
    .bind(body.webhook_events)
    .bind(body.chat_weekly_report)
    .bind(bcc_email)
    .bind(body.autopilot)
    .bind(body.autopilot_budget_micros)
    .bind(body.autopilot_lead_cap)
    .bind(body.autopilot_auto_send)
    .bind(body.text_steps)
    .bind(body.daily_text_limit)
    .bind(body.assign_rule)
    .bind(assign_members)
    .bind(body.meeting_briefs)
    .bind(body.nudge_quiet_deals)
    .bind(body.deal_health)
    .bind(body.qualify_deals)
    .bind(body.pipeline_forecast)
    .bind(body.coach_calls)
    .bind(body.prep_calls)
    .bind(body.win_back_lost_deals)
    .bind(body.write_battlecards)
    .bind(body.grow_accounts)
    .bind(body.ask_for_referrals)
    .bind(body.watch_churn)
    .bind(body.prep_renewals)
    .bind(body.review_window_hours.is_some())
    .bind(body.review_window_hours.flatten())
    .bind(body.monthly_cap_micros.is_some())
    .bind(body.monthly_cap_micros.flatten())
    .bind(assign_territories)
    .bind(body.match_provider)
    .bind(body.lead_time_zone)
    .bind(days_off)
    .bind(body.stop_company_on_reply)
    .bind(body.mailbox_ids.is_some())
    .bind(mailbox_ids)
    .bind(body.company_daily_limit.is_some())
    .bind(body.company_daily_limit.flatten())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(settings)
}

#[derive(Deserialize)]
struct TestBody {
    target: String,
    url: String,
}

/// Posts a sample alert to the URL in the form, saved or not, so a team can check the hookup without waiting for a hot lead.
async fn test_integration(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<TestBody>) -> AppResult<Json<Value>> {
    let project = auth.project(&state.db, id).await?;
    util_input::one_of("target", &body.target, &["webhook", "slack"])?;
    let name = if project.name.is_empty() { project.domain.clone().unwrap_or_default() } else { project.name.clone() };
    let link = format!("{}/app/p/{}/inbox", state.cfg.public_url, project.id);
    // Same shape as a hot_lead event from hooks::lead_event, so tools like Zapier can map fields from it.
    let payload = if body.target == "slack" {
        let summary = format!("hot-lead alerts for {name} will post in this channel.");
        hooks::chat_payload(&body.url, &hooks::ChatAlert { label: "Splee test", summary: &summary, quote: None, link: &link, link_text: "Open the inbox" })
    } else {
        json!({
            "event": "test",
            "project": {"id": project.id, "name": name},
            "lead": {
                "id": Uuid::nil(), "full_name": "Sarah Chen", "email": "sarah@northwind.example", "title": "Founder",
                "status": "hot", "company_name": "Northwind Studio", "company_domain": "northwind.example",
                "linkedin_url": "https://www.linkedin.com/in/sarah-chen-example", "x_url": "https://x.com/sarahchen_example",
            },
            "reply": {
                "id": Uuid::nil(), "subject": "Re: Northwind Studio x Splee", "body": "This is a test alert from Splee.",
                "classification": "hot", "reason": "Test",
            },
            "url": link,
        })
    };
    let target = if body.target == "slack" { hooks::chat_endpoint(&body.url) } else { body.url.clone() };
    let alert = if body.target == "webhook" { hooks::Alert::Webhook } else { hooks::Alert::Chat };
    let secret = if body.target == "webhook" { Some(load_or_create(&state.db, project.id).await?.webhook_secret) } else { None };
    hooks::post_json(&target, &payload, secret.as_deref()).await.map_err(|e| AppError::BadRequest(format!("The test didn't go through: {e:#}")))?;
    // A passing test on the saved URL clears its last failure, so fixing an endpoint and testing it is enough. A failed test only shows in the toast.
    hooks::record(&state.db, project.id, alert, &body.url, &Ok(())).await;
    Ok(Json(json!({ "ok": true })))
}

async fn list_deliveries(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<WebhookDelivery>>> {
    auth.project(&state.db, id).await?;
    let deliveries = sqlx::query_as::<_, WebhookDelivery>("select * from webhook_deliveries where project_id = $1 order by created_at desc")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(deliveries))
}

/// Posts a past delivery's payload again, signed afresh, to the endpoint it went to. It's logged as a new delivery.
async fn resend_delivery(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, delivery_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<WebhookDelivery>> {
    auth.project(&state.db, id).await?;
    let past = sqlx::query_as::<_, WebhookDelivery>("select * from webhook_deliveries where id = $1 and project_id = $2")
        .bind(delivery_id)
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let listed: bool = sqlx::query_scalar("select exists (select 1 from webhook_endpoints where project_id = $1 and url = $2)")
        .bind(id)
        .bind(&past.url)
        .fetch_one(&state.db)
        .await?;
    if !listed {
        return Err(AppError::BadRequest("That endpoint isn't in the list any more".into()));
    }
    let settings = load_or_create(&state.db, id).await?;
    let result = hooks::post_json(&past.url, &past.payload, Some(&settings.webhook_secret)).await;
    hooks::record(&state.db, id, hooks::Alert::Webhook, &past.url, &result).await;
    Ok(Json(hooks::log_delivery(&state.db, id, &past.event, &past.url, &past.payload, &result).await?))
}

/// For a secret that leaked. Requests signed with the old one stop verifying right away.
async fn replace_webhook_secret(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set webhook_secret = encode(gen_random_bytes(24), 'hex') where project_id = $1 returning *",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct CrmBody {
    token: String,
    /// For a CRM with Crm::account.
    #[serde(default)]
    account: String,
}

fn crm(name: &str) -> AppResult<Crm> {
    Crm::parse(name).ok_or(AppError::NotFound)
}

/// Saves a CRM token once the CRM accepts it, so a typo shows up here and not on the first hot lead.
async fn connect_crm(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<CrmBody>,
) -> AppResult<Json<ProjectSettings>> {
    let crm = crm(&name)?;
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    if crm.signs_in() {
        return Err(AppError::BadRequest(format!("Connect {} by signing in", crm.label())));
    }
    let token = body.token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest(format!("Paste your {} token", crm.label())));
    }
    let token = match crm.account() {
        Some(what) => {
            let account = body.account.trim();
            if account.is_empty() {
                return Err(AppError::BadRequest(format!("Paste your {} {what}", crm.label())));
            }
            format!("{account}\n{token}")
        }
        None => token.to_string(),
    };
    crm.check(&token).await.map_err(|e| AppError::BadRequest(format!("{} didn't accept that: {e:#}", crm.label())))?;
    let saved = set_crm_token(&state.db, id, crm, &crypto::encrypt(&state.cfg.app_secret, &token)).await?;
    audit::integration(&state.db, &auth, Some(id), crm.label(), true).await;
    Ok(saved)
}

async fn disconnect_crm(State(state): State<AppState>, auth: Auth, Path((id, name)): Path<(Uuid, String)>) -> AppResult<Json<ProjectSettings>> {
    let crm = crm(&name)?;
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    if crm.signs_in() {
        crate::crm::signin::disconnect(&state.db, &state.cfg, id, crm).await?;
        audit::integration(&state.db, &auth, Some(id), crm.label(), false).await;
        return Ok(Json(load_or_create(&state.db, id).await?));
    }
    let saved = set_crm_token(&state.db, id, crm, "").await?;
    audit::integration(&state.db, &auth, Some(id), crm.label(), false).await;
    Ok(saved)
}

async fn set_crm_token(db: &PgPool, id: Uuid, crm: Crm, enc: &str) -> AppResult<Json<ProjectSettings>> {
    // crm.name() is a fixed string, never user input.
    let n = crm.name();
    let settings = sqlx::query_as::<_, ProjectSettings>(&format!(
        "update project_settings set {n}_token_enc = $2, {n}_error = null, {n}_failed_at = null where project_id = $1 returning *"
    ))
    .bind(id)
    .bind(enc)
    .fetch_one(db)
    .await?;
    Ok(Json(settings))
}

/// A service and its API key, for Email verification.
#[derive(Deserialize)]
struct ServiceKey {
    provider: String,
    key: String,
}

/// Saves an email verification key once the service accepts it.
async fn connect_verifier(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<ServiceKey>,
) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let verifier = Verifier::parse(&body.provider).ok_or_else(|| AppError::BadRequest("Pick a verification service".into()))?;
    let key = body.key.trim();
    if key.is_empty() {
        return Err(AppError::BadRequest(format!("Paste your {} API key", verifier.label())));
    }
    verifier.check_key(key).await.map_err(|e| AppError::BadRequest(format!("{} didn't accept that key: {e:#}", verifier.label())))?;
    let saved = set_verifier(&state.db, id, &body.provider, &crypto::encrypt(&state.cfg.app_secret, key)).await?;
    audit::integration(&state.db, &auth, Some(id), verifier.label(), true).await;
    Ok(saved)
}

async fn disconnect_verifier(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    let current = load_or_create(&state.db, id).await?.verifier;
    let saved = set_verifier(&state.db, id, "", "").await?;
    if let Some(verifier) = Verifier::parse(&current) {
        audit::integration(&state.db, &auth, Some(id), verifier.label(), false).await;
    }
    Ok(saved)
}

#[derive(Deserialize)]
struct UnipileBody {
    dsn: String,
    token: String,
}

/// Saves a Unipile token with the LinkedIn account it opens, once Unipile accepts it.
async fn connect_unipile(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<UnipileBody>,
) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let base = unipile::base_url(&body.dsn)
        .ok_or_else(|| AppError::BadRequest("Paste the DSN from your Unipile dashboard, like api8.unipile.com:13851".into()))?;
    let token = body.token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("Paste your Unipile access token".into()));
    }
    let (account, name) =
        unipile::find_account(&base, token).await.map_err(|e| AppError::BadRequest(format!("Unipile didn't accept that: {e:#}")))?;
    remove_unipile_webhooks(&state, id).await?;
    let hook_token: String =
        sqlx::query_scalar("select unipile_webhook_token from project_settings where project_id = $1").bind(id).fetch_one(&state.db).await?;
    let url = format!("{}/api/unipile/{hook_token}", state.cfg.public_url.trim_end_matches('/'));
    let hooks = unipile::add_webhooks(&base, token, &account, &url)
        .await
        .map_err(|e| AppError::BadRequest(format!("Unipile didn't take Splee's webhook for LinkedIn replies: {e:#}")))?;
    let saved = set_unipile(&state.db, id, &base, &crypto::encrypt(&state.cfg.app_secret, token), &account, &name, &hooks).await?;
    audit::integration(&state.db, &auth, Some(id), "LinkedIn (Unipile)", true).await;
    Ok(saved)
}

async fn disconnect_unipile(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    remove_unipile_webhooks(&state, id).await?;
    let saved = set_unipile(&state.db, id, "", "", "", "", &[]).await?;
    audit::integration(&state.db, &auth, Some(id), "LinkedIn (Unipile)", false).await;
    Ok(saved)
}

/// The webhooks a previous connection made, so a reconnect or disconnect doesn't leave Unipile posting to Splee.
async fn remove_unipile_webhooks(state: &AppState, id: Uuid) -> AppResult<()> {
    let (base, enc, ids): (String, String, Vec<String>) =
        sqlx::query_as("select unipile_dsn, unipile_token_enc, unipile_webhook_ids from project_settings where project_id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    if let (false, Ok(token)) = (ids.is_empty(), crypto::decrypt(&state.cfg.app_secret, &enc)) {
        unipile::remove_webhooks(&base, &token, &ids).await;
    }
    Ok(())
}

async fn set_unipile(db: &PgPool, id: Uuid, base: &str, enc: &str, account: &str, name: &str, hooks: &[String]) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set unipile_dsn = $2, unipile_token_enc = $3, unipile_account_id = $4, unipile_account_name = $5,
           unipile_webhook_ids = $6, unipile_error = null, unipile_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(base)
    .bind(enc)
    .bind(account)
    .bind(name)
    .bind(hooks)
    .fetch_one(db)
    .await?;
    Ok(Json(settings))
}

async fn set_verifier(db: &PgPool, id: Uuid, provider: &str, enc: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set verifier = $2, verifier_key_enc = $3, verifier_error = null, verifier_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(provider)
    .bind(enc)
    .fetch_one(db)
    .await?;
    Ok(Json(settings))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Duration;

    use super::*;
    use crate::config::Config;

    /// A project with no settings row yet, two mailboxes in its org, and another org with a project and a mailbox of its own.
    struct Seeded {
        state: AppState,
        auth: Auth,
        project: Uuid,
        mailboxes: [Uuid; 2],
        other: Auth,
        other_project: Uuid,
        other_mailbox: Uuid,
    }

    async fn seed(db: PgPool) -> Seeded {
        let mut orgs = Vec::new();
        for name in ["Acme", "Other"] {
            let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap();
            let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, $2, 'x') returning id")
                .bind(format!("owner@{}.example", name.to_lowercase()))
                .bind(name)
                .fetch_one(&db)
                .await
                .unwrap();
            sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(user).execute(&db).await.unwrap();
            let project: Uuid =
                sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(&db).await.unwrap();
            let mut mailboxes = Vec::new();
            for n in 1..=2 {
                let mailbox: Uuid = sqlx::query_scalar(
                    "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, $2, 'smtp.example', '', '') returning id",
                )
                .bind(org)
                .bind(format!("box{n}@{}.example", name.to_lowercase()))
                .fetch_one(&db)
                .await
                .unwrap();
                mailboxes.push(mailbox);
            }
            orgs.push((Auth { user_id: user, org_id: org, role: "owner".into(), token_hash: String::new() }, project, mailboxes));
        }
        let (other, other_project, other_mailboxes) = orgs.pop().unwrap();
        let (auth, project, mailboxes) = orgs.pop().unwrap();
        let state = AppState { db, cfg: Arc::new(Config::test()), events: tokio::sync::broadcast::channel(16).0, rate_limits: Default::default() };
        Seeded { state, auth, project, mailboxes: [mailboxes[0], mailboxes[1]], other, other_project, other_mailbox: other_mailboxes[0] }
    }

    impl Seeded {
        async fn save(&self, body: Value) -> AppResult<ProjectSettings> {
            let body = serde_json::from_value(body).unwrap();
            update_settings(State(self.state.clone()), self.auth.clone(), Path(self.project), Json(body)).await.map(|Json(s)| s)
        }

        /// The whole settings row, to show a refused save changed nothing.
        async fn row(&self) -> Value {
            sqlx::query_scalar("select to_jsonb(s) from project_settings s where project_id = $1").bind(self.project).fetch_one(&self.state.db).await.unwrap()
        }

        async fn audit(&self) -> Vec<(String, String)> {
            // Every org's, so an event filed under the wrong org shows up too.
            sqlx::query_as("select action, target_label from audit_events order by created_at")
                .fetch_all(&self.state.db)
                .await
                .unwrap()
        }
    }

    fn refused<T>(result: AppResult<T>) -> String {
        match result {
            Err(AppError::BadRequest(m)) => m,
            Err(e) => panic!("{e:?}"),
            Ok(_) => panic!("saved"),
        }
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_refused_setting_saves_nothing(db: PgPool) {
        let s = seed(db).await;
        let Json(created) = get_settings(State(s.state.clone()), s.auth.clone(), Path(s.project)).await.unwrap();
        assert_eq!((created.daily_email_limit, created.send_start_hour), (125, 9), "the first read makes the row with its defaults");
        let before = s.row().await;

        let step = |extra: Value| {
            let mut step = json!({ "delay_days": 3, "enabled": true, "instructions": "" });
            step.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            step
        };
        let text = |channel: &str, delay: i32, instructions: String| json!({ "channel": channel, "delay_days": delay, "instructions": instructions });
        let window = "Send window must start before it ends, within 0-24h";
        let days: Vec<String> = (0..101).map(|d| (NaiveDate::from_ymd_opt(2027, 1, 1).unwrap() + Duration::days(d)).to_string()).collect();
        let cases = [
            (json!({ "daily_email_limit": 0 }), "Daily email limit must be between 1 and 5,000".to_string()),
            (json!({ "daily_email_limit": 5001 }), "Daily email limit must be between 1 and 5,000".into()),
            (json!({ "timezone": "Mars/Olympus_Mons" }), "Unknown timezone".into()),
            (json!({ "send_days": [1, 8] }), "Send days must be between 1 (Monday) and 7 (Sunday)".into()),
            (json!({ "send_days": [0] }), "Send days must be between 1 (Monday) and 7 (Sunday)".into()),
            // The saved window is 9 to 17, so moving only one end is checked against the other.
            (json!({ "send_start_hour": 17 }), window.into()),
            (json!({ "send_end_hour": 9 }), window.into()),
            (json!({ "send_start_hour": 0, "send_end_hour": 25 }), window.into()),
            (json!({ "send_start_hour": -1 }), window.into()),
            (json!({ "follow_ups": {} }), "follow_ups must be a list".into()),
            (json!({ "follow_ups": vec![step(json!({})); 6] }), "A sequence can have up to 5 follow-ups".into()),
            (json!({ "follow_ups": [step(json!({ "kind": "sms" }))] }), "Each follow-up is an email or a call".into()),
            (json!({ "follow_ups": [step(json!({ "delay_days": 0 }))] }), "Each follow-up's delay must be 1 to 30 days".into()),
            (json!({ "follow_ups": [step(json!({ "delay_days": 31 }))] }), "Each follow-up's delay must be 1 to 30 days".into()),
            (json!({ "follow_ups": [{ "delay_days": 3 }] }), "Each follow-up needs enabled, true or false".into()),
            (json!({ "follow_ups": [step(json!({ "instructions": 5 }))] }), "A follow-up's instructions must be text".into()),
            (json!({ "text_steps": "sms" }), "text_steps must be a list".into()),
            (json!({ "text_steps": vec![text("sms", 2, String::new()); 6] }), "A sequence can have up to 5 texts".into()),
            (json!({ "text_steps": [text("fax", 2, String::new())] }), "Each text is SMS or WhatsApp".into()),
            (json!({ "text_steps": [text("whatsapp", 31, String::new())] }), "Each text's delay must be 1 to 30 days".into()),
            (json!({ "text_steps": [text("sms", 2, "a".repeat(1001))] }), "Keep each text's instructions under 1,000 characters".into()),
            (json!({ "daily_text_limit": 1001 }), "Daily text limit must be between 1 and 1,000".into()),
            (json!({ "channels": [] }), "channels and targeting must be objects".into()),
            (json!({ "targeting": "founders" }), "channels and targeting must be objects".into()),
            (json!({ "auto_reply_mode": "sometimes" }), "auto_reply_mode must be one of: off, approve, auto".into()),
            (json!({ "max_words": 19 }), "Max words must be between 20 and 500".into()),
            (json!({ "max_words": 501 }), "Max words must be between 20 and 500".into()),
            (json!({ "webhook_events": [hooks::EVENTS[0], "lead.exploded"] }), format!("webhook_events must be one of: {}", hooks::EVENTS.join(", "))),
            (json!({ "autopilot_budget_micros": -1 }), "Autopilot budget must be between $0 and $100,000 a month".into()),
            (json!({ "autopilot_budget_micros": 100_000_000_001_i64 }), "Autopilot budget must be between $0 and $100,000 a month".into()),
            (json!({ "autopilot_lead_cap": 100_001 }), "Autopilot lead cap must be between 0 and 100,000 a month".into()),
            (json!({ "review_window_hours": 0 }), "The review window must be between 1 and 72 hours".into()),
            (json!({ "review_window_hours": 73 }), "The review window must be between 1 and 72 hours".into()),
            (json!({ "monthly_cap_micros": 999_999 }), "The monthly cap must be between $1 and $1,000,000".into()),
            (json!({ "monthly_cap_micros": 1_000_000_000_001_i64 }), "The monthly cap must be between $1 and $1,000,000".into()),
            (json!({ "company_daily_limit": 51 }), "First emails per company must be between 1 and 50 a day".into()),
            (json!({ "days_off": days }), "A project can have up to 100 days off".into()),
            (json!({ "sender_name": "ana@acme.example" }), "The sender name must be a person's or team's name of up to 100 characters, not an email address".into()),
            (json!({ "sender_name": "Ana\nLopez" }), "The sender name must be a person's or team's name of up to 100 characters, not an email address".into()),
            (json!({ "sender_name": "a".repeat(101) }), "The sender name must be a person's or team's name of up to 100 characters, not an email address".into()),
            (json!({ "mailbox_ids": [s.mailboxes[0], s.other_mailbox] }), "Pick mailboxes connected to this organization".into()),
            (json!({ "mailbox_ids": [Uuid::new_v4()] }), "Pick mailboxes connected to this organization".into()),
            (json!({ "assign_rule": "lottery" }), format!("assign_rule must be one of: {}", crate::ownership::RULES.join(", "))),
            (json!({ "bcc_email": "not an address" }), "The BCC address isn't a valid email address".into()),
            // Good values sent with a bad one aren't saved either.
            (json!({ "tone": "warm", "daily_email_limit": 80, "max_words": 10 }), "Max words must be between 20 and 500".into()),
        ];
        for (body, message) in cases {
            assert_eq!(refused(s.save(body.clone()).await), message, "{body}");
            assert_eq!(s.row().await, before, "{body} changed something");
        }

        let theirs = |body: Value| update_settings(State(s.state.clone()), s.other.clone(), Path(s.project), Json(serde_json::from_value(body).unwrap()));
        assert!(matches!(theirs(json!({ "tone": "rude" })).await, Err(AppError::NotFound)));
        assert!(matches!(get_settings(State(s.state.clone()), s.other.clone(), Path(s.project)).await, Err(AppError::NotFound)));
        assert_eq!(s.row().await, before);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_save_keeps_what_it_leaves_out_and_a_null_clears(db: PgPool) {
        let s = seed(db).await;
        let mut both = s.mailboxes.to_vec();
        both.sort();
        let [first, second] = [both[0], both[1]];
        let hundred: Vec<NaiveDate> = (0..100).map(|d| NaiveDate::from_ymd_opt(2027, 1, 1).unwrap() + Duration::days(d)).collect();
        let mut days: Vec<String> = hundred.iter().rev().map(|d| d.to_string()).collect();
        days.push("2027-01-01".into());
        let saved = s
            .save(json!({
                "daily_email_limit": 40,
                "timezone": "Europe/Berlin",
                "send_start_hour": 8,
                "send_end_hour": 18,
                "review_window_hours": 24,
                "monthly_cap_micros": 5_000_000,
                "company_daily_limit": 3,
                "mailbox_ids": [second, first, second],
                "days_off": days,
                "sender_name": "  Ana    Lopez  ",
                "booking_link": "  https://cal.example/ana  ",
            }))
            .await
            .unwrap();
        assert_eq!(saved.days_off, hundred, "sorted, and a repeated day doesn't count toward the 100");
        assert_eq!(
            (saved.daily_email_limit, saved.timezone.as_str(), saved.send_start_hour, saved.send_end_hour, saved.review_window_hours, saved.monthly_cap_micros),
            (40, "Europe/Berlin", 8, 18, Some(24), Some(5_000_000))
        );
        assert_eq!((saved.company_daily_limit, saved.mailbox_ids.clone()), (Some(3), Some(both.clone())));
        assert_eq!((saved.sender_name.as_str(), saved.booking_link.as_str()), ("Ana Lopez", "https://cal.example/ana"));

        // Moving one end of the window is checked against the saved other end, and everything not sent stays.
        let before = s.row().await;
        let saved = s.save(json!({ "send_start_hour": 6, "tone": "warm" })).await.unwrap();
        assert_eq!((saved.send_start_hour, saved.send_end_hour, saved.tone.as_str()), (6, 18, "warm"));
        let mut expected = before.clone();
        expected["send_start_hour"] = json!(6);
        expected["tone"] = json!("warm");
        assert_eq!(s.row().await, expected);

        let saved = s.save(json!({ "review_window_hours": null, "monthly_cap_micros": null, "company_daily_limit": null, "mailbox_ids": null })).await.unwrap();
        assert_eq!((saved.review_window_hours, saved.monthly_cap_micros, saved.company_daily_limit, saved.mailbox_ids), (None, None, None, None));
        assert_eq!((saved.daily_email_limit, saved.days_off.len(), saved.tone.as_str()), (40, 100, "warm"));
        let saved = s.save(json!({ "mailbox_ids": [], "days_off": [] })).await.unwrap();
        assert_eq!((saved.mailbox_ids, saved.days_off), (Some(vec![]), vec![]), "an empty pick is kept, unlike null");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn two_saves_moving_each_end_of_the_send_window_keep_both(db: PgPool) {
        let s = seed(db).await;
        load_or_create(&s.state.db, s.project).await.unwrap();
        // Racing them once may not overlap, so race them a few times.
        for _ in 0..10 {
            sqlx::query("update project_settings set send_start_hour = 9, send_end_hour = 17 where project_id = $1").bind(s.project).execute(&s.state.db).await.unwrap();
            let (start, end) = tokio::join!(s.save(json!({ "send_start_hour": 6 })), s.save(json!({ "send_end_hour": 12 })));
            assert!(start.is_ok() && end.is_ok());
            let window: (i32, i32) =
                sqlx::query_as("select send_start_hour, send_end_hour from project_settings where project_id = $1").bind(s.project).fetch_one(&s.state.db).await.unwrap();
            assert_eq!(window, (6, 12));
        }
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn follow_ups_are_checked_only_when_they_change(db: PgPool) {
        let s = seed(db).await;
        load_or_create(&s.state.db, s.project).await.unwrap();
        // Saved before the check got stricter: no `enabled`.
        let old = json!([{ "delay_days": 3, "instructions": "" }]);
        sqlx::query("update project_settings set follow_ups = $2 where project_id = $1").bind(s.project).bind(&old).execute(&s.state.db).await.unwrap();

        let saved = s.save(json!({ "follow_ups": old, "tone": "warm" })).await.unwrap();
        assert_eq!((saved.follow_ups, saved.tone.as_str()), (old.clone(), "warm"), "sending the saved steps back doesn't block the page");
        let changed = json!([{ "delay_days": 4, "instructions": "" }]);
        assert_eq!(refused(s.save(json!({ "follow_ups": changed })).await), "Each follow-up needs enabled, true or false");
        let fixed = json!([{ "delay_days": 4, "enabled": true }, { "kind": "call", "delay_days": 2, "enabled": false, "instructions": "Ask about pricing" }]);
        assert_eq!(s.save(json!({ "follow_ups": fixed })).await.unwrap().follow_ups, fixed);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_new_bcc_address_clears_its_last_error_and_new_targeting_rescores(db: PgPool) {
        let s = seed(db).await;
        load_or_create(&s.state.db, s.project).await.unwrap();
        sqlx::query(
            "update project_settings set bcc_email = 'crm@acme.example', bcc_error = 'Mailbox full', bcc_failed_at = now(),
               targeting = '{\"titles\": [\"Founder\"]}', score_inputs_at = '2026-01-01T00:00:00Z' where project_id = $1",
        )
        .bind(s.project)
        .execute(&s.state.db)
        .await
        .unwrap();
        let rescored = || async {
            sqlx::query_scalar::<_, Option<chrono::DateTime<Utc>>>("select score_inputs_at from project_settings where project_id = $1")
                .bind(s.project)
                .fetch_one(&s.state.db)
                .await
                .unwrap()
                .unwrap()
        };
        let january = "2026-01-01T00:00:00Z".parse::<chrono::DateTime<Utc>>().unwrap();

        let saved = s.save(json!({ "bcc_email": " CRM@Acme.example ", "targeting": { "titles": ["Founder"] } })).await.unwrap();
        assert_eq!((saved.bcc_email.as_str(), saved.bcc_error.as_deref(), saved.bcc_failed_at.is_some()), ("crm@acme.example", Some("Mailbox full"), true));
        assert_eq!(rescored().await, january, "the same targeting doesn't rescore");

        let saved = s.save(json!({ "bcc_email": "log@acme.example", "targeting": { "titles": ["CEO"] } })).await.unwrap();
        assert_eq!((saved.bcc_email.as_str(), saved.bcc_error, saved.bcc_failed_at), ("log@acme.example", None, None));
        assert_eq!(saved.targeting, json!({ "titles": ["CEO"] }));
        let changed = rescored().await;
        assert!(changed > january);

        let saved = s.save(json!({ "bcc_email": "  " })).await.unwrap();
        assert_eq!(saved.bcc_email, "", "blank turns it off");
        assert_eq!((saved.targeting, rescored().await), (json!({ "titles": ["CEO"] }), changed), "leaving targeting out keeps it and its time");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_webhook_secret_can_be_replaced_and_deliveries_stay_with_their_project(db: PgPool) {
        let s = seed(db).await;
        let first = load_or_create(&s.state.db, s.project).await.unwrap().webhook_secret;
        let Json(saved) = replace_webhook_secret(State(s.state.clone()), s.auth.clone(), Path(s.project)).await.unwrap();
        assert!(saved.webhook_secret.len() == 48 && saved.webhook_secret.chars().all(|c| c.is_ascii_hexdigit()), "{}", saved.webhook_secret);
        assert_ne!(saved.webhook_secret, first);
        assert!(matches!(replace_webhook_secret(State(s.state.clone()), s.other.clone(), Path(s.project)).await, Err(AppError::NotFound)));
        assert_eq!(load_or_create(&s.state.db, s.project).await.unwrap().webhook_secret, saved.webhook_secret);

        let mut ids = Vec::new();
        for (project, event, hours_ago) in [(s.project, "hot_lead", 2), (s.project, "lead_replied", 1), (s.other_project, "hot_lead", 0), (s.project, "deal.won", 3)] {
            let id: Uuid = sqlx::query_scalar(
                "insert into webhook_deliveries (project_id, event, payload, url, created_at)
                 values ($1, $2, '{}', 'https://hooks.example/a', now() - make_interval(hours => $3)) returning id",
            )
            .bind(project)
            .bind(event)
            .bind(hours_ago)
            .fetch_one(&s.state.db)
            .await
            .unwrap();
            ids.push(id);
        }
        let Json(listed) = list_deliveries(State(s.state.clone()), s.auth.clone(), Path(s.project)).await.unwrap();
        assert_eq!(listed.iter().map(|d| d.id).collect::<Vec<_>>(), [ids[1], ids[0], ids[3]], "newest first, this project's only");
        assert!(matches!(list_deliveries(State(s.state.clone()), s.other.clone(), Path(s.project)).await, Err(AppError::NotFound)));

        // The endpoint is only listed on the other project.
        sqlx::query("insert into webhook_endpoints (project_id, url) values ($1, 'https://hooks.example/a')").bind(s.other_project).execute(&s.state.db).await.unwrap();
        let resend = |delivery: Uuid| resend_delivery(State(s.state.clone()), s.auth.clone(), Path((s.project, delivery)));
        assert!(matches!(resend(ids[2]).await, Err(AppError::NotFound)), "another project's delivery");
        assert!(matches!(resend_delivery(State(s.state.clone()), s.other.clone(), Path((s.project, ids[0]))).await, Err(AppError::NotFound)));
        assert_eq!(refused(resend(ids[0]).await), "That endpoint isn't in the list any more");
        let count: i64 = sqlx::query_scalar("select count(*) from webhook_deliveries").fetch_one(&s.state.db).await.unwrap();
        assert_eq!(count, 4, "nothing was sent or logged");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn integration_forms_refuse_before_calling_out_and_disconnecting_clears_them(db: PgPool) {
        let s = seed(db).await;
        let (state, auth, id) = (s.state.clone(), s.auth.clone(), s.project);
        load_or_create(&s.state.db, id).await.unwrap();
        sqlx::query(
            "update project_settings set hubspot_token_enc = 'x', hubspot_error = 'Token expired', hubspot_failed_at = now(), pipedrive_token_enc = 'y',
               verifier = 'hunter', verifier_key_enc = 'z', verifier_error = 'Out of credits', verifier_failed_at = now(),
               unipile_dsn = 'https://api8.unipile.com:13851/api/v1', unipile_token_enc = 'w', unipile_account_id = 'acc', unipile_account_name = 'Ana Lopez',
               unipile_error = 'Session expired', unipile_failed_at = now()
             where project_id = $1",
        )
        .bind(id)
        .execute(&s.state.db)
        .await
        .unwrap();
        let before = s.row().await;

        let crm = |auth: &Auth, name: &str, token: &str, account: &str| {
            let body = CrmBody { token: token.into(), account: account.into() };
            connect_crm(State(state.clone()), auth.clone(), Path((id, name.to_string())), Json(body))
        };
        assert!(matches!(crm(&auth, "myspace", "t", "").await, Err(AppError::NotFound)));
        assert!(matches!(crm(&s.other, "hubspot", "t", "").await, Err(AppError::NotFound)));
        assert_eq!(refused(crm(&auth, "salesforce", "t", "").await), "Connect Salesforce by signing in");
        assert_eq!(refused(crm(&auth, "hubspot", "  ", "").await), "Paste your HubSpot token");
        assert_eq!(refused(crm(&auth, "copper", "t", " ").await), "Paste your Copper email");
        assert_eq!(refused(crm(&auth, "grist", "t", "").await), "Paste your Grist page link");

        let verifier = |auth: &Auth, provider: &str, key: &str| {
            let body = ServiceKey { provider: provider.into(), key: key.into() };
            connect_verifier(State(state.clone()), auth.clone(), Path(id), Json(body))
        };
        assert!(matches!(verifier(&s.other, "hunter", "k").await, Err(AppError::NotFound)));
        assert_eq!(refused(verifier(&auth, "nope", "k").await), "Pick a verification service");
        assert_eq!(refused(verifier(&auth, "hunter", " ").await), "Paste your Hunter API key");

        let unipile = |auth: &Auth, dsn: &str, token: &str| {
            let body = UnipileBody { dsn: dsn.into(), token: token.into() };
            connect_unipile(State(state.clone()), auth.clone(), Path(id), Json(body))
        };
        let dsn = "Paste the DSN from your Unipile dashboard, like api8.unipile.com:13851";
        assert!(matches!(unipile(&s.other, "api8.unipile.com:13851", "t").await, Err(AppError::NotFound)));
        assert_eq!(refused(unipile(&auth, "api8.evil.example:13851", "t").await), dsn);
        assert_eq!(refused(unipile(&auth, "http://api8.unipile.com:13851", "t").await), dsn);
        assert_eq!(refused(unipile(&auth, "api8.unipile.com:13851", " ").await), "Paste your Unipile access token");

        let test = |auth: &Auth, target: &str| {
            let body = TestBody { target: target.into(), url: "https://hooks.example/a".into() };
            test_integration(State(state.clone()), auth.clone(), Path(id), Json(body))
        };
        assert!(matches!(test(&s.other, "webhook").await, Err(AppError::NotFound)));
        assert_eq!(refused(test(&auth, "email").await), "target must be one of: webhook, slack");

        assert!(matches!(disconnect_crm(State(state.clone()), s.other.clone(), Path((id, "hubspot".into()))).await, Err(AppError::NotFound)));
        assert!(matches!(disconnect_verifier(State(state.clone()), s.other.clone(), Path(id)).await, Err(AppError::NotFound)));
        assert!(matches!(disconnect_unipile(State(state.clone()), s.other.clone(), Path(id)).await, Err(AppError::NotFound)));
        assert_eq!(s.row().await, before, "nothing changed");
        assert_eq!(s.audit().await, [], "nothing connected or disconnected");

        let Json(saved) = disconnect_crm(State(state.clone()), auth.clone(), Path((id, "hubspot".into()))).await.unwrap();
        assert_eq!((saved.hubspot_connected, saved.hubspot_error, saved.hubspot_failed_at, saved.pipedrive_connected), (false, None, None, true));
        let Json(saved) = disconnect_verifier(State(state.clone()), auth.clone(), Path(id)).await.unwrap();
        assert_eq!((saved.verifier.as_str(), saved.verifier_error, saved.verifier_failed_at), ("", None, None));
        assert_eq!(disconnect_verifier(State(state.clone()), auth.clone(), Path(id)).await.unwrap().0.verifier, "");
        // With no webhooks saved, disconnecting Unipile has nothing to remove there.
        let Json(saved) = disconnect_unipile(State(state.clone()), auth.clone(), Path(id)).await.unwrap();
        assert_eq!((saved.unipile_connected, saved.unipile_account_name.as_str(), saved.unipile_error, saved.unipile_failed_at), (false, "", None, None));
        let left: (String, String, String, String, String) = sqlx::query_as(
            "select hubspot_token_enc, verifier_key_enc, unipile_token_enc, unipile_dsn, unipile_account_id from project_settings where project_id = $1",
        )
        .bind(id)
        .fetch_one(&s.state.db)
        .await
        .unwrap();
        assert_eq!(left, Default::default());
        let disconnected = |label: &str| ("integration.disconnected".to_string(), label.to_string());
        assert_eq!(s.audit().await, [disconnected("HubSpot"), disconnected("Hunter"), disconnected("LinkedIn (Unipile)")], "a second disconnect of nothing isn't audited");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn assignment_keeps_its_order_and_only_takes_people_on_the_team(db: PgPool) {
        let s = seed(db).await;
        let mut team = vec![s.auth.user_id];
        for name in ["bea", "cal"] {
            let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1 || '@acme.example', $1, 'x') returning id")
                .bind(name)
                .fetch_one(&s.state.db)
                .await
                .unwrap();
            sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(s.auth.org_id).bind(user).execute(&s.state.db).await.unwrap();
            team.push(user);
        }
        team.sort();
        let saved = s
            .save(json!({
                "assign_rule": "round_robin",
                "assign_members": [team[2], team[0], team[2], team[1]],
                "assign_territories": [{ "owner": team[1], "countries": [" de ", "AT", "de"] }],
            }))
            .await
            .unwrap();
        let assignment: (String, Vec<Uuid>, Value) =
            sqlx::query_as("select assign_rule, assign_members, assign_territories from project_settings where project_id = $1").bind(s.project).fetch_one(&s.state.db).await.unwrap();
        assert_eq!(assignment, ("round_robin".to_string(), vec![team[2], team[0], team[1]], json!([{ "owner": team[1], "countries": ["DE", "AT"] }])));
        assert_eq!(saved.assign_rule, "round_robin");

        let before = s.row().await;
        let outsider = s.other.user_id;
        let not_on_team = "The owner has to be on your team";
        assert_eq!(refused(s.save(json!({ "assign_members": [team[0], outsider] })).await), not_on_team);
        assert_eq!(refused(s.save(json!({ "assign_territories": [{ "owner": outsider, "countries": ["FR"] }] })).await), not_on_team);
        assert_eq!(s.row().await, before);
    }
}
