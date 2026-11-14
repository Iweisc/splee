//! Sales playbooks: the agent reads what a project's team has learned (the latest buyer profile, the last year's win and loss
//! reviews, battlecards, each campaign's latest insights, the best coached calls and the emails that got hot replies) and
//! writes the playbook a new rep reads first: who to sell to, the pitch, discovery questions, objections and answers, how to
//! beat each competitor, what gets replies, how deals get won and what loses them. Written only when someone asks.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::{Ctx, Outcome, kind};
use crate::models::Ts;

/// Fewer things to read than this and there's nothing to write a playbook from.
pub const MIN_SOURCES: i64 = 3;
const WRITER: &str = "You write the sales playbook for a B2B sales team using Splee, an outbound sales tool, like its best \
sales leader onboarding a new rep. Use only what the team has learned, given below. Never invent customers, numbers, quotes \
or competitors. Write plainly, in the second person, as advice to the rep. Plain text only, no markdown.";

/// What the playbook read, for the page to say so.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Counts {
    pub won: i64,
    pub lost: i64,
    pub battlecards: i64,
    pub campaigns: i64,
    pub calls: i64,
    pub emails: i64,
    pub profile: bool,
}

impl Counts {
    pub fn total(&self) -> i64 {
        self.won + self.lost + self.battlecards + self.campaigns + self.calls + self.emails + i64::from(self.profile)
    }
}

/// The prompt's material and what it counts.
struct Material {
    counts: Counts,
    text: String,
    /// Competitors named in the battlecards and reviews, the only ones the playbook may name.
    competitors: Vec<String>,
}

async fn gather(db: &PgPool, project_id: Uuid) -> sqlx::Result<Material> {
    let mut counts = Counts::default();
    let mut parts = vec![];
    let text = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().trim().to_string();
    let items = |v: &Value, k: &str| v[k].as_array().cloned().unwrap_or_default();

    let profile: Option<Value> = sqlx::query_scalar("select profile from buyer_profiles where project_id = $1 and profile is not null order by created_at desc limit 1")
        .bind(project_id)
        .fetch_optional(db)
        .await?;
    if let Some(p) = profile {
        counts.profile = true;
        let traits = |k: &str| items(&p, k).iter().map(|t| format!("- {} ({} deals): {}", text(t, "trait"), t["count"], text(t, "why"))).collect::<Vec<_>>().join("\n");
        parts.push(format!("Who buys from us: {}\nWhat won deals share:\n{}\nWhat lost deals share:\n{}", text(&p, "summary"), traits("traits"), traits("avoid")));
    }

    let reviews: Vec<(String, Option<String>, String, String, String, String)> = sqlx::query_as(
        "select r.outcome, r.reason, r.turning_point, r.lesson, r.competitor, coalesce(nullif(l.company_name, ''), l.full_name)
         from deal_reviews r join deals d on d.id = r.deal_id join leads l on l.id = d.lead_id
         where r.project_id = $1 and r.reason is not null and r.created_at > now() - interval '12 months' order by r.created_at desc limit 40",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    let mut competitors: Vec<String> = vec![];
    if !reviews.is_empty() {
        let lines: Vec<String> = reviews
            .iter()
            .map(|(outcome, reason, turning, lesson, competitor, company)| {
                if outcome == "won" { counts.won += 1 } else { counts.lost += 1 }
                if !competitor.trim().is_empty() && !competitors.iter().any(|c| c.eq_ignore_ascii_case(competitor.trim())) {
                    competitors.push(competitor.trim().to_string());
                }
                let against = if competitor.trim().is_empty() { String::new() } else { format!(", against {}", competitor.trim()) };
                format!("- {outcome} {company} on {}{against}. Turning point: {turning} Lesson: {lesson}", reason.as_deref().unwrap_or_default().replace('_', " "))
            })
            .collect();
        parts.push(format!("Win and loss reviews from the last 12 months:\n{}", lines.join("\n")));
    }

    let cards: Vec<(String, Value)> = sqlx::query_as(
        "select coalesce(nullif(c.name, ''), c.domain::text), b.card from battlecards b join competitors c on c.id = b.competitor_id
         where b.project_id = $1 and b.card is not null order by b.written_at desc nulls last limit 6",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    for (name, card) in &cards {
        counts.battlecards += 1;
        if !competitors.iter().any(|c| c.eq_ignore_ascii_case(name)) {
            competitors.push(name.clone());
        }
        let wins = items(card, "wins").iter().map(|p| format!("- we win: {}", text(p, "point"))).collect::<Vec<_>>().join("\n");
        let objections = items(card, "objections").iter().map(|o| format!("- they say \"{}\", we say \"{}\"", text(o, "they_say"), text(o, "we_say"))).collect::<Vec<_>>().join("\n");
        parts.push(format!("Battlecard for {name}: {}\n{wins}\n{objections}", text(card, "summary")));
    }

    let insights: Vec<(String, Value)> = sqlx::query_as(
        "select distinct on (i.segment_id) s.name, i.insights from campaign_insights i join segments s on s.id = i.segment_id
         where i.project_id = $1 and i.insights is not null order by i.segment_id, i.created_at desc limit 6",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    for (name, i) in &insights {
        counts.campaigns += 1;
        let working = items(i, "working").iter().map(|w| format!("- working: {}", w.as_str().unwrap_or_default())).collect::<Vec<_>>().join("\n");
        let objections = items(i, "objections")
            .iter()
            .map(|o| format!("- objection in {} replies: {}. Answer: {}", o["count"], text(o, "theme"), text(o, "answer")))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("Replies to the {name} campaign: {}\n{working}\n{objections}", text(i, "summary")));
    }

    let coached: Vec<Value> = sqlx::query_scalar(
        "select coaching from call_coaching where project_id = $1 and coaching is not null and created_at > now() - interval '6 months'
         order by (coaching->>'score')::int desc, created_at desc limit 12",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    if !coached.is_empty() {
        counts.calls = coached.len() as i64;
        let lines: Vec<String> = coached
            .iter()
            .map(|c| {
                let strengths = items(c, "strengths").iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join("; ");
                let instead = items(c, "improve").iter().map(|i| text(i, "instead")).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("; ");
                format!("- {}/10. Did well: {strengths}. Better: {instead}", c["score"])
            })
            .collect();
        parts.push(format!("Coached sales calls, the best first:\n{}", lines.join("\n")));
    }

    let emails: Vec<(String, String)> = sqlx::query_as(
        "select o.subject, left(o.body, 700) from email_messages i
         join lateral (select subject, body from email_messages o where o.lead_id = i.lead_id and o.direction = 'outbound' and o.status = 'sent'
                       and o.sent_at < i.created_at order by o.sent_at desc limit 1) o on true
         where i.project_id = $1 and i.direction = 'inbound' and i.classification = 'hot' order by i.created_at desc limit 8",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    if !emails.is_empty() {
        counts.emails = emails.len() as i64;
        let lines: Vec<String> = emails.iter().enumerate().map(|(n, (subject, body))| format!("Email {}: {subject}\n{}", n + 1, body.trim())).collect();
        parts.push(format!("Emails that got a hot reply:\n{}", lines.join("\n\n")));
    }
    Ok(Material { counts, text: parts.join("\n\n"), competitors })
}

/// What the project has to write a playbook from now.
pub async fn available(db: &PgPool, project_id: Uuid) -> sqlx::Result<Counts> {
    Ok(gather(db, project_id).await?.counts)
}

/// Writes a playbook for the project now. Returns the run.
pub async fn request(db: &PgPool, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar("insert into playbooks (project_id) values ($1) returning id").bind(project_id).fetch_one(&mut *tx).await?;
    let run = super::enqueue(&mut *tx, kind::PLAYBOOK, Some(project_id), Some(org_id), json!({ "playbook_id": id })).await?;
    sqlx::query("update playbooks set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(run)
}

/// The project's latest playbook: `status` is ready, writing or failed, with the run's `error`.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Saved {
    pub id: Uuid,
    pub playbook: Option<Value>,
    pub sources: Value,
    pub created_at: Ts,
    pub status: String,
    pub error: Option<String>,
}

pub async fn latest(db: &PgPool, project_id: Uuid) -> sqlx::Result<Option<Saved>> {
    sqlx::query_as(
        "select b.id, b.playbook, b.sources, b.created_at,
                case when b.playbook is not null then 'ready' when r.status in ('queued', 'running') then 'writing' else 'failed' end as status,
                case when b.playbook is null then nullif(r.error, '') end as error
         from playbooks b left join agent_runs r on r.id = b.run_id where b.project_id = $1 order by b.created_at desc limit 1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Question {
    pub question: String,
    pub why: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Objection {
    pub they_say: String,
    pub we_say: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rival {
    pub name: String,
    pub win_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmailLesson {
    pub lesson: String,
    pub example: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Playbook {
    pub summary: String,
    pub fit: Vec<String>,
    pub avoid: Vec<String>,
    pub pitch: String,
    pub discovery: Vec<Question>,
    pub objections: Vec<Objection>,
    pub competitors: Vec<Rival>,
    pub emails: Vec<EmailLesson>,
    pub winning: Vec<String>,
    pub pitfalls: Vec<String>,
}

#[derive(Deserialize)]
struct Input {
    playbook_id: Uuid,
}

fn schema() -> Value {
    let text = json!({ "type": "string" });
    let list = json!({ "type": "array", "items": text });
    let pairs = |a: &str, b: &str| json!({ "type": "array", "items": { "type": "object", "properties": { a: text, b: text }, "required": [a, b] } });
    json!({
        "type": "object",
        "properties": {
            "summary": text, "fit": list, "avoid": list, "pitch": text,
            "discovery": pairs("question", "why"), "objections": pairs("they_say", "we_say"), "competitors": pairs("name", "win_by"),
            "emails": pairs("lesson", "example"), "winning": list, "pitfalls": list,
        },
        "required": ["summary", "fit", "avoid", "pitch", "discovery", "objections", "competitors", "emails", "winning", "pitfalls"]
    })
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let (project_id, written): (Uuid, bool) = sqlx::query_as("select project_id, playbook is not null from playbooks where id = $1 and project_id = $2")
        .bind(input.playbook_id)
        .bind(ctx.project_id()?)
        .fetch_optional(db)
        .await?
        .context("the playbook was deleted")?;
    if written {
        return Ok(Outcome::new("Already written"));
    }
    let material = gather(db, project_id).await?;
    anyhow::ensure!(
        material.counts.total() >= MIN_SOURCES,
        "There's not enough to write a playbook from yet: it needs at least {MIN_SOURCES} of win or loss reviews, battlecards, campaign insights, coached calls or hot replies"
    );
    let (_, profile) = load_project(ctx, project_id).await?;
    let prompt = format!(
        "What we sell: {name}. {description}\n\nWhat the team has learned:\n\n{material}\n\n\
         Write the playbook. Ground every line in the material above, and leave a list empty rather than guess.\n\
         1. summary: 2 sentences on who we win with and how.\n\
         2. fit: up to 4 signs a prospect is a good fit. avoid: up to 4 signs they aren't.\n\
         3. pitch: 2 to 3 sentences a rep can say on a first call, in the words that worked.\n\
         4. discovery: up to 8 questions to ask, each with why in under 15 words.\n\
         5. objections: up to 8 things buyers say and what to say back, in a sentence or two.\n\
         6. competitors: for up to 6 competitors named above, how to win against them, in under 30 words.\n\
         7. emails: up to 5 lessons on what gets replies, each with a short example line taken from the emails above.\n\
         8. winning: up to 6 things that turned deals into wins, from the turning points and lessons.\n\
         9. pitfalls: up to 5 things that lost deals.",
        name = profile.name,
        description = profile.description,
        material = material.text,
    );
    ctx.log(format!("reading {} things the team learned…", material.counts.total())).await?;
    let call = Call { timeout: Duration::from_secs(4 * 60), budget_usd: 0.6, ..Call::writing(WRITER, prompt, schema()) };
    let playbook = tidy(ctx.ask::<Playbook>(&call).await?, &material.competitors);
    sqlx::query("update playbooks set playbook = $2, sources = $3 where id = $1 and run_id = $4 and playbook is null")
        .bind(input.playbook_id)
        .bind(json!(playbook))
        .bind(json!(material.counts))
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    Ok(Outcome::new(format!("Wrote the playbook from {} things the team learned", material.counts.total())))
}

/// Trims each line, drops empty ones, keeps each list to its size, and keeps only competitors the material names.
fn tidy(p: Playbook, competitors: &[String]) -> Playbook {
    let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    let lines = |v: &[String], max: usize| v.iter().map(|s| clip(s, 240)).filter(|s| !s.is_empty()).take(max).collect::<Vec<_>>();
    Playbook {
        summary: clip(&p.summary, 600),
        fit: lines(&p.fit, 4),
        avoid: lines(&p.avoid, 4),
        pitch: clip(&p.pitch, 600),
        discovery: p.discovery.iter().filter(|q| !q.question.trim().is_empty()).take(8).map(|q| Question { question: clip(&q.question, 240), why: clip(&q.why, 160) }).collect(),
        objections: p.objections.iter().filter(|o| !o.they_say.trim().is_empty()).take(8).map(|o| Objection { they_say: clip(&o.they_say, 240), we_say: clip(&o.we_say, 400) }).collect(),
        competitors: p
            .competitors
            .iter()
            .filter_map(|r| competitors.iter().find(|c| c.eq_ignore_ascii_case(r.name.trim())).map(|name| Rival { name: name.clone(), win_by: clip(&r.win_by, 300) }))
            .take(6)
            .collect(),
        emails: p.emails.iter().filter(|e| !e.lesson.trim().is_empty()).take(5).map(|e| EmailLesson { lesson: clip(&e.lesson, 240), example: clip(&e.example, 300) }).collect(),
        winning: lines(&p.winning, 6),
        pitfalls: lines(&p.pitfalls, 5),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_playbook_reads_what_the_team_learned_and_names_only_competitors_it_met(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(available(&db, project).await.unwrap().total(), 0);

        sqlx::query("insert into buyer_profiles (project_id, profile, won) values ($1, $2, 3)")
            .bind(project)
            .bind(json!({ "summary": "Sales teams of 5 to 20 on HubSpot.", "campaign": "", "traits": [{ "trait": "HubSpot", "count": 3, "why": "Logging", "deal_ids": [] }], "avoid": [] }))
            .execute(&db)
            .await
            .unwrap();
        crate::deals::ensure_stages(&db, org).await.unwrap();
        let stage: Uuid = sqlx::query_scalar("select id from deal_stages where org_id = $1 and name = 'Proposal'").bind(org).fetch_one(&db).await.unwrap();
        for (company, outcome, reason, competitor, lesson) in
            [("Lumenfold", "won", "proof", "Gong", "A trial on their own calls won it."), ("Orbitly", "lost", "price", "", "We never met the CFO.")]
        {
            let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, company_name) values ($1, 'Ines Duarte', $2) returning id")
                .bind(project)
                .bind(company)
                .fetch_one(&db)
                .await
                .unwrap();
            let deal: Uuid = sqlx::query_scalar("insert into deals (org_id, project_id, lead_id, stage_id, status) values ($1, $2, $3, $4, $5) returning id")
                .bind(org)
                .bind(project)
                .bind(lead)
                .bind(stage)
                .bind(outcome)
                .fetch_one(&db)
                .await
                .unwrap();
            sqlx::query("insert into deal_reviews (project_id, deal_id, outcome, reason, turning_point, competitor, lesson) values ($1, $2, $3, $4, 'The demo.', $5, $6)")
                .bind(project)
                .bind(deal)
                .bind(outcome)
                .bind(reason)
                .bind(competitor)
                .bind(lesson)
                .execute(&db)
                .await
                .unwrap();
            if outcome == "won" {
                sqlx::query(
                    "insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, sent_at)
                     values ($1, $2, 'outbound', 'initial', 'Your German calls', 'Saw you hired three AEs in Munich.', 'sent', now() - interval '2 days')",
                )
                .bind(project)
                .bind(lead)
                .execute(&db)
                .await
                .unwrap();
                sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, classification) values ($1, $2, 'inbound', 'inbound', 'Re', 'Yes, show me.', 'received', 'hot')")
                    .bind(project)
                    .bind(lead)
                    .execute(&db)
                    .await
                    .unwrap();
            }
        }
        let rival: Uuid = sqlx::query_scalar("insert into competitors (project_id, name, domain) values ($1, 'Chorus', 'chorus.example') returning id")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into battlecards (project_id, competitor_id, card, written_at) values ($1, $2, $3, now())")
            .bind(project)
            .bind(rival)
            .bind(json!({ "summary": "Call recording for big teams.", "pricing": "", "strengths": [], "wins": [{ "point": "Setup in a day", "evidence": "" }],
                          "losses": [], "landmines": [], "objections": [{ "they_say": "Chorus is cheaper", "we_say": "Count the admin hours." }], "quotes": [] }))
            .execute(&db)
            .await
            .unwrap();
        let counts = available(&db, project).await.unwrap();
        assert_eq!(counts, Counts { won: 1, lost: 1, battlecards: 1, campaigns: 0, calls: 0, emails: 1, profile: true });

        let run = request(&db, project, org).await.unwrap();
        assert_eq!(latest(&db, project).await.unwrap().unwrap().status, "writing");
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where id = $1 returning *").bind(run).fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(run, |_| {
            json!({ "summary": "We win with HubSpot teams.", "fit": ["HubSpot", " ", "5 to 20 reps", "Hiring AEs", "German calls", "Fifth"], "avoid": [],
                    "pitch": "Fieldnote logs every call.", "discovery": [{ "question": "Who logs your calls today?", "why": "Finds the pain" }],
                    "objections": [{ "they_say": "Chorus is cheaper", "we_say": "Count the admin hours." }, { "they_say": "", "we_say": "x" }],
                    "competitors": [{ "name": "gong", "win_by": "Offer a trial on their own calls." }, { "name": "Chorus", "win_by": "Setup in a day." },
                                    { "name": "Salesloft", "win_by": "Never met them." }],
                    "emails": [{ "lesson": "Open with their hiring", "example": "Saw you hired three AEs in Munich." }],
                    "winning": ["A trial on their own calls"], "pitfalls": ["Never meeting the CFO"] })
        });
        run_and_finish(&ctx).await;
        let prompt = super::super::fake::prompts(run).first().cloned().unwrap();
        for part in [
            "Who buys from us: Sales teams of 5 to 20 on HubSpot.",
            "- won Lumenfold on proof, against Gong. Turning point: The demo. Lesson: A trial on their own calls won it.",
            "- lost Orbitly on price. Turning point",
            "Battlecard for Chorus: Call recording for big teams.",
            "- they say \"Chorus is cheaper\", we say \"Count the admin hours.\"",
            "Email 1: Your German calls\nSaw you hired three AEs in Munich.",
        ] {
            assert!(prompt.contains(part), "missing {part:?} in {prompt}");
        }

        let saved = latest(&db, project).await.unwrap().unwrap();
        assert_eq!(saved.status, "ready");
        assert_eq!(saved.sources["won"], 1);
        let p: Playbook = serde_json::from_value(saved.playbook.unwrap()).unwrap();
        assert_eq!(p.fit, vec!["HubSpot", "5 to 20 reps", "Hiring AEs", "German calls"]);
        assert_eq!(p.objections.len(), 1);
        let rivals: Vec<_> = p.competitors.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(rivals, vec!["Gong", "Chorus"], "only competitors the team met, named as the team wrote them");
    }

    async fn run_and_finish(ctx: &Ctx) {
        run(ctx).await.unwrap();
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(ctx.run.id).execute(ctx.db()).await.unwrap();
    }
}
