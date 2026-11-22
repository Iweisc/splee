//! A short summary of a transcribed call and a suggested next step, for the lead's timeline and the inbox, and when someone
//! talked with the lead, the follow-up email, which waits for approval like the one after a meeting (agent/meetings.rs), and
//! a note in the project's CRMs.

use std::future::Future;

use anyhow::Context;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::explore::load_project;
use super::meetings::follow_up_blocker;
use super::sdk::Call;
use super::write::{WriterSettings, draft_in_thread};
use super::{Ctx, Outcome};
use crate::models::{self, Lead};

const NOTE_TAKER: &str = "You take notes on sales calls for Splee, an outbound sales tool, and write the follow-up email. Use only \
what the transcript says. Never invent commitments, prices or dates. Plain text only.";

#[derive(Deserialize)]
struct CallInput {
    call_id: Uuid,
}

#[derive(Deserialize)]
pub(super) struct Summary {
    summary: String,
    next_step: String,
    /// Both empty when nobody talked with the lead, like on a voicemail.
    follow_up_subject: String,
    follow_up_body: String,
}

pub async fn summarize_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    summarize(ctx, |call| async move { ctx.ask::<Summary>(&call).await }).await
}

/// `ask` is the model call, which tests replace. The real one adds its cost to the run, like every agent run.
pub(super) async fn summarize<F, Fut>(ctx: &Ctx, ask: F) -> anyhow::Result<Outcome>
where
    F: FnOnce(Call) -> Fut,
    Fut: Future<Output = anyhow::Result<Summary>>,
{
    let input: CallInput = ctx.input()?;
    let call = sqlx::query_as::<_, models::Call>("select * from calls where id = $1 and project_id = $2")
        .bind(input.call_id)
        .bind(ctx.project_id()?)
        .fetch_optional(ctx.db())
        .await?
        .context("call not found")?;
    if call.transcript.trim().is_empty() {
        return Ok(Outcome::new("Nothing was said on the call"));
    }
    let db = ctx.db();
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(call.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, call.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    let settings = WriterSettings::load(ctx, call.project_id).await?;
    let lang = crate::language::for_lead(db, lead.id).await?;
    let transcript: String = call.transcript.chars().take(30_000).collect();
    let who = [lead.full_name.trim(), lead.title.trim(), lead.company_name.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(", ");
    let us = &profile.name;
    let call_was = if call.inbound {
        format!("a sales call a lead ({who}) made to {us}. \"Lead\" is the caller, \"Us\" the person who answered")
    } else {
        format!("a sales call from {us} to a lead ({who}). \"Us\" is the caller, \"Lead\" the person called")
    };
    let sender = if settings.sender_name.is_empty() { us.clone() } else { format!("{} at {us}", settings.sender_name) };
    let prompt = format!(
        "Here is the transcript of {call_was}. The transcript comes from speech recognition, so expect small errors.\n\n{transcript}\n\n\
         1. Write a summary of 2 or 3 sentences: what they said about their situation, interest and any objection.\n\
         2. Suggest the one next step that fits what was agreed or said, in one sentence starting with a verb, like \"Send the case \
         study they asked for.\"\n\
         3. Write the follow-up email to {first} from {sender}: thank them for the call, recap what they said and what was agreed, \
         and the next step. At most 120 words. Tone: {tone}. Start with a greeting and their first name. No sign-off. Give it a \
         short subject too. {language} Leave the subject and body empty when nobody from their side really talked with us, like a \
         voicemail, a gatekeeper or a wrong number, or when they asked not to be contacted.\n\
         Write the summary and next step in English.",
        first = if lead.first_name.is_empty() { "the lead" } else { &lead.first_name },
        tone = settings.tone,
        language = crate::language::instruction(lang),
    );
    let schema = json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "next_step": { "type": "string" },
            "follow_up_subject": { "type": "string" },
            "follow_up_body": { "type": "string" },
        },
        "required": ["summary", "next_step", "follow_up_subject", "follow_up_body"]
    });
    ctx.log(format!("summarizing the call with {}…", lead.first_name)).await?;
    let answer = ask(Call::writing(NOTE_TAKER, prompt, schema)).await?;

    let (subject, body) = (answer.follow_up_subject.trim(), answer.follow_up_body.trim());
    let talked = !subject.is_empty() && !body.is_empty();
    let mut tx = db.begin().await?;
    // A run started twice for the call drafts once and tells the CRMs once.
    let (drafted, summarized): (Option<Uuid>, String) =
        sqlx::query_as("select follow_up_email_id, summary from calls where id = $1 for update").bind(call.id).fetch_one(&mut *tx).await?;
    let draft = if drafted.is_some() || !talked || follow_up_blocker(&mut tx, org_id, &lead).await?.is_some() {
        None
    } else {
        Some(draft_in_thread(&mut tx, &lead, "call_follow_up", subject, &settings.sign(body), lang.code).await?)
    };
    sqlx::query("update calls set summary = $2, next_step = $3, follow_up_email_id = coalesce(follow_up_email_id, $4) where id = $1")
        .bind(call.id)
        .bind(answer.summary.trim())
        .bind(answer.next_step.trim())
        .bind(draft)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    // Like a hot reply, only a conversation goes to the CRMs, not a voicemail or a gatekeeper.
    if talked && summarized.is_empty() {
        let link = crate::hooks::lead_url(&ctx.state.cfg.public_url, call.project_id, lead.id);
        crate::crm::sync(&ctx.state, call.project_id, &lead, &crm_note(&call, answer.summary.trim(), answer.next_step.trim(), &link)).await;
    }
    let message = if draft.is_some() { "Summarized the call and drafted the follow-up" } else { "Summarized the call" };
    Ok(Outcome { message: message.into(), output: json!({ "call_id": call.id, "email_id": draft }) })
}

/// The first line is what Salesforce shows as the Task's subject.
fn crm_note(call: &models::Call, summary: &str, next_step: &str, link: &str) -> String {
    let who = if call.inbound { "They called us" } else { "We called them" };
    let length = call.duration_secs.map(|secs| format!(", {} min", ((secs + 30) / 60).max(1))).unwrap_or_default();
    let next = if next_step.is_empty() { String::new() } else { format!("\n\nNext step: {next_step}") };
    format!("Call notes from Splee. {who}{length}.\n\n{summary}{next}\n{link}")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicI32;

    use sqlx::PgPool;

    use super::*;
    use crate::crm::Crm;
    use crate::crm::signin::tests::{Fake, LIVE, connect_fake};
    use crate::models::AgentRun;

    async fn summarize_with(db: &PgPool, call: Uuid, answer: (&str, &str)) -> (Outcome, String) {
        let run = sqlx::query_as::<_, AgentRun>(
            "insert into agent_runs (project_id, org_id, kind, input) select c.project_id, p.org_id, 'summarize_call', $2 from calls c
             join projects p on p.id = c.project_id where c.id = $1 returning *",
        )
        .bind(call)
        .bind(json!({ "call_id": call }))
        .fetch_one(db)
        .await
        .unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run, step: AtomicI32::new(0) };
        let asked = Mutex::new(String::new());
        let outcome = summarize(&ctx, |c| async {
            *asked.lock().unwrap() = c.prompt;
            ctx.add_cost(0.004).await;
            Ok(Summary {
                summary: " Sam wants to see results first. ".into(),
                next_step: "Send the case study.".into(),
                follow_up_subject: answer.0.into(),
                follow_up_body: answer.1.into(),
            })
        })
        .await
        .unwrap();
        let cost: f64 = sqlx::query_scalar("select cost_usd from agent_runs where id = $1").bind(ctx.run.id).fetch_one(db).await.unwrap();
        assert!((cost - 0.004).abs() < 1e-9, "{cost}");
        (outcome, asked.into_inner().unwrap())
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_transcribed_call_gets_a_summary_and_a_follow_up_to_approve(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into project_settings (project_id, sender_name, tone) values ($1, 'Jo', 'warm')").bind(project).execute(&db).await.unwrap();
        let (crm, state) = (Fake::start().await, crate::autopilot::tests::state(db.clone()));
        connect_fake(&state, &crm, project, Crm::Zoho, LIVE, "good", 3600).await;
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, full_name, first_name, email, company_name, phone)
             values ($1, 'Sam Lee', 'Sam', 'sam@smile.test', 'Smile Dental', '+447700900123') returning id",
        )
        .bind(project)
        .fetch_one(&db)
        .await
        .unwrap();
        let call = |transcript: &'static str, inbound: bool| {
            sqlx::query_scalar::<_, Uuid>("insert into calls (project_id, lead_id, kind, status, transcript, inbound) values ($1, $2, 'manual', 'done', $3, $4) returning id")
                .bind(project)
                .bind(lead)
                .bind(transcript)
                .bind(inbound)
                .fetch_one(&db)
        };

        let talked = call("Us: Is now a good time?\nLead: Send me the case study.", false).await.unwrap();
        sqlx::query("update calls set duration_secs = 254 where id = $1").bind(talked).execute(&db).await.unwrap();
        let (outcome, prompt) = summarize_with(&db, talked, ("Our call today", "Hi Sam, thanks for the call. The case study is on its way.")).await;
        assert_eq!(outcome.message, "Summarized the call and drafted the follow-up");
        assert!(prompt.contains("Lead: Send me the case study.") && prompt.contains("from Fieldnote to a lead (Sam Lee, Smile Dental)"), "{prompt}");
        assert!(prompt.contains("\"Us\" is the caller") && prompt.contains("to Sam from Jo at Fieldnote") && prompt.contains("Tone: warm"), "{prompt}");
        let (summary, next, email): (String, String, Option<Uuid>) =
            sqlx::query_as("select summary, next_step, follow_up_email_id from calls where id = $1").bind(talked).fetch_one(&db).await.unwrap();
        assert_eq!((summary.as_str(), next.as_str()), ("Sam wants to see results first.", "Send the case study."));
        assert_eq!(outcome.output["email_id"], json!(email));
        let draft: (String, String, String, String, String) =
            sqlx::query_as("select kind, status, to_email, subject, body from email_messages where id = $1").bind(email).fetch_one(&db).await.unwrap();
        let body = "Hi Sam, thanks for the call. The case study is on its way.\n\nJo";
        assert_eq!(draft, ("call_follow_up".into(), "draft".into(), "sam@smile.test".into(), "Our call today".into(), body.into()));
        let link = format!("{}/app/p/{project}/inbox?lead={lead}", state.cfg.public_url);
        let notes = || crm.records("zoho:Note").iter().map(|n| n["Note_Content"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let note = |who: &str| format!("Call notes from Splee. {who}.\n\nSam wants to see results first.\n\nNext step: Send the case study.\n{link}");
        assert_eq!(notes(), [note("We called them, 4 min")], "the CRM gets the call");

        // A second run for the call doesn't draft again or tell the CRM twice.
        let (outcome, _) = summarize_with(&db, talked, ("Again", "Hi Sam, again.")).await;
        assert_eq!(outcome.message, "Summarized the call");
        let drafts: i64 = sqlx::query_scalar("select count(*) from email_messages where lead_id = $1").bind(lead).fetch_one(&db).await.unwrap();
        assert_eq!(drafts, 1);
        assert_eq!(notes().len(), 1);

        // A voicemail gets no follow-up or note, and a call the lead made is told the right way round.
        let voicemail = call("Lead: You've reached Sam, leave a message.", true).await.unwrap();
        let (outcome, prompt) = summarize_with(&db, voicemail, ("", "")).await;
        assert_eq!(outcome.message, "Summarized the call");
        assert!(prompt.contains("a sales call a lead (Sam Lee, Smile Dental) made to Fieldnote. \"Lead\" is the caller"), "{prompt}");
        let email: Option<Uuid> = sqlx::query_scalar("select follow_up_email_id from calls where id = $1").bind(voicemail).fetch_one(&db).await.unwrap();
        assert_eq!(email, None);
        assert_eq!(notes().len(), 1);

        // Nor does a lead who opted out, though the CRM still hears about the conversation.
        sqlx::query("update leads set status = 'unsubscribed' where id = $1").bind(lead).execute(&db).await.unwrap();
        let after = call("Lead: Hi, it's Sam.\nUs: Hi Sam!", true).await.unwrap();
        assert_eq!(summarize_with(&db, after, ("Thanks", "Hi Sam, thanks.")).await.0.message, "Summarized the call");
        assert_eq!(notes()[1], note("They called us"));
    }
}
