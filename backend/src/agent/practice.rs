//! Practice calls: a rep rehearses the next call on an open deal with the agent playing the buyer. Setting up reads what
//! Splee knows about the deal (the conversation, the gaps in its qualification and the competitors it's up against, with
//! their battlecards' objections) and builds the buyer, kept from the rep, with the line they open with. Each reply is one
//! short call in character. Scoring reads the whole call against what the buyer cared about and would agree to.
//! A session has one step running at a time, its `run_id`.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::{Ctx, Outcome, battlecards, briefs, kind, qualify};
use crate::models::{Lead, Ts};

pub const SCENARIOS: [&str; 3] = ["next_call", "pricing", "competitor"];
/// Turns a session can have, both sides, before the buyer has to go.
pub const MAX_TURNS: usize = 40;
pub const MAX_LINE: usize = 1500;

const ACTOR: &str = "You play a B2B buyer in a sales training role-play for Splee, an outbound sales tool. Stay in character \
as a real, busy person: polite but not easy, never helpful for the sake of it. Use only the facts you're given about them and \
never invent numbers or events they couldn't know. You never coach the salesperson or break character.";
const COACH: &str = "You coach B2B salespeople for Splee, an outbound sales tool, like a sharp, kind sales manager reviewing a \
practice call. Judge from the transcript only and quote the rep's words exactly as written. Plain text only.";

/// A session as the practice window shows it. `status` is preparing, thinking, scoring, live, ended, scored or failed,
/// which a cancelled step counts as, so it can be tried again.
/// `persona` is the buyer's name, title and company until it's scored, then all of it.
pub const SELECT: &str = "select s.id, s.deal_id, s.scenario, s.persona, s.turns, s.ended, s.score, nullif(u.name, '') as user_name,
        case when r.status in ('queued', 'running') then
               case when s.persona is null then 'preparing' when s.ended then 'scoring' else 'thinking' end
             when r.status in ('failed', 'cancelled') then 'failed' when s.score is not null then 'scored' when s.ended then 'ended'
             when s.persona is null then 'failed' else 'live' end as status,
        r.error, s.created_at
     from practice_sessions s left join agent_runs r on r.id = s.run_id left join users u on u.id = s.user_id";

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Session {
    pub id: Uuid,
    pub deal_id: Uuid,
    pub scenario: String,
    pub persona: Option<Value>,
    pub turns: Value,
    pub ended: bool,
    pub score: Option<Value>,
    pub user_name: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub created_at: Ts,
}

impl Session {
    /// Keeps what the buyer thinks to themselves from the rep until it's scored.
    pub fn for_rep(mut self) -> Self {
        if self.score.is_none() {
            self.persona = self.persona.map(|p| json!({ "name": p["name"], "title": p["title"], "company": p["company"] }));
        }
        self
    }

    pub fn busy(&self) -> bool {
        matches!(self.status.as_str(), "preparing" | "thinking" | "scoring")
    }
}

pub async fn load(db: &PgPool, id: Uuid) -> sqlx::Result<Option<Session>> {
    sqlx::query_as::<_, Session>(&format!("{SELECT} where s.id = $1")).bind(id).fetch_optional(db).await
}

/// Points the session at a new run for its next step.
pub async fn start(tx: &mut sqlx::PgConnection, session: Uuid, project_id: Uuid, org_id: Uuid, step: &str) -> sqlx::Result<Uuid> {
    let run = super::enqueue(&mut *tx, kind::PRACTICE, Some(project_id), Some(org_id), json!({ "session_id": session, "step": step })).await?;
    sqlx::query("update practice_sessions set run_id = $2 where id = $1").bind(session).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Turn {
    pub who: String,
    pub text: String,
}

#[derive(Deserialize)]
struct Input {
    session_id: Uuid,
    step: String,
}

#[derive(sqlx::FromRow)]
struct Row {
    deal_id: Uuid,
    scenario: String,
    persona: Option<Value>,
    turns: Value,
    lead_id: Uuid,
    stage: String,
    value_cents: i64,
    currency: String,
    notes: String,
}

#[derive(Deserialize, Serialize)]
struct Persona {
    name: String,
    title: String,
    company: String,
    situation: String,
    cares_about: Vec<String>,
    objections: Vec<String>,
    agrees_if: String,
    opening: String,
}

#[derive(Deserialize)]
struct Reply {
    reply: String,
    hangs_up: bool,
}

#[derive(Deserialize, Serialize)]
struct Moment {
    you_said: String,
    #[serde(rename = "try")]
    instead: String,
}

#[derive(Deserialize, Serialize)]
struct Score {
    score: i64,
    summary: String,
    went_well: Vec<String>,
    try_next: Vec<String>,
    moments: Vec<Moment>,
}

fn list(items: &[String]) -> String {
    items.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n")
}

/// The transcript as the model reads it, the buyer by first name and the rep as "Rep".
fn transcript(turns: &[Turn], buyer: &str) -> String {
    turns.iter().map(|t| format!("{}: {}", if t.who == "rep" { "Rep" } else { buyer }, t.text)).collect::<Vec<_>>().join("\n")
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let row = sqlx::query_as::<_, Row>(
        "select s.deal_id, s.scenario, s.persona, s.turns, d.lead_id, st.name as stage, d.value_cents, d.currency, d.notes
         from practice_sessions s join deals d on d.id = s.deal_id join deal_stages st on st.id = d.stage_id
         where s.id = $1 and s.project_id = $2 and s.run_id = $3",
    )
    .bind(input.session_id)
    .bind(ctx.project_id()?)
    .bind(ctx.run.id)
    .fetch_optional(db)
    .await?
    .context("the practice call was deleted or moved on")?;
    let turns: Vec<Turn> = serde_json::from_value(row.turns.clone())?;
    match input.step.as_str() {
        "setup" => setup(ctx, input.session_id, &row).await,
        "reply" => reply(ctx, input.session_id, &row, &turns).await,
        "score" => score(ctx, input.session_id, &row, &turns).await,
        other => anyhow::bail!("unknown practice step {other}"),
    }
}

async fn setup(ctx: &Ctx, id: Uuid, row: &Row) -> anyhow::Result<Outcome> {
    let db = ctx.db();
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(row.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    let context = briefs::gather(db, &lead, org_id).await?;
    let gaps = qualify::gaps(db, lead.id).await?;
    let rivals = battlecards::up_against(db, row.deal_id, lead.project_id).await?;
    let mut against = vec![];
    for rival in &rivals {
        let objections: Vec<String> = rival
            .card
            .as_ref()
            .and_then(|c| serde_json::from_value::<battlecards::Card>(c.clone()).ok())
            .map(|c| c.objections.into_iter().map(|o| o.they_say).take(3).collect())
            .unwrap_or_default();
        against.push(if objections.is_empty() { rival.name.clone() } else { format!("{}, whose buyers say: {}", rival.name, objections.join(" / ")) });
    }
    let scenario = match row.scenario.as_str() {
        "pricing" => "They push back hard on price and budget, and want to know why it's worth it now.",
        "competitor" if !against.is_empty() => "They're seriously looking at a competitor and bring up its points.",
        "competitor" => "They're seriously looking at another tool and bring up why it might be enough.",
        _ => "It's the next call on this deal, as the conversation stands.",
    };
    let prompt = format!(
        "Build the buyer for a practice call, from what we know about them.\n\n\
         Them: {who}, {title}.\nTheir deal is in {stage}{value}.{notes}\n\n\
         What we sell: {name}. {description}\n\nThe conversation and what else Splee knows:\n{context}\n{gaps}{against}\n\n\
         The practice: {scenario}\n\n\
         1. name, title, company: theirs.\n\
         2. situation: two or three sentences, where they stand now, from what they said and did.\n\
         3. cares_about: up to 3 things that matter to them in this decision, from the evidence.\n\
         4. objections: up to 3 objections they'll raise in this practice, in their words.\n\
         5. agrees_if: what the rep would have to show or ask for them to agree to a next step.\n\
         6. opening: the first thing they say when the call starts, one or two short sentences.",
        who = lead.full_name,
        title = [lead.title.as_str(), lead.company_name.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" at "),
        stage = row.stage,
        value = if row.value_cents > 0 { format!(", worth {} {}", row.value_cents / 100, row.currency.to_uppercase()) } else { String::new() },
        notes = if row.notes.trim().is_empty() { String::new() } else { format!(" The deal's notes: {}", row.notes.trim()) },
        name = profile.name,
        description = profile.description,
        gaps = if gaps.is_empty() { String::new() } else { format!("What we still don't know about the deal:\n{gaps}\n") },
        against = if against.is_empty() { String::new() } else { format!("Competitors in this deal: {}.", against.join("; ")) },
    );
    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" }, "title": { "type": "string" }, "company": { "type": "string" },
            "situation": { "type": "string" },
            "cares_about": { "type": "array", "items": { "type": "string" } },
            "objections": { "type": "array", "items": { "type": "string" } },
            "agrees_if": { "type": "string" }, "opening": { "type": "string" }
        },
        "required": ["name", "title", "company", "situation", "cares_about", "objections", "agrees_if", "opening"]
    });
    ctx.log(format!("getting {} ready for the call…", lead.full_name)).await?;
    let call = Call { budget_usd: 0.2, ..Call::writing(ACTOR, prompt, schema) };
    let mut persona: Persona = ctx.ask(&call).await?;
    persona.cares_about.truncate(3);
    persona.objections.truncate(3);
    let opening = Turn { who: "buyer".into(), text: persona.opening.trim().to_string() };
    sqlx::query("update practice_sessions set persona = $2, turns = jsonb_build_array($3::jsonb) where id = $1 and run_id = $4")
        .bind(id)
        .bind(serde_json::to_value(&persona)?)
        .bind(serde_json::to_value(&opening)?)
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    Ok(Outcome::new(format!("{} is on the line", persona.name)))
}

fn persona_of(row: &Row) -> anyhow::Result<Persona> {
    serde_json::from_value(row.persona.clone().context("the buyer isn't ready")?).context("the buyer is unreadable")
}

fn brief(p: &Persona) -> String {
    format!(
        "You are {name}, {title} at {company}. {situation}\nWhat matters to you:\n{cares}\nObjections you have:\n{objections}\n\
         You'd agree to a next step if: {agrees}",
        name = p.name,
        title = p.title,
        company = p.company,
        situation = p.situation,
        cares = list(&p.cares_about),
        objections = list(&p.objections),
        agrees = p.agrees_if,
    )
}

async fn reply(ctx: &Ctx, id: Uuid, row: &Row, turns: &[Turn]) -> anyhow::Result<Outcome> {
    let persona = persona_of(row)?;
    let first = persona.name.split_whitespace().next().unwrap_or(&persona.name).to_string();
    let last_call = turns.len() + 1 >= MAX_TURNS;
    let prompt = format!(
        "{brief}\n\nThe call so far:\n{transcript}\n\n\
         Answer the rep's last line as {first}, out loud on a call: 1 to 3 short sentences, no stage directions. Raise your \
         objections when they fit, answer good questions honestly, and push back on vague claims. Agree to a next step only \
         once the rep has earned it as above. hangs_up is true when you end the call, after agreeing to a next step, or \
         saying no, or if the rep says goodbye.{last}",
        brief = brief(&persona),
        transcript = transcript(turns, &first),
        last = if last_call { " You're out of time: wrap up and end the call now." } else { "" },
    );
    let schema = json!({
        "type": "object",
        "properties": { "reply": { "type": "string" }, "hangs_up": { "type": "boolean" } },
        "required": ["reply", "hangs_up"]
    });
    let call = Call { budget_usd: 0.1, timeout: Duration::from_secs(90), ..Call::writing(ACTOR, prompt, schema) };
    let answer: Reply = ctx.ask(&call).await?;
    let turn = Turn { who: "buyer".into(), text: answer.reply.trim().to_string() };
    sqlx::query("update practice_sessions set turns = turns || jsonb_build_array($2::jsonb), ended = $3 where id = $1 and run_id = $4")
        .bind(id)
        .bind(serde_json::to_value(&turn)?)
        .bind(answer.hangs_up || last_call)
        .bind(ctx.run.id)
        .execute(ctx.db())
        .await?;
    Ok(Outcome::new(format!("{first} answered")))
}

async fn score(ctx: &Ctx, id: Uuid, row: &Row, turns: &[Turn]) -> anyhow::Result<Outcome> {
    let persona = persona_of(row)?;
    let first = persona.name.split_whitespace().next().unwrap_or(&persona.name).to_string();
    let prompt = format!(
        "A rep practiced a sales call with an actor playing a real buyer. The actor's brief:\n{brief}\n\n\
         The call:\n{transcript}\n\n\
         Score the rep.\n\
         1. score: 0 to 100. 80 or more only if they found what mattered to {first}, handled the objections with substance \
         and earned a clear next step. Under 50 if they pitched without asking or let an objection go.\n\
         2. summary: one or two sentences, how the call went.\n\
         3. went_well: up to 3 things to keep doing, each tied to what they said.\n\
         4. try_next: up to 3 things to change, each tied to the brief: what {first} cared about, an objection or what they'd \
         agree to.\n\
         5. moments: up to 3 lines of the rep's to say differently: you_said exactly as written, and try, what to say instead.",
        brief = brief(&persona),
        transcript = transcript(turns, &first),
    );
    let schema = json!({
        "type": "object",
        "properties": {
            "score": { "type": "integer" }, "summary": { "type": "string" },
            "went_well": { "type": "array", "items": { "type": "string" } },
            "try_next": { "type": "array", "items": { "type": "string" } },
            "moments": { "type": "array", "items": { "type": "object",
                "properties": { "you_said": { "type": "string" }, "try": { "type": "string" } }, "required": ["you_said", "try"] } }
        },
        "required": ["score", "summary", "went_well", "try_next", "moments"]
    });
    ctx.log("scoring the call…").await?;
    let call = Call { budget_usd: 0.2, ..Call::writing(COACH, prompt, schema) };
    let mut score: Score = ctx.ask(&call).await?;
    score.score = score.score.clamp(0, 100);
    score.went_well.truncate(3);
    score.try_next.truncate(3);
    // A line the rep never said isn't theirs to change.
    let said: Vec<&str> = turns.iter().filter(|t| t.who == "rep").map(|t| t.text.as_str()).collect();
    score.moments.retain(|m| !m.you_said.trim().is_empty() && said.iter().any(|s| s.contains(m.you_said.trim())));
    score.moments.truncate(3);
    sqlx::query("update practice_sessions set score = $2 where id = $1 and run_id = $3")
        .bind(id)
        .bind(serde_json::to_value(&score)?)
        .bind(ctx.run.id)
        .execute(ctx.db())
        .await?;
    Ok(Outcome::new(format!("Scored {} out of 100", score.score)))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    async fn run_next(db: &PgPool, answer: Value) -> String {
        let started = sqlx::query_as::<_, AgentRun>(
            "update agent_runs set status = 'running' where id = (select id from agent_runs where kind = 'practice' and status = 'queued' limit 1) returning *",
        )
        .fetch_one(db)
        .await
        .unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, move |_| answer.clone());
        run(&ctx).await.unwrap();
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(ctx.run.id).execute(db).await.unwrap();
        super::super::fake::prompts(ctx.run.id).first().cloned().unwrap_or_default()
    }

    async fn step(db: &PgPool, session: Uuid, project: Uuid, org: Uuid, name: &str) {
        let mut tx = db.begin().await.unwrap();
        start(&mut tx, session, project, org, name).await.unwrap();
        tx.commit().await.unwrap();
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_buyer_is_built_from_the_deal_answers_in_character_and_the_call_is_scored_on_what_the_rep_said(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, full_name, first_name, title, company_name, company_domain, email) values ($1, 'Ines Duarte', 'Ines', 'VP Sales', 'Lumenfold', 'lumenfold.example', 'ines@lumenfold.example') returning id",
        )
        .bind(project)
        .fetch_one(&db)
        .await
        .unwrap();
        let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Proposal', 'open', 3) returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        let deal: Uuid = sqlx::query_scalar(
            "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, notes) values ($1, $2, $3, $4, 'open', 480000, 'usd', 'Comparing us with Gong') returning id",
        )
        .bind(org)
        .bind(project)
        .bind(lead)
        .bind(stage)
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query("insert into competitors (project_id, name, domain) values ($1, 'Gong', 'gong.example')").bind(project).execute(&db).await.unwrap();
        let session: Uuid = sqlx::query_scalar("insert into practice_sessions (project_id, deal_id, scenario) values ($1, $2, 'competitor') returning id")
            .bind(project)
            .bind(deal)
            .fetch_one(&db)
            .await
            .unwrap();
        step(&db, session, project, org, "setup").await;
        assert_eq!(load(&db, session).await.unwrap().unwrap().status, "preparing");

        let prompt = run_next(
            &db,
            json!({ "name": "Ines Duarte", "title": "VP Sales", "company": "Lumenfold", "situation": "Comparing tools.",
                    "cares_about": ["German calls", "HubSpot", "Price", "Extra"], "objections": ["Gong does this"],
                    "agrees_if": "The rep offers a trial on their own calls.", "opening": "Hi, I have ten minutes." }),
        )
        .await;
        assert!(prompt.contains("Their deal is in Proposal, worth 4800 USD. The deal's notes: Comparing us with Gong") && prompt.contains("Competitors in this deal: Gong."), "{prompt}");
        let s = load(&db, session).await.unwrap().unwrap();
        assert_eq!((s.status.as_str(), s.turns.clone()), ("live", json!([{ "who": "buyer", "text": "Hi, I have ten minutes." }])));
        assert_eq!(s.persona.as_ref().unwrap()["cares_about"].as_array().unwrap().len(), 3);
        let shown = load(&db, session).await.unwrap().unwrap().for_rep();
        assert_eq!(shown.persona, Some(json!({ "name": "Ines Duarte", "title": "VP Sales", "company": "Lumenfold" })), "what they think stays hidden");

        sqlx::query("update practice_sessions set turns = turns || '[{\"who\": \"rep\", \"text\": \"Thanks Ines. What made you look at Gong?\"}]' where id = $1")
            .bind(session)
            .execute(&db)
            .await
            .unwrap();
        step(&db, session, project, org, "reply").await;
        assert_eq!(load(&db, session).await.unwrap().unwrap().status, "thinking");
        let prompt = run_next(&db, json!({ "reply": "Their coaching looked good.", "hangs_up": false })).await;
        assert!(prompt.contains("You are Ines Duarte, VP Sales at Lumenfold.") && prompt.contains("Rep: Thanks Ines. What made you look at Gong?"), "{prompt}");
        let s = load(&db, session).await.unwrap().unwrap();
        assert_eq!((s.status.as_str(), s.turns.as_array().unwrap().len()), ("live", 3));

        sqlx::query("update practice_sessions set ended = true where id = $1").bind(session).execute(&db).await.unwrap();
        step(&db, session, project, org, "score").await;
        assert_eq!(load(&db, session).await.unwrap().unwrap().status, "scoring");
        run_next(
            &db,
            json!({ "score": 140, "summary": "Good opening.", "went_well": ["Asked why"], "try_next": ["Offer a trial"],
                    "moments": [{ "you_said": "What made you look at Gong?", "try": "What would Gong have to do to win?" },
                                { "you_said": "We're cheaper", "try": "Never said" }] }),
        )
        .await;
        let s = load(&db, session).await.unwrap().unwrap().for_rep();
        let score = s.score.unwrap();
        assert_eq!((s.status.as_str(), score["score"].as_i64(), score["moments"].as_array().unwrap().len()), ("scored", Some(100), 1), "a line never said is dropped");
        assert_eq!(s.persona.unwrap()["agrees_if"], "The rep offers a trial on their own calls.", "revealed once scored");
    }
}
