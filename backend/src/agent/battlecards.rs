//! Battlecards: for a competitor, the agent reads their website, the deals we won or lost against them (agent/reviews.rs)
//! and what buyers said about them in emails and call transcripts, and writes how to sell against them: who they are,
//! their pricing, where they're strong, where we win and lose with evidence, questions that bring out our strengths,
//! answers to what buyers say for them, and buyers' own words, kept only when they're in the excerpts. Competitors we
//! won or lost a deal against get a card by themselves, and a newer review rewrites it after a week. Others get one when
//! asked.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::write::materials;
use super::{Ctx, Outcome, kind};
use crate::models::Ts;
use crate::state::AppState;

const PER_PASS: i64 = 10;
const MAX_EXCERPTS: usize = 10;
/// Characters of context kept on each side of a mention.
const AROUND: usize = 220;

const WRITER: &str = "You write battlecards for a B2B sales team using Splee, an outbound sales tool: short, honest notes on \
how to sell against one competitor. Use only what you read on the web and what you're given. Never invent customers, numbers \
or quotes, and say what the competitor genuinely does well.";

/// How a review names a competitor: as tracked, by name or else domain. See reviews::competitor.
pub const TRACKED_AS: &str = "coalesce(nullif(c.name, ''), c.domain::text)";

/// Each competitor of a project ($1) with its record and card. `status` is writing, ready or failed, or null before a card
/// was asked for.
pub const ITEMS: &str = "select c.id as competitor_id, c.name, c.domain::text as domain,
        (select count(*) from deal_reviews rv join deals d on d.id = rv.deal_id
         where rv.project_id = c.project_id and rv.outcome = 'won' and d.status = 'won'
           and lower(rv.competitor) = lower(coalesce(nullif(c.name, ''), c.domain::text))) as won,
        (select count(*) from deal_reviews rv join deals d on d.id = rv.deal_id
         where rv.project_id = c.project_id and rv.outcome = 'lost' and d.status = 'lost'
           and lower(rv.competitor) = lower(coalesce(nullif(c.name, ''), c.domain::text))) as lost,
        b.card, b.written_at,
        case when r.status in ('queued', 'running') then 'writing' when b.card is not null then 'ready' when b.id is not null then 'failed' end as status
     from competitors c left join battlecards b on b.competitor_id = c.id left join agent_runs r on r.id = b.run_id
     where c.project_id = $1 order by c.created_at";

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Item {
    pub competitor_id: Uuid,
    pub name: String,
    pub domain: String,
    pub won: i64,
    pub lost: i64,
    pub card: Option<Value>,
    pub written_at: Option<Ts>,
    pub status: Option<String>,
}

/// The competitors a deal is up against, with their cards: the one its review names, and those its notes, lost reason or
/// the buyer's emails and calls mention.
pub async fn up_against(db: &PgPool, deal_id: Uuid, project_id: Uuid) -> sqlx::Result<Vec<Item>> {
    let reviewed: Vec<Uuid> = sqlx::query_scalar(&format!(
        "select c.id from competitors c join deal_reviews rv on rv.deal_id = $1 and lower(rv.competitor) = lower({TRACKED_AS}) where c.project_id = $2"
    ))
    .bind(deal_id)
    .bind(project_id)
    .fetch_all(db)
    .await?;
    let said: Vec<String> = sqlx::query_scalar(
        "(select concat_ws(E'\\n', notes, lost_reason) from deals where id = $1)
         union all (select e.body from email_messages e join deals d on d.lead_id = e.lead_id
                    where d.id = $1 and e.direction = 'inbound' order by e.created_at desc limit 30)
         union all (select m.transcript from meetings m join deals d on d.lead_id = m.lead_id where d.id = $1 order by m.started_at desc limit 10)
         union all (select c.transcript from calls c join deals d on d.lead_id = c.lead_id
                    where d.id = $1 and c.transcript <> '' order by c.created_at desc limit 10)",
    )
    .bind(deal_id)
    .fetch_all(db)
    .await?;
    let text = said.join("\n");
    let items = sqlx::query_as::<_, Item>(ITEMS).bind(project_id).fetch_all(db).await?;
    Ok(items.into_iter().filter(|i| reviewed.contains(&i.competitor_id) || find(&text, &words(&i.name, &i.domain)).is_some()).collect())
}

/// The words a competitor goes by, lowercased: its name and its domain's first label.
pub fn words(name: &str, domain: &str) -> Vec<String> {
    let mut words = vec![];
    for w in [name.trim().to_ascii_lowercase(), domain.split('.').next().unwrap_or_default().trim().to_ascii_lowercase()] {
        if w.len() > 1 && !words.contains(&w) {
            words.push(w);
        }
    }
    words
}

/// The byte offset of the first whole-word mention of any of `words` in `text`, ignoring ASCII case.
pub fn find(text: &str, words: &[String]) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    words
        .iter()
        .filter_map(|w| {
            lower.match_indices(w.as_str()).map(|(i, _)| i).find(|&i| {
                let before = lower[..i].chars().next_back();
                let after = lower[i + w.len()..].chars().next();
                !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
            })
        })
        .min()
}

/// Queues a card for each competitor a review names that has none, or whose card is a week old and a newer review names
/// them, in projects that keep battlecards on. Returns how many.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(&format!(
        "with due as (
           insert into battlecards (project_id, competitor_id)
           select c.project_id, c.id from competitors c
           join projects p on p.id = c.project_id and p.org_id is not null
           join project_settings s on s.project_id = c.project_id and s.write_battlecards
           left join battlecards b on b.competitor_id = c.id left join agent_runs r on r.id = b.run_id
           where coalesce(r.status, '') not in ('queued', 'running')
             and exists (select 1 from deal_reviews rv where rv.project_id = c.project_id and lower(rv.competitor) = lower({TRACKED_AS})
                           and (b.id is null or (rv.created_at > b.asked_at and b.asked_at < now() - interval '7 days')))
           limit $1
           on conflict (competitor_id) do update set asked_at = now()
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id"
    ))
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
    let run = super::enqueue(&mut *tx, kind::BATTLECARD, Some(project_id), Some(org_id), json!({ "battlecard_id": id })).await?;
    sqlx::query("update battlecards set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

/// Writes the competitor's card now, keeping the old one on show until the new one lands. One already being written is
/// reused. Returns the run.
pub async fn request(db: &PgPool, competitor_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "insert into battlecards (project_id, competitor_id) values ($1, $2) on conflict (competitor_id) do update set asked_at = now() returning id",
    )
    .bind(project_id)
    .bind(competitor_id)
    .fetch_one(&mut *tx)
    .await?;
    let writing: Option<Uuid> =
        sqlx::query_scalar("select r.id from battlecards b join agent_runs r on r.id = b.run_id where b.id = $1 and r.status in ('queued', 'running')")
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

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        crate::mail::every(Duration::from_secs(3600), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "battlecards").await
    });
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Point {
    pub point: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Objection {
    pub they_say: String,
    pub we_say: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Quote {
    pub quote: String,
    pub who: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Card {
    pub summary: String,
    pub pricing: String,
    pub strengths: Vec<String>,
    pub wins: Vec<Point>,
    pub losses: Vec<Point>,
    pub landmines: Vec<String>,
    pub objections: Vec<Objection>,
    pub quotes: Vec<Quote>,
}

#[derive(Deserialize)]
struct AnswerQuote {
    quote: String,
    source: i64,
}

#[derive(Deserialize)]
struct Answer {
    summary: String,
    pricing: String,
    strengths: Vec<String>,
    wins: Vec<Point>,
    losses: Vec<Point>,
    landmines: Vec<String>,
    objections: Vec<Objection>,
    quotes: Vec<AnswerQuote>,
}

fn schema() -> Value {
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    let points = json!({ "type": "array", "items": { "type": "object", "properties": { "point": { "type": "string" }, "evidence": { "type": "string" } }, "required": ["point", "evidence"] } });
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "pricing": { "type": "string" },
            "strengths": strings,
            "wins": points,
            "losses": points,
            "landmines": strings,
            "objections": { "type": "array", "items": { "type": "object", "properties": { "they_say": { "type": "string" }, "we_say": { "type": "string" } }, "required": ["they_say", "we_say"] } },
            "quotes": { "type": "array", "items": { "type": "object", "properties": { "quote": { "type": "string" }, "source": { "type": "integer" } }, "required": ["quote", "source"] } }
        },
        "required": ["summary", "pricing", "strengths", "wins", "losses", "landmines", "objections", "quotes"]
    })
}

/// A buyer's words around a mention of the competitor, with who said them and where.
#[derive(Debug, Clone, PartialEq)]
pub struct Excerpt {
    pub who: String,
    pub text: String,
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text around the first whole-word mention, or None without one.
fn excerpt(text: &str, words: &[String]) -> Option<String> {
    let at = find(text, words)?;
    let mut start = at.saturating_sub(AROUND);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (at + AROUND).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    let cut = |b: bool| if b { "…" } else { "" };
    Some(format!("{}{}{}", cut(start > 0), squash(&text[start..end]), cut(end < text.len())))
}

/// Buyers' emails and call transcripts in the project from the last year that mention the competitor, newest first.
async fn excerpts(db: &PgPool, project_id: Uuid, words: &[String]) -> sqlx::Result<Vec<Excerpt>> {
    let patterns: Vec<String> = words.iter().map(|w| format!("%{}%", w.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))).collect();
    let rows: Vec<(String, String, String, String, Ts)> = sqlx::query_as(
        "select l.full_name, l.company_name, 'an email', e.body, coalesce(e.sent_at, e.created_at) as at from email_messages e join leads l on l.id = e.lead_id
         where e.project_id = $1 and e.direction = 'inbound' and e.created_at > now() - interval '365 days' and e.body ilike any($2)
         union all
         select l.full_name, l.company_name, 'a meeting', m.transcript, m.started_at from meetings m join leads l on l.id = m.lead_id
         where m.project_id = $1 and m.started_at > now() - interval '365 days' and m.transcript ilike any($2)
         union all
         select l.full_name, l.company_name, 'a call', k.transcript, coalesce(k.called_at, k.created_at) from calls k join leads l on l.id = k.lead_id
         where k.project_id = $1 and k.created_at > now() - interval '365 days' and k.transcript ilike any($2)
         order by at desc limit 100",
    )
    .bind(project_id)
    .bind(&patterns)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(name, company, kind, text, at)| {
            let who = if company.trim().is_empty() { name } else { format!("{name}, {company}") };
            excerpt(&text, words).map(|text| Excerpt { who: format!("{who}, in {kind} on {}", at.format("%-d %b %Y")), text })
        })
        .take(MAX_EXCERPTS)
        .collect())
}

fn tidy(a: Answer, excerpts: &[Excerpt]) -> Card {
    let clip = |s: &str, n: usize| -> String { s.trim().chars().take(n).collect() };
    let list = |v: Vec<String>, max: usize| v.iter().map(|s| clip(s, 240)).filter(|s| !s.is_empty()).take(max).collect();
    let points = |v: Vec<Point>| {
        v.into_iter()
            .map(|p| Point { point: clip(&p.point, 200), evidence: clip(&p.evidence, 240) })
            .filter(|p| !p.point.is_empty() && !p.evidence.is_empty())
            .take(3)
            .collect()
    };
    let norm = |s: &str| squash(&s.to_lowercase()).trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    let quotes = a
        .quotes
        .into_iter()
        .filter_map(|q| {
            let source = excerpts.get(usize::try_from(q.source).ok()?.checked_sub(1)?)?;
            let quote = clip(&q.quote, 300);
            (!norm(&quote).is_empty() && norm(&source.text).contains(&norm(&quote))).then(|| Quote { quote, who: source.who.clone() })
        })
        .take(3)
        .collect();
    Card {
        summary: clip(&a.summary, 400),
        pricing: clip(&a.pricing, 400),
        strengths: list(a.strengths, 3),
        wins: points(a.wins),
        losses: points(a.losses),
        landmines: list(a.landmines, 3),
        objections: a
            .objections
            .into_iter()
            .map(|o| Objection { they_say: clip(&o.they_say, 200), we_say: clip(&o.we_say, 400) })
            .filter(|o| !o.they_say.is_empty() && !o.we_say.is_empty())
            .take(4)
            .collect(),
        quotes,
    }
}

#[derive(Deserialize)]
struct Input {
    battlecard_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct Deal {
    outcome: String,
    company: String,
    value_cents: i64,
    currency: String,
    closed_at: Option<Ts>,
    reason: Option<String>,
    story: String,
    turning_point: String,
    lesson: String,
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let (project_id, name, domain, note): (Uuid, String, String, String) = sqlx::query_as(
        "select c.project_id, c.name, c.domain::text, c.note from battlecards b join competitors c on c.id = b.competitor_id where b.id = $1 and b.project_id = $2",
    )
    .bind(input.battlecard_id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the competitor was deleted")?;
    let (_, profile) = load_project(ctx, project_id).await?;
    let label = if name.trim().is_empty() { domain.clone() } else { name.trim().to_string() };
    let words = words(&name, &domain);

    let deals = sqlx::query_as::<_, Deal>(
        "select rv.outcome, coalesce(nullif(l.company_name, ''), l.full_name) as company, d.value_cents, d.currency, d.closed_at, rv.reason,
                rv.story, rv.turning_point, rv.lesson
         from deal_reviews rv join deals d on d.id = rv.deal_id and d.status = rv.outcome join leads l on l.id = d.lead_id
         where rv.project_id = $1 and lower(rv.competitor) = lower($2) order by d.closed_at desc nulls last limit 12",
    )
    .bind(project_id)
    .bind(&label)
    .fetch_all(db)
    .await?;
    let deals_text = if deals.is_empty() {
        "None yet.".to_string()
    } else {
        deals
            .iter()
            .map(|d| {
                let value = if d.value_cents > 0 { format!(", {} {}", d.value_cents / 100, d.currency.to_uppercase()) } else { String::new() };
                let when = d.closed_at.map(|t| format!(" on {}", t.format("%-d %b %Y"))).unwrap_or_default();
                format!(
                    "- We {} {}{value}{when}. Reason: {}. {} Turning point: {} Lesson: {}",
                    d.outcome,
                    d.company,
                    d.reason.as_deref().unwrap_or("not reviewed").replace('_', " "),
                    d.story.trim(),
                    d.turning_point.trim(),
                    d.lesson.trim()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let excerpts = excerpts(db, project_id, &words).await?;
    let excerpts_text = if excerpts.is_empty() {
        "None yet.".to_string()
    } else {
        excerpts.iter().enumerate().map(|(i, e)| format!("[{}] {}: \"{}\"", i + 1, e.who, e.text)).collect::<Vec<_>>().join("\n")
    };
    let materials = materials(ctx, project_id).await?;
    let prompt = format!(
        "Write a battlecard for selling {us} against {label} ({domain}).{note}\n\n\
         What we sell: {us}. {description}\nOur materials:\n{materials}\n\n\
         Deals we won or lost against them:\n{deals_text}\n\n\
         What buyers said that mentions them, as excerpts:\n{excerpts_text}\n\n\
         First read their website: the home page, pricing and one or two product pages, at most 6 pages.\n\
         1. summary: under 40 words, who they sell to and how they pitch themselves.\n\
         2. pricing: under 40 words, what their site says about price and plans. Empty if it isn't public.\n\
         3. strengths: up to 3 things they genuinely do well, each under 20 words.\n\
         4. wins: up to 3 places we beat them, each with point (under 20 words) and evidence (under 25 words: the deal, the \
         buyer's words or our material that shows it). Leave one out rather than give it no evidence.\n\
         5. losses: up to 3 places we lose to them, the same way, from the deals and what buyers said.\n\
         6. landmines: up to 3 questions to ask a buyer who is looking at them, each under 25 words, that bring out where \
         we're stronger without naming them.\n\
         7. objections: up to 4, each with they_say (what a buyer says for them, under 20 words) and we_say (our honest \
         answer, under 40 words).\n\
         8. quotes: up to 3 buyers' words about them, each with quote (copied exactly from one excerpt, under 30 words) and \
         source (the excerpt's number). Empty without excerpts.",
        us = profile.name,
        domain = domain,
        note = if note.trim().is_empty() { String::new() } else { format!(" Our note on them: {}", note.trim()) },
        description = profile.description,
        materials = if materials.is_empty() { "(none)".to_string() } else { materials },
    );
    ctx.log(format!("reading {label}'s website and our deals against them…")).await?;
    let call = Call { budget_usd: 0.6, timeout: Duration::from_secs(5 * 60), ..Call::research(WRITER, prompt, schema(), 14) };
    let answer: Answer = ctx.ask(&call).await?;
    let card = tidy(answer, &excerpts);
    anyhow::ensure!(!card.summary.is_empty(), "the agent wrote an empty battlecard");
    sqlx::query("update battlecards set card = $2, written_at = now() where id = $1 and run_id = $3")
        .bind(input.battlecard_id)
        .bind(json!(card))
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    Ok(Outcome::new(format!("Wrote the battlecard for {label}")))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    #[test]
    fn a_competitor_is_found_as_a_whole_word_by_name_or_domain() {
        let gong = words("Gong", "gong.io");
        assert_eq!(gong, vec!["gong"]);
        assert_eq!(find("We use GONG for the enterprise team.", &gong), Some(7));
        assert_eq!(find("Our gongs ring at noon.", &gong), None, "not part of another word");
        let close = words("Close CRM", "close.com");
        assert_eq!(close, vec!["close crm", "close"]);
        assert_eq!(find("They closed the round.", &close), None);
        assert_eq!(find("We're moving to Close next month.", &close), Some(16));
        let long = format!("{} We tried Gong. {}", "a ".repeat(200), "b ".repeat(200));
        let e = excerpt(&long, &gong).unwrap();
        assert!(e.starts_with('…') && e.ends_with('…') && e.contains("We tried Gong."), "{e}");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_competitor_we_lost_to_gets_one_card_with_real_quotes_only(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Ours') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Fieldnote', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(&db).await.unwrap();
        let gong: Uuid = sqlx::query_scalar("insert into competitors (project_id, domain, name) values ($1, 'gong.io', 'Gong') returning id")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into competitors (project_id, domain, name) values ($1, 'chorus.ai', 'Chorus')").bind(project).execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "no deal against either yet");

        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, first_name, company_name, email) values ($1, 'Elena Petrova', 'Elena', 'Orbitly', 'elena@orbitly.example') returning id")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Lost', 'lost', 9) returning id").bind(org).fetch_one(&db).await.unwrap();
        let deal: Uuid = sqlx::query_scalar(
            "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, closed_at) values ($1, $2, $3, $4, 'lost', 600000, now()) returning id",
        )
        .bind(org)
        .bind(project)
        .bind(lead)
        .bind(stage)
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query("insert into deal_reviews (project_id, deal_id, outcome, reason, story, competitor, lesson) values ($1, $2, 'lost', 'competitor', 'They kept Gong.', 'Gong', 'Ask who the tool covers.')")
            .bind(project)
            .bind(deal)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, subject, body, status) values ($1, $2, 'inbound', 'inbound', 'Re', 'Honestly Gong already covers the enterprise team, so this is not a priority.', 'received')")
            .bind(project)
            .bind(lead)
            .execute(&db)
            .await
            .unwrap();

        assert_eq!(queue_due(&db).await.unwrap(), 1, "only the competitor we lost to");
        assert_eq!(queue_due(&db).await.unwrap(), 0, "once");
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'battlecard' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |_| {
            json!({
                "summary": " Conversation intelligence for enterprise sales teams. ", "pricing": "", "strengths": ["Deep call analytics", " "],
                "wins": [{ "point": "Notes for new AEs", "evidence": "Elena's AEs aren't covered by Gong." }, { "point": "Cheaper", "evidence": " " }],
                "losses": [{ "point": "Already installed", "evidence": "Orbitly kept Gong in Nov 2026." }],
                "landmines": ["Who on your team does the tool not cover today?"],
                "objections": [{ "they_say": "We already have Gong", "we_say": "Fieldnote covers the reps Gong doesn't." }],
                "quotes": [
                    { "quote": "gong already covers the  enterprise team", "source": 1 },
                    { "quote": "Gong is too expensive", "source": 1 },
                    { "quote": "anything", "source": 7 }
                ]
            })
        });
        run(&ctx).await.unwrap();
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("We lost Orbitly, 6000 USD") && prompt.contains("[1] Elena Petrova, Orbitly, in an email on "), "{prompt}");
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(ctx.run.id).execute(&db).await.unwrap();

        let items = sqlx::query_as::<_, Item>(ITEMS).bind(project).fetch_all(&db).await.unwrap();
        let g = items.iter().find(|i| i.competitor_id == gong).unwrap();
        assert_eq!((g.won, g.lost, g.status.as_deref()), (0, 1, Some("ready")));
        let card: Card = serde_json::from_value(g.card.clone().unwrap()).unwrap();
        assert_eq!(card.summary, "Conversation intelligence for enterprise sales teams.");
        assert_eq!((card.strengths.len(), card.wins.len(), card.losses.len()), (1, 1, 1), "a point without evidence is dropped");
        assert_eq!(card.quotes.len(), 1, "only words that are in the excerpt");
        assert!(card.quotes[0].who.starts_with("Elena Petrova, Orbitly, in an email on "));
        assert_eq!(items.iter().find(|i| i.competitor_id != gong).unwrap().status, None);
    }
}
