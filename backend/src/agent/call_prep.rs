//! Call prep: before an open call comes due, the agent writes the caller a talk track from what Splee knows about the
//! lead (briefs::gather): an opener tied to something they did, questions to ask, the objections they're likely to raise
//! with answers, a voicemail to leave and one reminder from the coaching the caller got on recent calls (coaching.rs).
//! One Claude call per call, without searching the web. It shows on the Calls page and beside the dialer.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::briefs::gather;
use super::explore::load_project;
use super::sdk::Call;
use super::write::{WriterSettings, company_signals, lead_brief, materials};
use super::{Ctx, Outcome, kind};
use crate::models::{Lead, Ts};

/// Open calls due within this many hours, or overdue, get a talk track.
const AHEAD_HOURS: i32 = 12;
const PER_PASS: i64 = 30;
const PREPPER: &str = "You prepare salespeople for phone calls to leads for Splee, an outbound sales tool. Use only what you're \
given. Never invent facts, numbers, names or quotes. Write lines the caller says out loud: short, plain and spoken, never \
salesy. Address the caller as \"you\" outside the lines they say. Plain text only, no markdown.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Handle {
    pub objection: String,
    pub answer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Prep {
    /// The first thing to say once they pick up, tied to something they did or their situation.
    pub opener: String,
    pub questions: Vec<String>,
    pub objections: Vec<Handle>,
    pub voicemail: String,
    /// One thing to do differently from the caller's recent coaching, or empty when they have none.
    pub reminder: String,
}

/// A call's talk track: `status` is ready, writing or failed.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Saved {
    pub prep: Option<Value>,
    pub created_at: Ts,
    pub status: String,
}

/// The SQL for a call's talk track as json, for the call lists. `c` is the call.
pub const PREP_JSON: &str = "(select json_build_object('prep', p.prep, 'created_at', p.created_at, 'status',
        case when p.prep is not null then 'ready' when r.status in ('queued', 'running') then 'writing' else 'failed' end)
      from call_prep p left join agent_runs r on r.id = p.run_id where p.call_id = c.id)";

/// Queues a talk track for every open call due within AHEAD_HOURS, or overdue, that has none, in projects that keep it on.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "with due as (
           insert into call_prep (project_id, call_id)
           select c.project_id, c.id from calls c join project_settings s on s.project_id = c.project_id and s.prep_calls
           where c.status = 'open' and c.due_at < now() + make_interval(hours => $1)
             and not exists (select 1 from call_prep p where p.call_id = c.id)
           order by c.due_at limit $2
           on conflict do nothing
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id where p.org_id is not null",
    )
    .bind(AHEAD_HOURS)
    .bind(PER_PASS)
    .fetch_all(&mut *tx)
    .await?;
    for (id, project_id, org_id) in &due {
        start(&mut tx, *id, *project_id, *org_id).await?;
    }
    tx.commit().await?;
    Ok(due.len())
}

async fn start(tx: &mut sqlx::PgConnection, id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let run = super::enqueue(&mut *tx, kind::CALL_PREP, Some(project_id), Some(org_id), json!({ "prep_id": id })).await?;
    sqlx::query("update call_prep set run_id = $2, prep = null, created_at = now() where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

/// Writes the call's talk track again, or for the first time, now. One already being written is reused.
pub async fn request(db: &PgPool, call_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "insert into call_prep (project_id, call_id) values ($1, $2) on conflict (call_id) do update set call_id = excluded.call_id returning id",
    )
    .bind(project_id)
    .bind(call_id)
    .fetch_one(&mut *tx)
    .await?;
    let writing: Option<Uuid> =
        sqlx::query_scalar("select r.id from call_prep p join agent_runs r on r.id = p.run_id where p.id = $1 and r.status in ('queued', 'running')")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let run = match writing {
        Some(run) => run,
        None => start(&mut tx, id, project_id, org_id).await?,
    };
    tx.commit().await?;
    Ok(run)
}

pub async fn latest(db: &PgPool, call_id: Uuid) -> sqlx::Result<Option<Saved>> {
    sqlx::query_as(
        "select p.prep, p.created_at,
                case when p.prep is not null then 'ready' when r.status in ('queued', 'running') then 'writing' else 'failed' end as status
         from call_prep p left join agent_runs r on r.id = p.run_id where p.call_id = $1",
    )
    .bind(call_id)
    .fetch_optional(db)
    .await
}

pub fn spawn_worker(db: PgPool) {
    tokio::spawn(async move {
        crate::mail::every(Duration::from_secs(10 * 60), || async { queue_due(&db).await.map(|_| ()).map_err(Into::into) }, "call prep").await
    });
}

#[derive(Deserialize)]
struct Input {
    prep_id: Uuid,
}

#[derive(Deserialize)]
struct Answer {
    opener: String,
    questions: Vec<String>,
    objections: Vec<Handle>,
    voicemail: String,
    reminder: String,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "opener": { "type": "string" },
            "questions": { "type": "array", "items": { "type": "string" } },
            "objections": { "type": "array", "items": { "type": "object", "properties": {
                "objection": { "type": "string" }, "answer": { "type": "string" }
            }, "required": ["objection", "answer"] } },
            "voicemail": { "type": "string" },
            "reminder": { "type": "string" }
        },
        "required": ["opener", "questions", "objections", "voicemail", "reminder"]
    })
}

#[derive(sqlx::FromRow)]
struct Row {
    lead_id: Uuid,
    segment_id: Option<Uuid>,
    owner_id: Option<Uuid>,
    instructions: String,
    status: String,
    written: bool,
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let row = sqlx::query_as::<_, Row>(
        "select c.lead_id, c.segment_id, coalesce(c.owner_id, l.owner_id) as owner_id, c.instructions, c.status, p.prep is not null as written
         from call_prep p join calls c on c.id = p.call_id join leads l on l.id = c.lead_id where p.id = $1 and p.project_id = $2",
    )
    .bind(input.prep_id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the call was deleted")?;
    if row.written {
        return Ok(Outcome::new("The talk track is already written"));
    }
    if row.status != "open" {
        return Ok(Outcome::new("The call was already made or skipped"));
    }
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(row.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    let settings = WriterSettings::load(ctx, lead.project_id).await?;
    let signals = company_signals(ctx, &[&lead]).await?;
    let about = lead_brief(&lead, &signals);
    let context = gather(db, &lead, org_id).await?;
    let materials = materials(ctx, lead.project_id).await?;
    let earlier = earlier_calls(db, lead.id).await?;
    let heard = heard(db, row.segment_id).await?;
    let coaching = match row.owner_id {
        Some(owner) => coaching(db, lead.project_id, owner).await?,
        None => vec![],
    };

    let us = settings.sender_at(&profile.name);
    let step = if row.instructions.trim().is_empty() { String::new() } else { format!("\nWhat this call is for: {}", row.instructions.trim()) };
    let mut extra = String::new();
    if !earlier.is_empty() {
        extra.push_str(&format!("\n\nOur earlier calls with them:\n{}", earlier.join("\n")));
    }
    if !heard.is_empty() {
        extra.push_str(&format!("\n\nObjections leads in this campaign raised, with answers that work:\n{}", heard.join("\n")));
    }
    let reminder = if coaching.is_empty() {
        "5. reminder: empty, since the caller has no coaching yet.".to_string()
    } else {
        extra.push_str(&format!("\n\nWhat the caller was coached to do differently on recent calls:\n{}", coaching.join("\n")));
        "5. reminder: the one coaching point that matters most for this call, in under 25 words, as advice to the caller.".to_string()
    };
    let prompt = format!(
        "{us} is about to phone a lead. Write the talk track for the call.{step}\n\n\
         What we sell: {name}. {description}\nOur materials:\n{materials}\n\n\
         The lead and their company:\n{about}\n\nWhat else Splee knows:\n{context}{extra}\n\n\
         1. opener: what to say once they pick up, in under 40 words: who is calling, a reason for the call tied to something \
         they did or their situation, and a question that's easy to answer. Never ask \"is now a good time\".\n\
         2. questions: 3 open questions that find out their situation, the pain and its cost, and who else decides, each under 20 words.\n\
         3. objections: up to 3 objections this lead is likely to raise, from what they said or their situation, each with an answer \
         under 35 words that acknowledges it and ends with a question. Use the campaign's answers where they fit.\n\
         4. voicemail: a message under 45 words to leave if they don't pick up, with one reason to call back and no pressure.\n\
         {reminder}",
        name = profile.name,
        description = profile.description,
    );
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    ctx.log(format!("writing the talk track for the call to {first}…")).await?;
    let call = Call { timeout: Duration::from_secs(2 * 60), budget_usd: 0.3, ..Call::writing(PREPPER, prompt, schema()) };
    let prep = tidy(ctx.ask::<Answer>(&call).await?, !coaching.is_empty());

    sqlx::query("update call_prep set prep = $2 where id = $1 and run_id = $3 and prep is null")
        .bind(input.prep_id)
        .bind(json!(prep))
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    Ok(Outcome::new(format!("Wrote the talk track for the call to {first}")))
}

async fn earlier_calls(db: &PgPool, lead_id: Uuid) -> sqlx::Result<Vec<String>> {
    let rows: Vec<(Ts, String, String)> = sqlx::query_as(
        "select coalesce(called_at, closed_at, created_at), coalesce(outcome, ''), coalesce(nullif(summary, ''), notes) from calls
         where lead_id = $1 and status = 'done' order by coalesce(called_at, closed_at, created_at) desc limit 3",
    )
    .bind(lead_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .rev()
        .map(|(at, outcome, what)| {
            let outcome = outcome.replace('_', " ");
            format!("- {} ({}): {}", at.format("%-d %b"), if outcome.is_empty() { "logged" } else { &outcome }, what.trim())
        })
        .collect())
}

/// The objections the campaign's latest insights found in replies, with their answers.
async fn heard(db: &PgPool, segment_id: Option<Uuid>) -> anyhow::Result<Vec<String>> {
    let Some(segment_id) = segment_id else { return Ok(vec![]) };
    let Some(insights) = super::insights::latest(db, segment_id).await?.and_then(|s| s.insights) else { return Ok(vec![]) };
    let objections: Vec<super::insights::Objection> = serde_json::from_value(insights["objections"].clone()).unwrap_or_default();
    Ok(objections.iter().map(|o| format!("- {}: {}", o.theme, o.answer)).collect())
}

/// What the caller's last 3 coached calls said to do differently.
async fn coaching(db: &PgPool, project_id: Uuid, owner_id: Uuid) -> sqlx::Result<Vec<String>> {
    let rows: Vec<(Value,)> = sqlx::query_as(
        "select coaching -> 'improve' from call_coaching where project_id = $1 and owner_id = $2 and coaching is not null
         order by created_at desc limit 3",
    )
    .bind(project_id)
    .bind(owner_id)
    .fetch_all(db)
    .await?;
    let changes = rows.iter().flat_map(|(v,)| serde_json::from_value::<Vec<super::coaching::Change>>(v.clone()).unwrap_or_default());
    Ok(changes.map(|c| format!("- {}: {}", c.moment, c.instead)).collect())
}

fn tidy(a: Answer, coached: bool) -> Prep {
    let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    Prep {
        opener: clip(&a.opener, 400),
        questions: a.questions.iter().map(|q| clip(q, 200)).filter(|q| !q.is_empty()).take(3).collect(),
        objections: a
            .objections
            .iter()
            .filter(|h| !h.objection.trim().is_empty() && !h.answer.trim().is_empty())
            .take(3)
            .map(|h| Handle { objection: clip(&h.objection, 150), answer: clip(&h.answer, 400) })
            .collect(),
        voicemail: clip(&a.voicemail, 400),
        // Keep this empty without coaching: anything written then is the model's guess, not something the caller was told.
        reminder: if coached { clip(&a.reminder, 250) } else { String::new() },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_due_call_gets_one_talk_track_with_the_callers_coaching(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(&db).await.unwrap();
        let rep: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('sam@acme.example', 'Sam', 'x') returning id").fetch_one(&db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, first_name, company_name, owner_id) values ($1, 'Noor Haddad', 'Noor', 'Quill Labs', $2) returning id")
            .bind(project)
            .bind(rep)
            .fetch_one(&db)
            .await
            .unwrap();
        let other: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, owner_id) values ($1, 'Lee Park', $2) returning id")
            .bind(project)
            .bind(rep)
            .fetch_one(&db)
            .await
            .unwrap();
        let call = |lead: Uuid, status: &'static str, due: &'static str| {
            sqlx::query_scalar::<_, Uuid>(
                "insert into calls (project_id, lead_id, kind, status, due_at, instructions, summary, called_at)
                 values ($1, $2, 'manual', $3, now() + $4::interval, 'Ask about the trial', 'She liked the demo.', now() - interval '3 days') returning id",
            )
            .bind(project)
            .bind(lead)
            .bind(status)
            .bind(due)
            .fetch_one(&db)
        };
        let due = call(lead, "open", "2 hours").await.unwrap();
        call(lead, "open", "3 days").await.unwrap();
        call(lead, "done", "-3 days").await.unwrap();
        let earlier_call = call(other, "done", "-1 day").await.unwrap();
        sqlx::query("insert into call_coaching (project_id, lead_id, call_id, owner_id, coaching) values ($1, $2, $3, $4, $5)")
            .bind(project)
            .bind(other)
            .bind(earlier_call)
            .bind(rep)
            .bind(json!({ "score": 4, "next_step_agreed": false, "next_step": "", "strengths": [], "covered": [],
                "improve": [{ "moment": "Pitched too early", "quote": "", "instead": "Ask what missing notes cost before pitching." }] }))
            .execute(&db)
            .await
            .unwrap();

        assert_eq!(queue_due(&db).await.unwrap(), 1, "only the open call due soon");
        assert_eq!(queue_due(&db).await.unwrap(), 0, "once");
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'call_prep' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |_| {
            json!({
                "opener": " Hi Noor, it's Sam from Fieldnote. ", "questions": ["How do reps log calls?", " ", "Who owns the CRM?", "What does it cost?", "A fourth"],
                "objections": [{ "objection": "We use Gong", "answer": "Many teams do. What would you change about it?" }, { "objection": " ", "answer": "x" }],
                "voicemail": "Hi Noor, Sam here.", "reminder": "Ask what missing notes cost before you pitch."
            })
        });
        run(&ctx).await.unwrap();
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("What this call is for: Ask about the trial"), "{prompt}");
        assert!(prompt.contains("(logged): She liked the demo.") && !prompt.contains("Lee Park"), "this lead's earlier calls only: {prompt}");
        assert!(prompt.contains("- Pitched too early: Ask what missing notes cost before pitching."), "the caller's coaching: {prompt}");

        let saved = latest(&db, due).await.unwrap().unwrap();
        assert_eq!(saved.status, "ready");
        let prep: Prep = serde_json::from_value(saved.prep.unwrap()).unwrap();
        assert_eq!(prep.opener, "Hi Noor, it's Sam from Fieldnote.");
        assert_eq!(prep.questions, vec!["How do reps log calls?", "Who owns the CRM?", "What does it cost?"]);
        assert_eq!(prep.objections.len(), 1);
        assert_eq!(prep.reminder, "Ask what missing notes cost before you pitch.");

        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(ctx.run.id).execute(&db).await.unwrap();
        let again = request(&db, due, project, org).await.unwrap();
        let saved = latest(&db, due).await.unwrap().unwrap();
        assert_eq!((saved.status.as_str(), saved.prep.is_none()), ("writing", true), "writing again clears it");
        assert_ne!(again, ctx.run.id);
        assert_eq!(request(&db, due, project, org).await.unwrap(), again, "a track being written isn't asked for twice");
    }

    #[test]
    fn without_coaching_the_reminder_stays_empty() {
        let a = Answer { opener: "Hi".into(), questions: vec![], objections: vec![], voicemail: "".into(), reminder: "Slow down.".into() };
        assert_eq!(tidy(a, false).reminder, "");
    }
}
