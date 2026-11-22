//! The email to a lead who missed the meeting they booked (routes/leads.rs no_show): short, without blame, offering another
//! time with the booking link. It waits for approval like the follow-up after a meeting (agent/meetings.rs).

use std::future::Future;

use anyhow::Context;
use chrono_tz::Tz;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::explore::load_project;
use super::meetings::follow_up_blocker;
use super::sdk::Call;
use super::write::{WriterSettings, draft_in_thread};
use super::{Ctx, Outcome};
use crate::models::{Lead, Ts};

const WRITER: &str = "You write short follow-up emails for Splee, an outbound sales tool. Never invent commitments, prices or dates. \
Plain text only.";
const BLOCKED: &str = "No email: the lead has no address or asked not to be contacted";

#[derive(Deserialize)]
struct NoShowInput {
    lead_id: Uuid,
    missed_at: Ts,
}

#[derive(Deserialize)]
pub(super) struct Email {
    subject: String,
    body: String,
}

pub async fn follow_up_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    follow_up(ctx, |call| async move { ctx.ask::<Email>(&call).await }).await
}

/// `ask` is the model call, which tests replace. The real one adds its cost to the run, like every agent run.
pub(super) async fn follow_up<F, Fut>(ctx: &Ctx, ask: F) -> anyhow::Result<Outcome>
where
    F: FnOnce(Call) -> Fut,
    Fut: Future<Output = anyhow::Result<Email>>,
{
    let input: NoShowInput = ctx.input()?;
    let db = ctx.db();
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1 and project_id = $2")
        .bind(input.lead_id)
        .bind(ctx.project_id()?)
        .fetch_optional(db)
        .await?
        .context("lead not found")?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    if follow_up_blocker(&mut *db.acquire().await?, org_id, &lead).await?.is_some() {
        return Ok(Outcome::new(BLOCKED));
    }
    let settings = WriterSettings::load(ctx, lead.project_id).await?;
    let lang = crate::language::for_lead(db, lead.id).await?;
    let timezone: Option<String> =
        sqlx::query_scalar("select timezone from project_settings where project_id = $1").bind(lead.project_id).fetch_optional(db).await?;
    let tz: Tz = timezone.and_then(|tz| tz.parse().ok()).unwrap_or(chrono_tz::UTC);

    let who = [lead.full_name.trim(), lead.title.trim(), lead.company_name.trim()];
    let who = who.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(", ");
    let sender = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) };
    let next_step = if settings.booking_link.trim().is_empty() {
        "Ask which day and time would suit them instead.".to_string()
    } else {
        format!("Offer another time with the booking link {} on its own line.", settings.booking_link.trim())
    };
    let prompt = format!(
        "A lead ({who}) booked a call with {us} for {day} and didn't join it.\n\n\
         Write the email to {first} from {sender}: say we missed them, without blame or guilt, as plans change, and offer \
         another time. {next_step} At most 70 words. Tone: {tone}. Start with a greeting and their first name. No sign-off. Give \
         it a short subject too. {language}",
        us = profile.name,
        day = input.missed_at.with_timezone(&tz).format("%A %-d %B"),
        first = if lead.first_name.is_empty() { "the lead" } else { &lead.first_name },
        tone = settings.tone,
        language = crate::language::instruction(lang),
    );
    let schema = json!({
        "type": "object",
        "properties": { "subject": { "type": "string" }, "body": { "type": "string" } },
        "required": ["subject", "body"]
    });
    ctx.log(format!("writing to {} about the missed call…", if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name })).await?;
    let email = ask(Call::writing(WRITER, prompt, schema)).await?;

    let mut tx = db.begin().await?;
    // They may have opted out while the model was writing.
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(lead.id).fetch_one(&mut *tx).await?;
    if follow_up_blocker(&mut tx, org_id, &lead).await?.is_some() {
        return Ok(Outcome::new(BLOCKED));
    }
    let body = settings.sign(email.body.trim());
    let draft = draft_in_thread(&mut tx, &lead, "no_show_follow_up", email.subject.trim(), &body, lang.code).await?;
    tx.commit().await?;
    Ok(Outcome { message: "Drafted an email offering another time".into(), output: json!({ "lead_id": lead.id, "email_id": draft }) })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;
    use crate::state::AppState;

    /// Runs the lead's queued no-show run with a fake model that answers `email`, and returns the prompt it got.
    pub async fn run(state: &AppState, lead: Uuid, email: (&str, &str)) -> (Outcome, String) {
        let run = sqlx::query_as::<_, AgentRun>("select * from agent_runs where kind = 'no_show_follow_up' and input ->> 'lead_id' = $1::text")
            .bind(lead)
            .fetch_one(&state.db)
            .await
            .unwrap();
        let ctx = Ctx { state: state.clone(), run, step: AtomicI32::new(0) };
        let asked = Mutex::new(String::new());
        let outcome = follow_up(&ctx, |c| async {
            *asked.lock().unwrap() = c.prompt;
            ctx.add_cost(0.003).await;
            Ok(Email { subject: email.0.into(), body: email.1.into() })
        })
        .await
        .unwrap();
        (outcome, asked.into_inner().unwrap())
    }
}
