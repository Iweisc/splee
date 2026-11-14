//! Call coaching: once a sales meeting (meetings/) or a call (calls.rs) has its summary, the agent reads the transcript and
//! scores it from 1 to 10, with up to 3 things to keep doing and up to 3 to change, each tied to a moment, the words said
//! and what to say instead. How much we talked and how many questions we asked are counted here from the transcript's
//! speaker lines, never asked of the model, and a quote that isn't in the transcript is dropped.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::{Ctx, Outcome, kind, qualify};
use crate::models::Lead;
use crate::state::AppState;

/// Meetings and calls older than this aren't coached when coaching is first switched on.
const DAYS: i32 = 30;
const PER_PASS: i64 = 30;
const PROMPT_CHARS: usize = 60_000;
const COACH: &str = "You coach B2B salespeople for Splee, an outbound sales tool, like a sharp, kind sales manager listening to a \
recording. Judge from the transcript only. Quote words exactly as the transcript has them. Plain text only.";

/// Who talked how much, by words, and the questions we asked.
#[derive(Debug, Default, PartialEq)]
pub struct Talk {
    pub ours: usize,
    pub theirs: usize,
    pub questions: usize,
}

impl Talk {
    /// Our share of the words, or None when no line could be put on either side.
    pub fn share(&self) -> Option<i32> {
        let total = self.ours + self.theirs;
        (total > 0).then(|| ((self.ours * 100 + total / 2) / total) as i32)
    }
}

/// The speaker and words of a "Speaker: words" line, when it starts with a short name before a colon.
fn speaker(line: &str) -> Option<(&str, &str)> {
    let (who, text) = line.split_once(':')?;
    let who = who.trim();
    (!who.is_empty() && who.chars().count() <= 60 && !who.contains("http")).then_some((who, text))
}

/// Counts words per side. A line without a speaker belongs to the speaker before it.
pub fn talk(transcript: &str, ours: impl Fn(&str) -> bool) -> Talk {
    let mut t = Talk::default();
    let mut current: Option<bool> = None;
    for line in transcript.lines() {
        let text = match speaker(line) {
            Some((who, text)) => {
                current = Some(ours(who));
                text
            }
            None => line,
        };
        let words = text.split_whitespace().count();
        match current {
            Some(true) => {
                t.ours += words;
                t.questions += text.matches('?').count();
            }
            Some(false) => t.theirs += words,
            None => {}
        }
    }
    t
}

/// Whether a meeting's speaker is on our team. Meetings keep only the guests as attendees (meetings/mod.rs), so anyone who
/// isn't the lead or a guest, by full name, first name or the name before their email's @, is ours.
fn our_speaker(attendees: &Value, lead: &Lead) -> impl Fn(&str) -> bool + use<> {
    fn norm(s: &str) -> String {
        s.trim().to_lowercase()
    }
    fn first(s: &str) -> String {
        norm(s).split_whitespace().next().unwrap_or_default().to_string()
    }
    let mut guests = vec![norm(&lead.full_name), first(&lead.full_name), norm(&lead.first_name)];
    for a in attendees.as_array().into_iter().flatten() {
        let name = a["name"].as_str().unwrap_or_default();
        let email = a["email"].as_str().unwrap_or_default();
        guests.extend([norm(name), first(name), norm(email.split('@').next().unwrap_or_default())]);
    }
    guests.retain(|s| !s.is_empty());
    move |who: &str| !guests.contains(&norm(who)) && !guests.contains(&first(who))
}

fn speakers(transcript: &str) -> Vec<String> {
    let mut found: Vec<String> = vec![];
    for (who, _) in transcript.lines().filter_map(speaker) {
        if !found.iter().any(|f| f == who) {
            found.push(who.to_string());
        }
    }
    found
}

/// Queues coaching for every summarized meeting and call of the last DAYS that has none, in projects that keep it on.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "with due as (
           insert into call_coaching (project_id, lead_id, meeting_id, call_id, owner_id)
           select m.project_id, m.lead_id, m.id, null::uuid, l.owner_id from meetings m join leads l on l.id = m.lead_id
           join project_settings s on s.project_id = m.project_id and s.coach_calls
           where m.summarized_at is not null and m.transcript <> '' and m.started_at > now() - make_interval(days => $1)
             and not exists (select 1 from call_coaching c where c.meeting_id = m.id)
           union all
           select c.project_id, c.lead_id, null, c.id, coalesce(c.owner_id, l.owner_id) from calls c join leads l on l.id = c.lead_id
           join project_settings s on s.project_id = c.project_id and s.coach_calls
           where c.summary <> '' and c.transcript <> '' and coalesce(c.called_at, c.created_at) > now() - make_interval(days => $1)
             and not exists (select 1 from call_coaching k where k.call_id = c.id)
           limit $2
           on conflict do nothing
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id where p.org_id is not null",
    )
    .bind(DAYS)
    .bind(PER_PASS)
    .fetch_all(&mut *tx)
    .await?;
    for (id, project_id, org_id) in &due {
        let run = super::enqueue(&mut *tx, kind::COACH_CALL, Some(*project_id), Some(*org_id), json!({ "coaching_id": id })).await?;
        sqlx::query("update call_coaching set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(due.len())
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        crate::mail::every(Duration::from_secs(10 * 60), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "call coaching").await
    });
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Change {
    pub moment: String,
    /// Words from the transcript, or empty.
    pub quote: String,
    pub instead: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Coaching {
    pub score: i32,
    pub next_step_agreed: bool,
    pub next_step: String,
    pub strengths: Vec<String>,
    pub improve: Vec<Change>,
    /// The MEDDICC elements the call got into, in qualify::ELEMENTS' order.
    pub covered: Vec<String>,
}

#[derive(Deserialize)]
struct Input {
    coaching_id: Uuid,
}

#[derive(Deserialize)]
struct Answer {
    score: i32,
    next_step_agreed: bool,
    next_step: String,
    strengths: Vec<String>,
    improve: Vec<Change>,
    covered: Vec<String>,
}

fn schema() -> Value {
    let keys: Vec<&str> = qualify::ELEMENTS.iter().map(|(k, _)| *k).collect();
    json!({
        "type": "object",
        "properties": {
            "score": { "type": "integer" }, "next_step_agreed": { "type": "boolean" }, "next_step": { "type": "string" },
            "strengths": { "type": "array", "items": { "type": "string" } },
            "improve": { "type": "array", "items": { "type": "object", "properties": {
                "moment": { "type": "string" }, "quote": { "type": "string" }, "instead": { "type": "string" }
            }, "required": ["moment", "quote", "instead"] } },
            "covered": { "type": "array", "items": { "type": "string", "enum": keys } }
        },
        "required": ["score", "next_step_agreed", "next_step", "strengths", "improve", "covered"]
    })
}

#[derive(sqlx::FromRow)]
struct Row {
    lead_id: Uuid,
    meeting_id: Option<Uuid>,
    call_id: Option<Uuid>,
    done: bool,
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let row = sqlx::query_as::<_, Row>("select lead_id, meeting_id, call_id, coaching is not null as done from call_coaching where id = $1 and project_id = $2")
        .bind(input.coaching_id)
        .bind(ctx.project_id()?)
        .fetch_optional(db)
        .await?
        .context("the coaching was deleted")?;
    if row.done {
        return Ok(Outcome::new("Already coached"));
    }
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(row.lead_id).fetch_one(db).await?;
    let (transcript, what, counted) = match (row.meeting_id, row.call_id) {
        (Some(id), _) => {
            let (transcript, attendees, title): (String, Value, String) =
                sqlx::query_as("select left(transcript, $2), attendees, title from meetings where id = $1").bind(id).bind(PROMPT_CHARS as i32).fetch_one(db).await?;
            let speakers = speakers(&transcript);
            let counted = talk(&transcript, our_speaker(&attendees, &lead));
            (transcript, format!("a sales meeting, \"{title}\". Speakers: {}", speakers.join(", ")), counted)
        }
        (_, Some(id)) => {
            let transcript: String = sqlx::query_scalar("select left(transcript, $2) from calls where id = $1").bind(id).bind(PROMPT_CHARS as i32).fetch_one(db).await?;
            let counted = talk(&transcript, |s| s == "Us");
            (transcript, "a phone call. \"Us\" is our caller and \"Lead\" the person called, from speech recognition, so expect small errors".to_string(), counted)
        }
        _ => anyhow::bail!("the coaching has neither a meeting nor a call"),
    };
    let (_, profile) = load_project(ctx, lead.project_id).await?;
    let share = counted.share();
    let numbers = match share {
        Some(s) => format!("We spoke {s}% of the words and asked {} questions.", counted.questions),
        None => "Who spoke how much couldn't be counted.".to_string(),
    };
    let elements = qualify::ELEMENTS.iter().map(|(k, what)| format!("- {k}. {what}")).collect::<Vec<_>>().join("\n");
    let who = [lead.full_name.trim(), lead.title.trim(), lead.company_name.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(", ");
    let prompt = format!(
        "Here is {what}, between {us} and a lead ({who}). {numbers}\n\n{transcript}\n\n\
         Coach our side of it.\n\
         1. score: 1 to 10. 8 or more: they found the buyer's pain, listened more than they pitched and agreed a dated next \
         step. 5 to 7: good parts but a clear miss. 4 or less: mostly pitching, no discovery or no next step.\n\
         2. strengths: up to 3 things we did well, each under 20 words, naming the moment.\n\
         3. improve: up to 3 changes that matter most. For each, the moment in under 10 words, the words said then copied \
         exactly from the transcript (ours or theirs), and what to say or do instead, under 30 words.\n\
         4. next_step_agreed: whether both sides agreed a concrete next step, and next_step: what it is, or empty.\n\
         5. covered: which of these the call got into:\n{elements}",
        us = profile.name,
    );
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    ctx.log(format!("coaching the call with {first}…")).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.4, ..Call::writing(COACH, prompt, schema()) };
    let coaching = tidy(ctx.ask::<Answer>(&call).await?, &transcript);

    sqlx::query("update call_coaching set coaching = $2, talk_share = $3, questions = $4 where id = $1 and run_id = $5 and coaching is null")
        .bind(input.coaching_id)
        .bind(json!(coaching))
        .bind(share)
        .bind(counted.questions as i32)
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    Ok(Outcome::new(format!("Coached the call with {first}: {}/10", coaching.score)))
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Clamps the score, keeps a quote only when the transcript has it, drops empty items and keeps known elements in order.
fn tidy(a: Answer, transcript: &str) -> Coaching {
    let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    let said = squash(transcript);
    let improve = a
        .improve
        .iter()
        .filter(|c| !c.instead.trim().is_empty())
        .take(3)
        .map(|c| {
            let quote = c.quote.trim().trim_matches('"').trim();
            let found = !quote.is_empty() && said.contains(&squash(quote));
            Change { moment: clip(&c.moment, 100), quote: if found { clip(quote, 300) } else { String::new() }, instead: clip(&c.instead, 300) }
        })
        .collect();
    let covered = qualify::ELEMENTS.iter().map(|(k, _)| k.to_string()).filter(|k| a.covered.contains(k)).collect();
    let next_step = clip(&a.next_step, 200);
    Coaching {
        score: a.score.clamp(1, 10),
        next_step_agreed: a.next_step_agreed && !next_step.is_empty(),
        next_step,
        strengths: a.strengths.iter().map(|s| clip(s, 200)).filter(|s| !s.is_empty()).take(3).collect(),
        improve,
        covered,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    #[test]
    fn talk_is_counted_by_words_on_each_side_with_our_questions() {
        let t = talk("Us: Hi Noor, how do reps log calls today? And who owns that?\nLead: By hand, mostly on Fridays.\nIt takes hours.\nUs: Got it.", |s| s == "Us");
        assert_eq!(t, Talk { ours: 14, theirs: 8, questions: 2 }, "a line without a speaker belongs to the one before");
        assert_eq!(t.share(), Some(64));
        assert_eq!(talk("no speakers here", |_| true).share(), None);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_summarized_meeting_is_coached_once_with_our_talk_counted_from_the_internal_attendees(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(&db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, first_name, company_name) values ($1, 'Noor Haddad', 'Noor', 'Quill Labs') returning id")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        let transcript = "Dana Reyes: Thanks for joining. What made you reply?\nNoor: Our new AEs hate CRM notes.\nSam Ito: How many AEs do you have?\nLee: Four, soon six.";
        let attendees = json!([{ "name": "", "email": "noor@quill.example" }, { "name": "Lee Park", "email": "lee@quill.example" }]);
        let meeting: Uuid = sqlx::query_scalar(
            "insert into meetings (org_id, project_id, lead_id, provider, external_id, title, started_at, attendees, transcript, summary, summarized_at)
             values ($1, $2, $3, 'fireflies', 'x1', 'Fieldnote intro', now() - interval '1 day', $4, $5, 'They want fewer CRM notes.', now()) returning id",
        )
        .bind(org)
        .bind(project)
        .bind(lead)
        .bind(&attendees)
        .bind(transcript)
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query("insert into meetings (org_id, project_id, lead_id, provider, external_id, started_at, transcript) values ($1, $2, $3, 'fireflies', 'x2', now(), 'Dana Reyes: Hi')")
            .bind(org)
            .bind(project)
            .bind(lead)
            .execute(&db)
            .await
            .unwrap();

        assert_eq!(queue_due(&db).await.unwrap(), 1, "only the summarized meeting");
        assert_eq!(queue_due(&db).await.unwrap(), 0, "once");
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'coach_call' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |_| {
            json!({
                "score": 14, "next_step_agreed": true, "next_step": "", "strengths": ["Opened with a question", " "],
                "improve": [
                    { "moment": "Sizing", "quote": "how many  AEs do you have?", "instead": "Ask what the notes cost them." },
                    { "moment": "Made up", "quote": "We'll sign today", "instead": "Ask for a date." },
                    { "moment": "Empty", "quote": "", "instead": " " }
                ],
                "covered": ["pain", "metrics", "nonsense"]
            })
        });
        run(&ctx).await.unwrap();
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("We spoke 59% of the words and asked 2 questions.") && prompt.contains("Speakers: Dana Reyes, Noor, Sam Ito, Lee"), "{prompt}");

        let (share, questions, coaching): (Option<i32>, i32, Value) =
            sqlx::query_as("select talk_share, questions, coaching from call_coaching where meeting_id = $1").bind(meeting).fetch_one(&db).await.unwrap();
        assert_eq!((share, questions), (Some(59), 2), "a guest and the lead by first name are theirs");
        let c: Coaching = serde_json::from_value(coaching).unwrap();
        assert_eq!(c.score, 10);
        assert!(!c.next_step_agreed, "agreed on nothing named isn't agreed");
        assert_eq!(c.strengths, vec!["Opened with a question"]);
        assert_eq!(c.improve.len(), 2);
        assert_eq!((c.improve[0].quote.as_str(), c.improve[1].quote.as_str()), ("how many  AEs do you have?", ""), "a quote the transcript lacks is dropped");
        assert_eq!(c.covered, vec!["metrics", "pain"], "known elements, in order");
    }
}
