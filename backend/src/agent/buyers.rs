//! Buyer profiles: the agent reads a project's won deals, each with its lead, company and why it was won, next to its lost
//! deals, and says what the companies that buy have in common and what the ones that don't share. Each trait counts the won
//! (or lost) deals it cites that exist, so a trait no deal backs is dropped. It also drafts a campaign to find more companies
//! like the buyers, which the Win/loss tab creates through the usual campaign-from-a-description run.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::write::{company_signals, lead_brief};
use super::{Ctx, Outcome, kind};
use crate::models::{Lead, Ts};

/// Fewer won deals than this don't show a pattern.
pub const MIN_WON: i64 = 3;
const MAX_WON: i64 = 25;
const MAX_LOST: i64 = 15;
const PROFILER: &str = "You find who buys from a B2B company using Splee, an outbound sales tool, by comparing its won deals \
with its lost ones, like a sharp revenue analyst. Use only the deals given. A trait counts only when the deals you cite show it.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Trait {
    pub r#trait: String,
    pub count: usize,
    pub why: String,
    pub deal_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub summary: String,
    /// What the won deals share.
    pub traits: Vec<Trait>,
    /// What the lost deals share that the won ones don't.
    pub avoid: Vec<Trait>,
    /// A description of the companies to look for, for a new campaign.
    pub campaign: String,
}

/// Won deals the profile can read, all time.
pub async fn won_count(db: &PgPool, project_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar("select count(*) from deals where project_id = $1 and status = 'won'").bind(project_id).fetch_one(db).await
}

/// The project's latest profile: `status` is ready, writing or failed.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Saved {
    pub id: Uuid,
    pub profile: Option<Value>,
    pub won: i32,
    pub created_at: Ts,
    pub status: String,
}

pub async fn latest(db: &PgPool, project_id: Uuid) -> sqlx::Result<Option<Saved>> {
    sqlx::query_as(
        "select b.id, b.profile, b.won, b.created_at,
                case when b.profile is not null then 'ready' when r.status in ('queued', 'running') then 'writing' else 'failed' end as status
         from buyer_profiles b left join agent_runs r on r.id = b.run_id
         where b.project_id = $1 order by b.created_at desc limit 1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
}

/// Returns the run.
pub async fn request(db: &PgPool, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar("insert into buyer_profiles (project_id) values ($1) returning id").bind(project_id).fetch_one(&mut *tx).await?;
    let run = super::enqueue(&mut *tx, kind::BUYER_PROFILE, Some(project_id), Some(org_id), json!({ "profile_id": id })).await?;
    sqlx::query("update buyer_profiles set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(run)
}

#[derive(Deserialize)]
struct Input {
    profile_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct Closed {
    id: Uuid,
    lead_id: Uuid,
    value_cents: i64,
    currency: String,
    days: i32,
    campaign: Option<String>,
    reason: Option<String>,
    story: Option<String>,
    lost_reason: String,
}

#[derive(Deserialize)]
struct Answer {
    summary: String,
    traits: Vec<AnswerTrait>,
    avoid: Vec<AnswerTrait>,
    campaign: String,
}

#[derive(Deserialize)]
struct AnswerTrait {
    r#trait: String,
    deal_ids: Vec<String>,
    why: String,
}

fn schema() -> Value {
    let traits = json!({ "type": "array", "items": { "type": "object", "properties": {
        "trait": { "type": "string" }, "deal_ids": { "type": "array", "items": { "type": "string" } }, "why": { "type": "string" }
    }, "required": ["trait", "deal_ids", "why"] } });
    json!({
        "type": "object",
        "properties": { "summary": { "type": "string" }, "traits": traits, "avoid": traits, "campaign": { "type": "string" } },
        "required": ["summary", "traits", "avoid", "campaign"]
    })
}

async fn closed(db: &PgPool, project_id: Uuid, status: &str, limit: i64) -> sqlx::Result<Vec<Closed>> {
    sqlx::query_as(
        "select d.id, d.lead_id, d.value_cents, d.currency, extract(day from coalesce(d.closed_at, now()) - d.created_at)::int as days,
                s.name as campaign, r.reason, r.story, d.lost_reason
         from deals d join leads l on l.id = d.lead_id left join segments s on s.id = l.segment_id
         left join deal_reviews r on r.deal_id = d.id and r.outcome = d.status
         where d.project_id = $1 and d.status = $2 order by d.closed_at desc nulls last limit $3",
    )
    .bind(project_id)
    .bind(status)
    .bind(limit)
    .fetch_all(db)
    .await
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let (project_id, written): (Uuid, bool) = sqlx::query_as("select project_id, profile is not null from buyer_profiles where id = $1 and project_id = $2")
        .bind(input.profile_id)
        .bind(ctx.project_id()?)
        .fetch_optional(db)
        .await?
        .context("the profile was deleted")?;
    if written {
        return Ok(Outcome::new("Already written"));
    }
    let won = closed(db, project_id, "won", MAX_WON).await?;
    anyhow::ensure!(won.len() as i64 >= MIN_WON, "the project needs at least {MIN_WON} won deals");
    let lost = closed(db, project_id, "lost", MAX_LOST).await?;
    let ids: Vec<Uuid> = won.iter().chain(&lost).map(|d| d.lead_id).collect();
    let leads: Vec<Lead> = sqlx::query_as("select * from leads where id = any($1)").bind(&ids).fetch_all(db).await?;
    let refs: Vec<&Lead> = leads.iter().collect();
    let signals = company_signals(ctx, &refs).await?;
    let (project, profile) = load_project(ctx, project_id).await?;

    let describe = |prefix: char, i: usize, d: &Closed| -> anyhow::Result<String> {
        let lead = leads.iter().find(|l| l.id == d.lead_id).context("a deal's lead is gone")?;
        let mut about = lead_brief(lead, &signals);
        // Its id is ours, not the model's business.
        if let Some(o) = about.as_object_mut() {
            o.remove("lead_id");
        }
        let why = match (&d.reason, &d.story) {
            (Some(reason), Some(story)) if !story.is_empty() => format!(" Reason: {}. {story}", reason.replace('_', " ")),
            _ if !d.lost_reason.is_empty() => format!(" Reason given: {}.", d.lost_reason),
            _ => String::new(),
        };
        Ok(format!(
            "[{prefix}{}] {} {}, {} days from first deal to close{}.{why}\n{}",
            i + 1,
            crate::deals::amount(d.value_cents),
            d.currency.to_uppercase(),
            d.days,
            d.campaign.as_deref().map(|c| format!(", from the campaign \"{c}\"")).unwrap_or_default(),
            serde_json::to_string(&about)?,
        ))
    };
    let won_text = won.iter().enumerate().map(|(i, d)| describe('w', i, d)).collect::<anyhow::Result<Vec<_>>>()?.join("\n\n");
    let lost_text = lost.iter().enumerate().map(|(i, d)| describe('l', i, d)).collect::<anyhow::Result<Vec<_>>>()?.join("\n\n");
    let prompt = format!(
        "{brief}\n\nIts won deals, each with an id, the deal and the lead and company:\n{won_text}\n\n\
         Its lost deals:\n{lost}\n\n\
         1. summary: who buys from them, in 2 sentences, compared with who doesn't.\n\
         2. traits: up to 5 things the companies or people in the won deals have in common, like the kind of company, its \
         size, the buyer's role, a moment they were in or why they bought. For each, the ids of every won deal (w1, w2…) that \
         shows it and why it matters, in under 20 words.\n\
         3. avoid: up to 3 things the lost deals share that the won ones don't, with the ids of every lost deal (l1, l2…) that \
         shows it. Empty if there's no pattern.\n\
         4. campaign: 2 or 3 sentences describing the companies to look for next and the people to reach there, built from the \
         traits, written as instructions for finding them.\n\
         Name companies, not deal ids: the ids go in deal_ids only.",
        brief = profile.brief(project.domain.as_deref()),
        lost = if lost_text.is_empty() { "None yet.".to_string() } else { lost_text },
    );
    ctx.log(format!("comparing {} won deals with {} lost…", won.len(), lost.len())).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.5, ..Call::writing(PROFILER, prompt, schema()) };
    let found = tidy(ctx.ask::<Answer>(&call).await?, &won, &lost);

    sqlx::query("update buyer_profiles set profile = $2, won = $3 where id = $1 and run_id = $4 and profile is null")
        .bind(input.profile_id)
        .bind(json!(found))
        .bind(won.len() as i32)
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    Ok(Outcome::new(format!("Found what {} won deals have in common", won.len())))
}

/// Counts each trait by the distinct deals it cites from its own list, drops ones with none, and keeps the most backed.
fn tidy(a: Answer, won: &[Closed], lost: &[Closed]) -> Profile {
    let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    let count = |traits: Vec<AnswerTrait>, prefix: char, deals: &[Closed], max: usize| {
        let mut out: Vec<Trait> = traits
            .into_iter()
            .filter_map(|t| {
                let mut seen = HashSet::new();
                let ids: Vec<Uuid> = t
                    .deal_ids
                    .iter()
                    .filter_map(|id| id.trim().strip_prefix([prefix, prefix.to_ascii_uppercase()])?.parse::<usize>().ok())
                    .filter(|&i| i >= 1 && i <= deals.len())
                    .map(|i| deals[i - 1].id)
                    .filter(|id| seen.insert(*id))
                    .collect();
                (!ids.is_empty() && !t.r#trait.trim().is_empty()).then(|| Trait { r#trait: clip(&t.r#trait, 120), count: ids.len(), why: clip(&t.why, 200), deal_ids: ids })
            })
            .collect();
        out.sort_by_key(|t| std::cmp::Reverse(t.count));
        out.truncate(max);
        out
    };
    Profile { summary: clip(&a.summary, 500), traits: count(a.traits, 'w', won, 5), avoid: count(a.avoid, 'l', lost, 3), campaign: clip(&a.campaign, 800) }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn traits_count_the_deals_they_cite_from_their_own_list(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        crate::deals::ensure_stages(&db, org).await.unwrap();
        let stage = |kind: &'static str| {
            let db = db.clone();
            async move { sqlx::query_scalar::<_, Uuid>("select id from deal_stages where org_id = $1 and kind = $2 limit 1").bind(org).bind(kind).fetch_one(&db).await.unwrap() }
        };
        let (won_stage, lost_stage) = (stage("won").await, stage("lost").await);
        let mut deals = vec![];
        for (i, status) in ["won", "won", "won", "lost"].into_iter().enumerate() {
            let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, title, company_name) values ($1, $2, 'Head of Sales', $3) returning id")
                .bind(project)
                .bind(format!("Lead {i}"))
                .bind(format!("Company {i}"))
                .fetch_one(&db)
                .await
                .unwrap();
            let deal: Uuid = sqlx::query_scalar(
                "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, closed_at) values ($1, $2, $3, $4, $5, 500000, now() - make_interval(days => $6)) returning id",
            )
            .bind(org)
            .bind(project)
            .bind(lead)
            .bind(if status == "won" { won_stage } else { lost_stage })
            .bind(status)
            .bind(i as i32)
            .fetch_one(&db)
            .await
            .unwrap();
            deals.push(deal);
        }
        assert_eq!(won_count(&db, project).await.unwrap(), 3);
        request(&db, project, org).await.unwrap();
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'buyer_profile' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |_| {
            json!({
                "summary": " Sales leaders at small teams buy. ",
                "traits": [
                    { "trait": "First AEs", "deal_ids": ["w3"], "why": "Hiring" },
                    { "trait": "Sales leaders", "deal_ids": ["w1", "W2", "w2", "l1", "w9"], "why": "They feel the pain" },
                    { "trait": "Made up", "deal_ids": [], "why": "" }
                ],
                "avoid": [{ "trait": "Enterprise", "deal_ids": ["l1", "w1"], "why": "Gong already" }],
                "campaign": "Find B2B software companies hiring their first AEs."
            })
        });
        run(&ctx).await.unwrap();
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("[w1] 5,000 USD") && prompt.contains("[l1]") && !prompt.contains("lead_id"), "{prompt}");

        let saved = latest(&db, project).await.unwrap().unwrap();
        assert_eq!((saved.status.as_str(), saved.won), ("ready", 3));
        let p: Profile = serde_json::from_value(saved.profile.unwrap()).unwrap();
        assert_eq!(p.traits.len(), 2, "a trait no deal backs is dropped");
        assert_eq!((p.traits[0].r#trait.as_str(), p.traits[0].count), ("Sales leaders", 2), "counted by distinct won deals, most backed first");
        assert_eq!(p.traits[0].deal_ids, vec![deals[0], deals[1]], "w1 is the newest win");
        assert_eq!((p.avoid.len(), p.avoid[0].count, p.avoid[0].deal_ids[0]), (1, 1, deals[3]), "a lost trait counts lost deals only");
        assert_eq!(p.summary, "Sales leaders at small teams buy.");
    }
}
