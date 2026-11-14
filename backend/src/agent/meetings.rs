//! Notes on a sales meeting from Fireflies, Fathom, Gong or Zoom (meetings/): a short summary, the next steps, a follow-up
//! email to the lead that waits for approval like any draft, and the deal stage the meeting suggests. The stage is only a
//! suggestion. Someone accepts it with a click (routes/meetings.rs).

use std::future::Future;

use anyhow::Context;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::write::WriterSettings;
use super::{Ctx, Outcome};
use crate::models::{Lead, MEETING_COLUMNS, Meeting};

const NOTE_TAKER: &str = "You take notes on sales meetings for Splee, an outbound sales tool, and write the follow-up email. Use only \
what the transcript and notes say. Never invent commitments, prices or dates. Plain text only.";
/// What of the transcript the agent reads. The rest is kept, but an hour's meeting fits well inside this.
const PROMPT_CHARS: usize = 100_000;

#[derive(Deserialize)]
struct NotesInput {
    meeting_id: Uuid,
}

#[derive(Deserialize)]
pub(crate) struct Notes {
    pub summary: String,
    pub next_steps: Vec<String>,
    pub follow_up_subject: String,
    pub follow_up_body: String,
    /// A stage's name, or empty to leave the deal where it is.
    pub stage: String,
    pub stage_reason: String,
}

pub async fn notes_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    notes(ctx, |call| async move { ctx.ask::<Notes>(&call).await }).await
}

/// `ask` is the model call, which tests replace. The real one adds its cost to the run, like every agent run.
pub(super) async fn notes<F, Fut>(ctx: &Ctx, ask: F) -> anyhow::Result<Outcome>
where
    F: FnOnce(Call) -> Fut,
    Fut: Future<Output = anyhow::Result<Notes>>,
{
    let input: NotesInput = ctx.input()?;
    let db = ctx.db();
    let meeting = sqlx::query_as::<_, Meeting>(&format!("select {MEETING_COLUMNS} from meetings m where m.id = $1 and m.project_id = $2"))
        .bind(input.meeting_id)
        .bind(ctx.project_id()?)
        .fetch_optional(db)
        .await?
        .context("meeting not found")?;
    if meeting.summarized_at.is_some() {
        return Ok(Outcome::new("The meeting already has notes"));
    }
    let transcript: String = sqlx::query_scalar("select left(transcript, $2) from meetings where id = $1")
        .bind(meeting.id)
        .bind(PROMPT_CHARS as i32)
        .fetch_one(db)
        .await?;
    if transcript.trim().is_empty() && meeting.provider_summary.trim().is_empty() {
        return Ok(Outcome::new("The meeting has no transcript yet"));
    }
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(meeting.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, meeting.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    let settings = WriterSettings::load(ctx, meeting.project_id).await?;
    let lang = crate::language::for_lead(db, lead.id).await?;
    let stages = crate::deals::stages(db, org_id).await?;
    let latest_deal = crate::deals::for_lead(db, lead.id).await?;
    let deal = latest_deal.clone().filter(|d| d.status == "open");

    let who = [lead.full_name.trim(), lead.title.trim(), lead.company_name.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(", ");
    let attendees = meeting.attendees.as_array().into_iter().flatten().filter_map(|a| a["name"].as_str().or(a["email"].as_str())).collect::<Vec<_>>().join(", ");
    let stage_names: Vec<&str> = stages.iter().map(|s| s.name.as_str()).collect();
    let sender = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) };
    let prompt = format!(
        "Here is a sales meeting between {us} and a lead ({who}), \"{title}\" on {day}. Attendees from their side: {attendees}.\n\n\
         {provider_notes}Transcript (from speech recognition, so expect small errors):\n{transcript}\n\n\
         1. Summarize the meeting in 3 to 5 sentences: their situation, what they want, objections and what was agreed.\n\
         2. List the next steps that were agreed or are clearly needed, 1 to 5 short items, each starting with a verb.\n\
         3. Write the follow-up email to {first} from {sender}: thank them, recap what was agreed and the next steps, and \
         confirm the next meeting if one was set. At most 150 words. Tone: {tone}. Start with a greeting and their first name. \
         No sign-off. Give it a short subject too. {language} Write the summary, next steps and stage reason in English.\n\
         4. The deal is {deal_stage}. The stages are, in order: {stages}. Pick the stage the meeting moves it to, using a name \
         exactly as written, or an empty string when it should stay where it is. Say why in one sentence.",
        us = profile.name,
        title = meeting.title,
        day = meeting.started_at.format("%A %e %B %Y"),
        attendees = if attendees.is_empty() { "unknown" } else { &attendees },
        provider_notes = if meeting.provider_summary.trim().is_empty() {
            String::new()
        } else {
            format!("The recorder's own notes:\n{}\n\n", meeting.provider_summary.trim())
        },
        transcript = if transcript.trim().is_empty() { "(none)" } else { transcript.trim() },
        first = if lead.first_name.is_empty() { "the lead" } else { &lead.first_name },
        tone = settings.tone,
        language = crate::language::instruction(lang),
        deal_stage = deal.as_ref().map_or("not opened yet".to_string(), |d| format!("in {}", d.stage)),
        stages = stage_names.join(", "),
    );
    let mut stage_enum: Vec<&str> = stage_names.clone();
    stage_enum.push("");
    let schema = json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "next_steps": { "type": "array", "items": { "type": "string" } },
            "follow_up_subject": { "type": "string" },
            "follow_up_body": { "type": "string" },
            "stage": { "type": "string", "enum": stage_enum },
            "stage_reason": { "type": "string" },
        },
        "required": ["summary", "next_steps", "follow_up_subject", "follow_up_body", "stage", "stage_reason"]
    });
    ctx.log(format!("writing notes on the meeting with {}…", if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name })).await?;
    let call = Call { timeout: std::time::Duration::from_secs(4 * 60), budget_usd: 0.5, ..Call::writing(NOTE_TAKER, prompt, schema) };
    let notes = ask(call).await?;

    let next_steps: Vec<String> = notes.next_steps.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).take(5).collect();
    // Only a move is a suggestion: not the stage the deal is in, and nothing for a lead whose deal already closed.
    let stage = stages.iter().find(|s| !notes.stage.trim().is_empty() && s.name.eq_ignore_ascii_case(notes.stage.trim())).filter(|s| match &deal {
        Some(d) => d.stage_id != s.id,
        None => latest_deal.is_none(),
    });

    let mut tx = db.begin().await?;
    let draft = match follow_up_blocker(&mut tx, org_id, &lead).await? {
        Some(_) => None,
        None => {
            let last: Option<(Option<Uuid>, String)> = sqlx::query_as(
                "select mailbox_id, subject from email_messages where lead_id = $1 and direction = 'outbound' and status = 'sent'
                 order by sent_at desc limit 1",
            )
            .bind(lead.id)
            .fetch_optional(&mut *tx)
            .await?;
            let (mailbox, subject) = match last {
                Some((mailbox, s)) if !s.trim().is_empty() => (mailbox, if s.to_lowercase().starts_with("re:") { s } else { format!("Re: {s}") }),
                Some((mailbox, _)) => (mailbox, notes.follow_up_subject.trim().to_string()),
                None => (None, notes.follow_up_subject.trim().to_string()),
            };
            let id: Uuid = sqlx::query_scalar(
                "insert into email_messages (project_id, lead_id, segment_id, mailbox_id, direction, kind, subject, body, status, to_email, language)
                 values ($1, $2, $3, $4, 'outbound', 'meeting_follow_up', $5, $6, 'draft', $7, $8) returning id",
            )
            .bind(lead.project_id)
            .bind(lead.id)
            .bind(lead.segment_id)
            .bind(mailbox)
            .bind(subject)
            .bind(settings.sign(notes.follow_up_body.trim()))
            .bind(&lead.email)
            .bind(lang.code)
            .fetch_one(&mut *tx)
            .await?;
            Some(id)
        }
    };
    // A run started twice for the meeting writes its notes once.
    let saved = sqlx::query(
        "update meetings set summary = $2, next_steps = $3, follow_up_email_id = $4, suggested_stage_id = $5, stage_reason = $6,
                             stage_status = case when $5::uuid is null then 'none' else 'pending' end, summarized_at = now()
         where id = $1 and summarized_at is null",
    )
    .bind(meeting.id)
    .bind(notes.summary.trim())
    .bind(json!(next_steps))
    .bind(draft)
    .bind(stage.map(|s| s.id))
    .bind(if stage.is_some() { notes.stage_reason.trim() } else { "" })
    .execute(&mut *tx)
    .await?;
    if saved.rows_affected() == 0 {
        return Ok(Outcome::new("The meeting already has notes"));
    }
    tx.commit().await?;
    let message = match (draft, stage) {
        (Some(_), Some(s)) => format!("Wrote the meeting notes, drafted the follow-up and suggested {}", s.name),
        (Some(_), None) => "Wrote the meeting notes and drafted the follow-up".to_string(),
        (None, _) => "Wrote the meeting notes. No follow-up: the lead has no email or asked not to be contacted".to_string(),
    };
    Ok(Outcome { message, output: json!({ "meeting_id": meeting.id, "email_id": draft }) })
}

/// Why no follow-up goes to the lead: no address, or they're off limits.
pub(super) async fn follow_up_blocker(conn: &mut sqlx::PgConnection, org_id: Uuid, lead: &Lead) -> sqlx::Result<Option<&'static str>> {
    let Some(email) = lead.email.as_deref().filter(|e| e.contains('@')) else { return Ok(Some("no email")) };
    if matches!(lead.status.as_str(), "unsubscribed" | "bounced" | "do_not_contact") {
        return Ok(Some("opted out"));
    }
    let suppressed: bool = sqlx::query_scalar(
        "select exists (select 1 from suppressions where org_id = $1 and (value = lower($2) or value = split_part(lower($2), '@', 2)))",
    )
    .bind(org_id)
    .bind(email)
    .fetch_one(conn)
    .await?;
    Ok(suppressed.then_some("on the do-not-contact list"))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;
    use crate::state::AppState;

    /// Runs the queued notes run for `meeting` with a fake model that answers `notes`, and returns the prompt it got.
    pub async fn run_notes(state: &AppState, meeting: Uuid, notes: Notes) -> (Outcome, String) {
        let run = sqlx::query_as::<_, AgentRun>("select * from agent_runs where kind = 'meeting_notes' and input ->> 'meeting_id' = $1::text")
            .bind(meeting)
            .fetch_one(&state.db)
            .await
            .unwrap();
        let ctx = Ctx { state: state.clone(), run, step: AtomicI32::new(0) };
        let asked = Mutex::new(String::new());
        let outcome = super::notes(&ctx, |c| async {
            *asked.lock().unwrap() = c.prompt;
            ctx.add_cost(0.02).await;
            Ok(notes)
        })
        .await
        .unwrap();
        (outcome, asked.into_inner().unwrap())
    }

    pub fn answer(stage: &str) -> Notes {
        Notes {
            summary: " Sarah's team loses a day a week to manual reports. They liked the demo and want pricing for 40 seats. ".into(),
            next_steps: vec!["Send pricing for 40 seats".into(), " ".into(), "Book a call with their CFO".into()],
            follow_up_subject: "Pricing for Northwind".into(),
            follow_up_body: "Hi Sarah, thanks for your time today. Pricing for 40 seats is on its way.".into(),
            stage: stage.into(),
            stage_reason: "They asked for a proposal.".into(),
        }
    }
}
