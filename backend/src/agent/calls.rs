//! A short summary of a transcribed call and a suggested next step, for the lead's timeline and the inbox.

use std::future::Future;

use anyhow::Context;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::{Ctx, Outcome};
use crate::models::{self, Lead};

const NOTE_TAKER: &str = "You take notes on sales calls for Splee, an outbound sales tool. Use only what the transcript says. \
Never invent commitments, prices or dates. Plain text only.";

#[derive(Deserialize)]
struct CallInput {
    call_id: Uuid,
}

#[derive(Deserialize)]
pub(super) struct Summary {
    summary: String,
    next_step: String,
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
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(call.lead_id).fetch_one(ctx.db()).await?;
    let (_, profile) = load_project(ctx, call.project_id).await?;
    let transcript: String = call.transcript.chars().take(30_000).collect();
    let who = [lead.full_name.trim(), lead.title.trim(), lead.company_name.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(", ");
    let prompt = format!(
        "Here is the transcript of a sales call from {us} to a lead ({who}). \"Us\" is the caller, \"Lead\" the person called. \
         The transcript comes from speech recognition, so expect small errors.\n\n{transcript}\n\n\
         Write a summary of 2 or 3 sentences: what they said about their situation, interest and any objection. Then suggest the \
         one next step that fits what was agreed or said, in one sentence starting with a verb, like \"Send the case study they asked for.\"",
        us = profile.name,
    );
    let schema = json!({
        "type": "object",
        "properties": { "summary": { "type": "string" }, "next_step": { "type": "string" } },
        "required": ["summary", "next_step"]
    });
    ctx.log(format!("summarizing the call with {}…", lead.first_name)).await?;
    let answer = ask(Call::writing(NOTE_TAKER, prompt, schema)).await?;
    sqlx::query("update calls set summary = $2, next_step = $3 where id = $1")
        .bind(call.id)
        .bind(answer.summary.trim())
        .bind(answer.next_step.trim())
        .execute(ctx.db())
        .await?;
    Ok(Outcome { message: "Summarized the call".into(), output: json!({ "call_id": call.id }) })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicI32;

    use sqlx::PgPool;

    use super::*;
    use crate::models::AgentRun;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_transcribed_call_gets_a_summary_and_its_cost_goes_on_the_run(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, full_name, first_name, company_name, phone) values ($1, 'Sam Lee', 'Sam', 'Smile Dental', '+447700900123') returning id",
        )
        .bind(project)
        .fetch_one(&db)
        .await
        .unwrap();
        let call: Uuid = sqlx::query_scalar(
            "insert into calls (project_id, lead_id, kind, status, transcript) values ($1, $2, 'manual', 'done', 'Us: Is now a good time?\nLead: Send me the case study.')
             returning id",
        )
        .bind(project)
        .bind(lead)
        .fetch_one(&db)
        .await
        .unwrap();
        let run = sqlx::query_as::<_, AgentRun>("insert into agent_runs (project_id, org_id, kind, input) values ($1, $2, 'summarize_call', $3) returning *")
            .bind(project)
            .bind(org)
            .bind(json!({ "call_id": call }))
            .fetch_one(&db)
            .await
            .unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run, step: AtomicI32::new(0) };

        let asked = Mutex::new(String::new());
        let outcome = summarize(&ctx, |c| async {
            *asked.lock().unwrap() = c.prompt;
            ctx.add_cost(0.004).await;
            Ok(Summary { summary: " Sam wants to see results first. ".into(), next_step: "Send the case study.".into() })
        })
        .await
        .unwrap();
        assert_eq!(outcome.message, "Summarized the call");
        let prompt = asked.into_inner().unwrap();
        assert!(prompt.contains("Lead: Send me the case study.") && prompt.contains("from Fieldnote") && prompt.contains("Sam Lee, Smile Dental"), "{prompt}");
        let (summary, next): (String, String) = sqlx::query_as("select summary, next_step from calls where id = $1").bind(call).fetch_one(&db).await.unwrap();
        assert_eq!((summary.as_str(), next.as_str()), ("Sam wants to see results first.", "Send the case study."));
        let cost: f64 = sqlx::query_scalar("select cost_usd from agent_runs where id = $1").bind(ctx.run.id).fetch_one(&db).await.unwrap();
        assert!((cost - 0.004).abs() < 1e-9, "{cost}");
    }
}
