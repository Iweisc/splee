//! Campaign insights: the agent reads a campaign's latest replies and says what gets people to answer, which objections come
//! up and how often, who answers, and how to change the campaign's pain points or angle. Counts come from the replies the
//! model cites, not its estimate, and a quote it can't point to in a reply is dropped.

use std::collections::HashSet;
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

/// Fewer replies than this don't show a pattern.
pub const MIN_REPLIES: i64 = 5;
/// The newest replies it reads.
const MAX_REPLIES: i64 = 150;
const READER: &str = "You read the replies to a cold email campaign for Splee, an outbound sales tool, and tell its team what \
the replies say. Use only the replies and emails given. Never invent quotes or numbers. Plain text only, no markdown.";

/// What counts as an answer: everything but bounces and out-of-office notes.
const REPLIES: &str = "direction = 'inbound' and coalesce(classification, '') not in ('bounce', 'ooo')";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Objection {
    pub theme: String,
    pub count: usize,
    /// Their words, from one of the replies, or empty.
    pub quote: String,
    /// A short answer the team can send.
    pub answer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Suggestion {
    /// "pain_points" or "angle".
    pub field: String,
    /// What it says now, kept to show the change.
    pub current: String,
    pub value: String,
    pub why: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Insights {
    pub summary: String,
    pub working: Vec<String>,
    pub objections: Vec<Objection>,
    pub who: Vec<String>,
    pub suggestions: Vec<Suggestion>,
}

#[derive(Deserialize)]
struct Answer {
    summary: String,
    working: Vec<String>,
    objections: Vec<AnswerObjection>,
    who: Vec<String>,
    suggestions: Vec<AnswerSuggestion>,
}

#[derive(Deserialize)]
struct AnswerObjection {
    theme: String,
    reply_ids: Vec<String>,
    quote: String,
    answer: String,
}

#[derive(Deserialize)]
struct AnswerSuggestion {
    field: String,
    value: String,
    why: String,
}

fn schema() -> Value {
    let list = json!({ "type": "array", "items": { "type": "string" } });
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "working": list,
            "objections": { "type": "array", "items": { "type": "object", "properties": {
                "theme": { "type": "string" }, "reply_ids": list, "quote": { "type": "string" }, "answer": { "type": "string" }
            }, "required": ["theme", "reply_ids", "quote", "answer"] } },
            "who": list,
            "suggestions": { "type": "array", "items": { "type": "object", "properties": {
                "field": { "type": "string", "enum": ["pain_points", "angle"] }, "value": { "type": "string" }, "why": { "type": "string" }
            }, "required": ["field", "value", "why"] } },
        },
        "required": ["summary", "working", "objections", "who", "suggestions"]
    })
}

/// How many of the campaign's replies the agent could read.
pub async fn reply_count(db: &PgPool, segment_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar(&format!("select count(*) from email_messages where segment_id = $1 and {REPLIES}")).bind(segment_id).fetch_one(db).await
}

/// The campaign's latest insights: `status` is ready, writing or failed.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Saved {
    pub id: Uuid,
    pub insights: Option<Value>,
    pub replies: i32,
    pub applied: Value,
    pub created_at: Ts,
    pub status: String,
}

pub async fn latest(db: &PgPool, segment_id: Uuid) -> sqlx::Result<Option<Saved>> {
    sqlx::query_as(
        "select i.id, i.insights, i.replies, i.applied, i.created_at,
                case when i.insights is not null then 'ready' when r.status in ('queued', 'running') then 'writing' else 'failed' end as status
         from campaign_insights i left join agent_runs r on r.id = i.run_id
         where i.segment_id = $1 order by i.created_at desc limit 1",
    )
    .bind(segment_id)
    .fetch_optional(db)
    .await
}

pub async fn request(db: &PgPool, project_id: Uuid, org_id: Uuid, segment_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar("insert into campaign_insights (project_id, segment_id) values ($1, $2) returning id")
        .bind(project_id)
        .bind(segment_id)
        .fetch_one(&mut *tx)
        .await?;
    let run = super::enqueue(&mut *tx, kind::CAMPAIGN_INSIGHTS, Some(project_id), Some(org_id), json!({ "insights_id": id })).await?;
    sqlx::query("update campaign_insights set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(id)
}

/// Puts suggestion `index` of `insights_id` into its campaign. False when it's gone, already used or the campaign changed
/// that field since, so a click never overwrites someone's edit.
pub async fn apply(db: &PgPool, segment_id: Uuid, insights_id: Uuid, index: usize) -> anyhow::Result<bool> {
    let mut tx = db.begin().await?;
    let row: Option<(Option<Value>, Value)> = sqlx::query_as("select insights, applied from campaign_insights where id = $1 and segment_id = $2 for update")
        .bind(insights_id)
        .bind(segment_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some((Some(insights), applied)) = row else { return Ok(false) };
    let insights: Insights = serde_json::from_value(insights)?;
    let Some(s) = insights.suggestions.get(index) else { return Ok(false) };
    if applied.as_array().is_some_and(|a| a.iter().any(|i| i.as_u64() == Some(index as u64))) {
        return Ok(false);
    }
    // The column name comes from this match, never from the stored text.
    let column = match s.field.as_str() {
        "pain_points" => "pain_points",
        "angle" => "angle",
        _ => return Ok(false),
    };
    let updated = sqlx::query(&format!("update segments set {column} = $2 where id = $1 and {column} = $3"))
        .bind(segment_id)
        .bind(&s.value)
        .bind(&s.current)
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() == 0 {
        return Ok(false);
    }
    sqlx::query("update campaign_insights set applied = applied || to_jsonb($2::int) where id = $1").bind(insights_id).bind(index as i32).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(true)
}

#[derive(Deserialize)]
struct Input {
    insights_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct Reply {
    classification: Option<String>,
    body: String,
    title: String,
    company: String,
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let (segment_id, done): (Uuid, bool) = sqlx::query_as("select segment_id, insights is not null from campaign_insights where id = $1 and project_id = $2")
        .bind(input.insights_id)
        .bind(ctx.project_id()?)
        .fetch_optional(db)
        .await?
        .context("the insights were deleted")?;
    if done {
        return Ok(Outcome::new("Already written"));
    }
    let (name, pain_points, angle): (String, String, String) =
        sqlx::query_as("select name, pain_points, angle from segments where id = $1").bind(segment_id).fetch_one(db).await?;
    let replies: Vec<Reply> = sqlx::query_as(&format!(
        "select e.classification, e.body, l.title, l.company_name as company from email_messages e join leads l on l.id = e.lead_id
         where e.segment_id = $1 and e.{REPLIES} order by e.created_at desc limit $2"
    ))
    .bind(segment_id)
    .bind(MAX_REPLIES)
    .fetch_all(db)
    .await?;
    anyhow::ensure!(replies.len() as i64 >= MIN_REPLIES, "the campaign needs at least {MIN_REPLIES} replies");
    let bodies: Vec<String> = replies.iter().map(|r| crate::mail::inbound::strip_quoted(&r.body).chars().take(700).collect::<String>().trim().to_string()).collect();
    let sent: Vec<(String, String)> = sqlx::query_as(
        "select subject, left(body, 1200) from email_messages where segment_id = $1 and direction = 'outbound' and kind = 'initial' and status = 'sent'
         order by sent_at desc limit 3",
    )
    .bind(segment_id)
    .fetch_all(db)
    .await?;
    let (_, profile) = load_project(ctx, ctx.project_id()?).await?;

    let listed = replies
        .iter()
        .zip(&bodies)
        .enumerate()
        .map(|(i, (r, body))| format!("[r{}] {} | {} at {}\n{body}", i + 1, r.classification.as_deref().unwrap_or("unread"), r.title, r.company))
        .collect::<Vec<_>>()
        .join("\n\n");
    let examples = sent.iter().map(|(subject, body)| format!("Subject: {subject}\n{body}")).collect::<Vec<_>>().join("\n---\n");
    let prompt = format!(
        "{us} runs the campaign \"{name}\". Its pain points: {pain_points}\nIts angle: {angle}\n\n\
         Some first emails it sent:\n{examples}\n\n\
         The {n} newest replies, each with its id, how Splee sorted it (hot, question, not_interested, unsubscribe, other), and who wrote it:\n{listed}\n\n\
         1. summary: what the replies say about the campaign, in 2 or 3 sentences.\n\
         2. working: up to 4 things in the emails or the offer that the interested replies respond to.\n\
         3. objections: up to 5 reasons people give for not being interested or for hesitating, most common first. For each, the ids \
         of every reply that gives it, one short quote copied exactly from one of those replies, and a 1 to 3 sentence answer the \
         team could send, in {us}'s voice.\n\
         4. who: up to 3 patterns in who answers with interest, like roles or kinds of company. Empty if there's no pattern.\n\
         5. suggestions: up to 2 changes to the campaign's pain points or angle that the replies argue for. Give the whole new \
         text for the field and why, citing what the replies said. Empty if the replies don't argue for a change.\n\
         Name companies, not reply ids: the ids go in reply_ids only.",
        us = profile.name,
        examples = if examples.is_empty() { "(none)".to_string() } else { examples },
        n = replies.len(),
    );
    ctx.log(format!("reading {} replies…", replies.len())).await?;
    let call = Call { timeout: Duration::from_secs(4 * 60), budget_usd: 0.6, ..Call::writing(READER, prompt, schema()) };
    let answer: Answer = ctx.ask(&call).await?;
    let insights = tidy(answer, &bodies, &pain_points, &angle);

    sqlx::query("update campaign_insights set insights = $2, replies = $3 where id = $1 and insights is null")
        .bind(input.insights_id)
        .bind(json!(insights))
        .bind(replies.len() as i32)
        .execute(db)
        .await?;
    Ok(Outcome { message: format!("Read {} replies", replies.len()), output: json!({ "insights_id": input.insights_id }) })
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Counts each objection by the distinct replies it cites that exist, drops ones with none, keeps a quote only when it's in
/// a reply, and keeps suggestions that change a field.
fn tidy(a: Answer, bodies: &[String], pain_points: &str, angle: &str) -> Insights {
    let list = |v: Vec<String>, max: usize| v.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).take(max).collect();
    let squashed: Vec<String> = bodies.iter().map(|b| squash(b)).collect();
    let mut objections: Vec<Objection> = a
        .objections
        .into_iter()
        .filter_map(|o| {
            let ids: HashSet<usize> = o
                .reply_ids
                .iter()
                .filter_map(|id| id.trim().trim_start_matches(['r', 'R']).parse::<usize>().ok())
                .filter(|&i| i >= 1 && i <= bodies.len())
                .collect();
            let quote = o.quote.trim().trim_matches('"').trim().to_string();
            let found = !quote.is_empty() && squashed.iter().any(|b| b.contains(&squash(&quote)));
            (!ids.is_empty() && !o.theme.trim().is_empty()).then(|| Objection {
                theme: o.theme.trim().to_string(),
                count: ids.len(),
                quote: if found { quote } else { String::new() },
                answer: o.answer.trim().to_string(),
            })
        })
        .collect();
    objections.sort_by_key(|o| std::cmp::Reverse(o.count));
    objections.truncate(5);
    let suggestions = a
        .suggestions
        .into_iter()
        .filter_map(|s| {
            let current = match s.field.as_str() {
                "pain_points" => pain_points,
                "angle" => angle,
                _ => return None,
            };
            let value = s.value.trim().to_string();
            (!value.is_empty() && value != current.trim()).then(|| Suggestion { field: s.field, current: current.to_string(), value, why: s.why.trim().to_string() })
        })
        .take(2)
        .collect();
    Insights { summary: a.summary.trim().to_string(), working: list(a.working, 4), objections, who: list(a.who, 3), suggestions }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    async fn setup(db: &PgPool, replies: &[(&str, &str)]) -> (Uuid, Uuid, Uuid) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(db)
            .await
            .unwrap();
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name, pain_points, angle) values ($1, 'Clinics', 'Missed calls', 'Never miss a patient') returning id")
            .bind(project)
            .fetch_one(db)
            .await
            .unwrap();
        for (i, (classification, body)) in replies.iter().enumerate() {
            let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, segment_id, full_name, title, company_name) values ($1, $2, $3, 'Owner', 'Smile Dental') returning id")
                .bind(project)
                .bind(segment)
                .bind(format!("Lead {i}"))
                .fetch_one(db)
                .await
                .unwrap();
            sqlx::query("insert into email_messages (project_id, lead_id, segment_id, direction, kind, body, status, classification) values ($1, $2, $3, 'inbound', 'inbound', $4, 'received', $5)")
                .bind(project)
                .bind(lead)
                .bind(segment)
                .bind(body)
                .bind(classification)
                .execute(db)
                .await
                .unwrap();
        }
        (org, project, segment)
    }

    const REPLIES_5: [(&str, &str); 7] = [
        ("not_interested", "We already use Weave for this.\n\nOn Tue, Jo wrote:\n> Hi"),
        ("not_interested", "Happy with Weave, thanks."),
        ("hot", "Sounds good, can we talk Thursday?"),
        ("question", "How much is it for 3 locations?"),
        ("not_interested", "No budget this year."),
        ("bounce", "Delivery failed"),
        ("ooo", "Out until Monday"),
    ];

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn insights_count_objections_by_the_replies_they_cite_and_apply_once(db: PgPool) {
        let (org, project, segment) = setup(&db, &REPLIES_5).await;
        assert_eq!(reply_count(&db, segment).await.unwrap(), 5, "bounces and out-of-office notes aren't replies");
        let id = request(&db, project, org, segment).await.unwrap();
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'campaign_insights' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |_| {
            json!({
                "summary": " Most no's already use a competitor. ", "working": ["The missed-call number", " "],
                "objections": [
                    { "theme": "Already use Weave", "reply_ids": ["r4", "R5", "r5", "r99"], "quote": "\"happy with  Weave\"", "answer": "We work alongside Weave." },
                    { "theme": "No budget", "reply_ids": ["r1"], "quote": "We have zero budget", "answer": "It pays for itself." },
                    { "theme": "Made up", "reply_ids": [], "quote": "", "answer": "" }
                ],
                "who": ["Owners of small practices"],
                "suggestions": [
                    { "field": "angle", "value": "Works with the phone system you already have", "why": "Two replies said they use Weave." },
                    { "field": "angle", "value": "Never miss a patient", "why": "No change" },
                    { "field": "name", "value": "x", "why": "not a field" }
                ]
            })
        });
        run(&ctx).await.unwrap();
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("[r5] not_interested | Owner at Smile Dental\nWe already use Weave for this.") && !prompt.contains("> Hi"), "newest first, without the quoted thread: {prompt}");

        let saved = latest(&db, segment).await.unwrap().unwrap();
        assert_eq!((saved.status.as_str(), saved.replies), ("ready", 5));
        let got: Insights = serde_json::from_value(saved.insights.unwrap()).unwrap();
        assert_eq!(got.working, vec!["The missed-call number"]);
        assert_eq!(got.objections.len(), 2, "an objection no reply gives is dropped");
        assert_eq!((got.objections[0].count, got.objections[0].quote.as_str()), (2, "happy with  Weave"), "counted by distinct real replies");
        assert_eq!(got.objections[1].quote, "", "a quote that isn't in any reply is dropped");
        assert_eq!(got.suggestions.len(), 1, "only real changes to real fields");

        assert!(apply(&db, segment, id, 0).await.unwrap());
        let angle: String = sqlx::query_scalar("select angle from segments where id = $1").bind(segment).fetch_one(&db).await.unwrap();
        assert_eq!(angle, "Works with the phone system you already have");
        assert!(!apply(&db, segment, id, 0).await.unwrap(), "used once");
        assert!(!apply(&db, segment, id, 7).await.unwrap(), "no such suggestion");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_suggestion_never_overwrites_a_field_someone_edited_since(db: PgPool) {
        let (org, project, segment) = setup(&db, &REPLIES_5).await;
        let id = request(&db, project, org, segment).await.unwrap();
        let insights = Insights {
            summary: String::new(),
            working: vec![],
            objections: vec![],
            who: vec![],
            suggestions: vec![Suggestion { field: "angle".into(), current: "Never miss a patient".into(), value: "New".into(), why: String::new() }],
        };
        sqlx::query("update campaign_insights set insights = $2 where id = $1").bind(id).bind(json!(insights)).execute(&db).await.unwrap();
        sqlx::query("update segments set angle = 'Edited by hand' where id = $1").bind(segment).execute(&db).await.unwrap();
        assert!(!apply(&db, segment, id, 0).await.unwrap());
        let angle: String = sqlx::query_scalar("select angle from segments where id = $1").bind(segment).fetch_one(&db).await.unwrap();
        assert_eq!(angle, "Edited by hand");
    }
}
