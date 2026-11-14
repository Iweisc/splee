//! Customer stories: from a won deal's window, the agent drafts a case study from the conversation, the deal's review and
//! what they bought, and an email asking the customer to look at it on a page of their own (/story/{token}). There they
//! fix the quote, add their numbers, choose whether their name goes on it and approve it. It then becomes a case study in
//! the project's Materials, which the agent's emails and replies draw on, and the project's chats hear about it.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::types::Json;
use uuid::Uuid;

use super::explore::load_project;
use super::referrals::with_link;
use super::sdk::Call;
use super::write::{WriterSettings, draft_in_thread};
use super::{Ctx, Outcome, briefs, kind, meetings};
use crate::error::{AppError, AppResult};
use crate::hooks::{self, ChatAlert};
use crate::models::{Lead, Ts};
use crate::proposals::Item;

const WRITER: &str = "You write customer case studies for a B2B company, for Splee, an outbound sales tool, like a careful \
marketing writer who checks every claim. Use only what you're given. Never invent facts, results, numbers, names or quotes \
they didn't say. Plain text only, no markdown.";

/// The case study. The sections never name the customer, so the anonymous version only swaps the title and who the quote
/// is from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Draft {
    pub title: String,
    pub anonymous_title: String,
    /// Who they are without their name, like "a 40-clinic physiotherapy group".
    pub anonymous_name: String,
    pub summary: String,
    pub challenge: String,
    pub solution: String,
    pub results: String,
    pub quote: String,
}

impl Draft {
    fn tidy(mut self) -> Self {
        let clip = |s: &mut String, n: usize| *s = s.trim().chars().take(n).collect();
        clip(&mut self.title, 160);
        clip(&mut self.anonymous_title, 160);
        clip(&mut self.anonymous_name, 120);
        clip(&mut self.summary, 600);
        clip(&mut self.challenge, 1500);
        clip(&mut self.solution, 1500);
        clip(&mut self.results, 2000);
        clip(&mut self.quote, 600);
        self
    }
}

/// A story as a won deal's window shows it. `status` is writing, skipped, failed, drafted (the email waits for approval),
/// written (the page is ready but the email was deleted or didn't go), sent, approved or declined.
pub const SELECT: &str = "select s.id, s.deal_id, l.full_name as lead_name, l.company_name,
        case when r.status in ('queued', 'running') then 'writing' when s.approved_at is not null then 'approved'
             when s.declined_at is not null then 'declined' when s.skipped <> '' then 'skipped' when s.story is null then 'failed'
             when e.status = 'draft' then 'drafted' when e.id is null or e.status in ('failed', 'cancelled') then 'written'
             else 'sent' end as status,
        s.story, s.skipped, s.token, s.email_id, e.subject as email_subject, e.body as email_body, s.views, s.last_viewed_at,
        s.approved_at, s.approved_name, s.approved_title, s.named, s.declined_at, s.material_id, s.created_at
     from customer_stories s join deals d on d.id = s.deal_id join leads l on l.id = d.lead_id
     left join agent_runs r on r.id = s.run_id left join email_messages e on e.id = s.email_id";

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Story {
    pub id: Uuid,
    pub deal_id: Uuid,
    pub lead_name: String,
    pub company_name: String,
    pub status: String,
    pub story: Option<Json<Draft>>,
    pub skipped: String,
    pub token: String,
    pub email_id: Option<Uuid>,
    pub email_subject: Option<String>,
    pub email_body: Option<String>,
    pub views: i32,
    pub last_viewed_at: Option<Ts>,
    pub approved_at: Option<Ts>,
    pub approved_name: String,
    pub approved_title: String,
    pub named: Option<bool>,
    pub declined_at: Option<Ts>,
    pub material_id: Option<Uuid>,
    pub created_at: Ts,
}

/// Has the agent write the story and its email now, replacing a draft nobody approved and keeping the page's link. One
/// being written is reused. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "insert into customer_stories (project_id, deal_id) values ($1, $2) on conflict (deal_id) do update set project_id = excluded.project_id returning id",
    )
    .bind(project_id)
    .bind(deal_id)
    .fetch_one(&mut *tx)
    .await?;
    let writing: Option<Uuid> =
        sqlx::query_scalar("select r.id from customer_stories s join agent_runs r on r.id = s.run_id where s.id = $1 and r.status in ('queued', 'running')")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(run) = writing {
        tx.commit().await?;
        return Ok(run);
    }
    sqlx::query("delete from email_messages e using customer_stories s where s.id = $1 and e.id = s.email_id and e.status = 'draft'")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update customer_stories set story = null, skipped = '', email_id = null, declined_at = null where id = $1").bind(id).execute(&mut *tx).await?;
    let run = super::enqueue(&mut *tx, kind::CUSTOMER_STORY, Some(project_id), Some(org_id), json!({ "story_id": id })).await?;
    sqlx::query("update customer_stories set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(run)
}

pub fn page_url(public_url: &str, token: &str) -> String {
    format!("{}/story/{token}", public_url.trim_end_matches('/'))
}

#[derive(Deserialize)]
struct Input {
    story_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct Row {
    deal_id: Uuid,
    token: String,
    status: String,
    closed_at: Option<Ts>,
    lead_id: Uuid,
    value_cents: i64,
    currency: String,
    notes: String,
    created_at: Ts,
}

#[derive(Deserialize)]
struct Answer {
    write: bool,
    why: String,
    story: Draft,
    subject: String,
    body: String,
}

fn schema() -> Value {
    let text = json!({ "type": "string" });
    let fields = ["title", "anonymous_name", "anonymous_title", "summary", "challenge", "solution", "results", "quote"];
    json!({
        "type": "object",
        "properties": {
            "write": { "type": "boolean" },
            "why": text,
            "story": { "type": "object", "properties": fields.iter().map(|f| (f.to_string(), text.clone())).collect::<serde_json::Map<_, _>>(), "required": fields },
            "subject": text,
            "body": text
        },
        "required": ["write", "why", "story", "subject", "body"]
    })
}

async fn skip(ctx: &Ctx, id: Uuid, why: &str) -> anyhow::Result<Outcome> {
    sqlx::query("update customer_stories set skipped = $2 where id = $1 and run_id = $3 and story is null")
        .bind(id)
        .bind(why)
        .bind(ctx.run.id)
        .execute(ctx.db())
        .await?;
    Ok(Outcome::new(format!("No case study: {why}")))
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let id = input.story_id;
    let row = sqlx::query_as::<_, Row>(
        "select s.deal_id, s.token, d.status, d.closed_at, d.lead_id, d.value_cents, d.currency, d.notes, d.created_at
         from customer_stories s join deals d on d.id = s.deal_id where s.id = $1 and s.project_id = $2",
    )
    .bind(id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the story was deleted")?;
    let Some(won_at) = row.closed_at.filter(|_| row.status == "won") else {
        return skip(ctx, id, "the deal is no longer won").await;
    };
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(row.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    if let Some(blocker) = meetings::follow_up_blocker(&mut *db.acquire().await?, org_id, &lead).await? {
        return skip(ctx, id, blocker).await;
    }

    let review: Option<(Option<String>, String, String)> =
        sqlx::query_as("select reason, story, turning_point from deal_reviews where deal_id = $1 and outcome = 'won' and created_at >= $2")
            .bind(row.deal_id)
            .bind(won_at)
            .fetch_optional(db)
            .await?;
    let bought: Option<Json<Vec<Item>>> =
        sqlx::query_scalar("select items from proposals where deal_id = $1 and accepted_at is not null").bind(row.deal_id).fetch_optional(db).await?;
    let settings = WriterSettings::load(ctx, lead.project_id).await?;
    let lang = crate::language::for_lead(db, lead.id).await?;
    let context = briefs::gather(db, &lead, org_id).await?;
    let us = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) };
    let company = if lead.company_name.trim().is_empty() { lead.full_name.trim() } else { lead.company_name.trim() };

    let mut deal = vec![format!(
        "They became a customer on {}{}, after {} days.",
        won_at.format("%-d %b %Y"),
        if row.value_cents > 0 { format!(", worth {} {}", row.value_cents / 100, row.currency.to_uppercase()) } else { String::new() },
        (won_at - row.created_at).num_days(),
    )];
    if !row.notes.trim().is_empty() {
        deal.push(format!("The deal's notes: {}", row.notes.trim()));
    }
    if let Some((reason, story, turning_point)) = &review {
        deal.push(format!("Why they bought: {}. {} {}", reason.as_deref().unwrap_or("").replace('_', " "), story.trim(), turning_point.trim()));
    }
    if let Some(Json(items)) = bought.filter(|b| !b.0.is_empty()) {
        let items = items.iter().map(|i| format!("{} x {}", i.quantity, i.name.trim())).collect::<Vec<_>>().join(", ");
        deal.push(format!("What they bought: {items}."));
    }
    let prompt = format!(
        "{who} is our customer.\n{deal}\n\nWhat we sell: {name}. {description}\n\n\
         The conversation and what else Splee knows:\n{context}\n\n\
         Write a case study about them, which they'll check and approve before anyone sees it.\n\
         1. write: false only when the conversation says too little about their problem or why they chose us to tell it.\n\
         2. why: under 25 words, what the story rests on, or why there's too little.\n\
         3. story:\n\
         - title: under 12 words, naming {company}, about what changed for them.\n\
         - anonymous_name: who they are without their name, from what you know, like \"a 40-clinic physiotherapy group in \
         Portugal\". Start it lowercase.\n\
         - anonymous_title: the title with anonymous_name in place of {company}.\n\
         - summary: two sentences.\n\
         - challenge: 40 to 80 words, the problem they had before us, in their own words where you can.\n\
         - solution: 40 to 80 words, why they chose {name} and what they started with.\n\
         - results: under 80 words, only what changed that the conversation shows. Never a number they didn't give, and no \
         note about adding numbers, since their page asks them. If it's too early to tell, say what they expected.\n\
         - quote: one or two first-person sentences they'd say, built from what they wrote to us. They'll edit it.\n\
         The sections and the quote never name {company} or anyone at it, since they may stay anonymous. Write \"they\" or \
         \"the team\".\n\
         4. subject: short. It goes in the existing thread when there is one.\n\
         5. body: a 50 to 100 word email from {us}. Thank them for something specific from the conversation, say we wrote up \
         how it went and would like to share it, and that on this page they can fix anything, add their numbers and choose \
         whether their name goes on it, written as {LINK} on a line of its own. Make it easy to say no. No sign-off.\n\
         {language} The story too.",
        who = [lead.full_name.trim(), lead.company_name.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" at "),
        deal = deal.join("\n"),
        name = profile.name,
        description = profile.description,
        LINK = super::referrals::LINK,
        language = crate::language::instruction(lang),
    );
    ctx.log(format!("writing up how it went for {company}…")).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.4, ..Call::writing(WRITER, prompt, schema()) };
    let answer: Answer = ctx.ask(&call).await?;
    let story = answer.story.tidy();
    if !answer.write || story.title.is_empty() || answer.body.trim().is_empty() {
        let why = if answer.why.trim().is_empty() { "the conversation says too little to tell their story" } else { answer.why.trim() };
        return skip(ctx, id, why).await;
    }
    let body = settings.sign(&with_link(&answer.body, &page_url(&ctx.state.cfg.public_url, &row.token)));

    let mut tx = db.begin().await?;
    let email_id = draft_in_thread(&mut tx, &lead, "story_ask", &answer.subject, &body, lang.code).await?;
    let saved = sqlx::query("update customer_stories set story = $2, email_id = $3 where id = $1 and run_id = $4 and story is null and approved_at is null")
        .bind(id)
        .bind(Json(&story))
        .bind(email_id)
        .bind(ctx.run.id)
        .execute(&mut *tx)
        .await?;
    if saved.rows_affected() == 0 {
        return Ok(Outcome::new("The case study is already written"));
    }
    tx.commit().await?;
    Ok(Outcome { message: format!("Drafted a case study about {company} and the email asking them to approve it"), output: json!({ "email_id": email_id }) })
}

/// The story with the customer's company swapped for who they are, when they stay anonymous. The writer is told to keep
/// the name out of the sections, and this catches it when it doesn't. The page's preview does the same.
fn hidden(s: &str, company: &str, named: bool, anonymous_name: &str) -> String {
    if named || company.trim().is_empty() { s.to_string() } else { s.replace(company.trim(), anonymous_name) }
}

/// The case study's title and text as Materials keeps it.
pub fn render(d: &Draft, company: &str, named: bool, name: &str, job: &str) -> (String, String) {
    let hide = |s: &str| hidden(s, company, named, &d.anonymous_name);
    let title = if named { d.title.clone() } else { hide(&d.anonymous_title) };
    let who = if named {
        [name, job, company].iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ")
    } else if job.trim().is_empty() {
        let mut c = d.anonymous_name.chars();
        c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
    } else {
        format!("{} at {}", job.trim(), d.anonymous_name)
    };
    let sections = [("", &d.summary), ("The challenge", &d.challenge), ("What they did", &d.solution), ("The results", &d.results)];
    let mut text: Vec<String> = sections
        .iter()
        .filter(|(_, s)| !s.trim().is_empty())
        .map(|(h, s)| if h.is_empty() { hide(s) } else { format!("{h}\n{}", hide(s)) })
        .collect();
    if !d.quote.trim().is_empty() {
        text.push(format!("\"{}\"\n{who}", hide(d.quote.trim())));
    }
    (title, text.join("\n\n"))
}

/// What the story's page shows the customer.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Public {
    pub first_name: String,
    pub full_name: String,
    pub job_title: String,
    pub company: String,
    pub company_domain: Option<String>,
    pub from_name: String,
    pub from_domain: Option<String>,
    pub story: Json<Draft>,
    /// waiting, approved or declined.
    pub status: String,
    pub named: Option<bool>,
    pub approved_name: String,
    pub approved_title: String,
}

pub async fn public(db: &PgPool, token: &str) -> sqlx::Result<Option<Public>> {
    sqlx::query_as::<_, Public>(
        "select l.first_name, l.full_name, l.title as job_title, l.company_name as company, l.company_domain::text as company_domain,
                coalesce(nullif(pr.company->>'name', ''), pr.name) as from_name, pr.domain::text as from_domain, s.story,
                case when s.approved_at is not null then 'approved' when s.declined_at is not null then 'declined' else 'waiting' end as status,
                s.named, s.approved_name, s.approved_title
         from customer_stories s join deals d on d.id = s.deal_id join leads l on l.id = d.lead_id join projects pr on pr.id = s.project_id
         where s.token = $1 and d.status = 'won' and s.story is not null",
    )
    .bind(token)
    .fetch_optional(db)
    .await
}

pub async fn viewed(db: &PgPool, token: &str) -> sqlx::Result<()> {
    sqlx::query("update customer_stories set views = views + 1, last_viewed_at = now() where token = $1 and story is not null").bind(token).execute(db).await?;
    Ok(())
}

/// What the customer sends when they approve: who the quote is from, whether their company is named, and the quote and
/// results as they fixed them.
#[derive(Debug, Deserialize)]
pub struct Approval {
    pub name: String,
    #[serde(default)]
    pub title: String,
    pub named: bool,
    pub quote: String,
    pub results: String,
}

#[derive(sqlx::FromRow)]
struct Open {
    id: Uuid,
    project_id: Uuid,
    story: Json<Draft>,
    company: String,
    approved: bool,
    declined: bool,
}

async fn open(conn: &mut sqlx::PgConnection, token: &str) -> AppResult<Open> {
    sqlx::query_as::<_, Open>(
        "select s.id, s.project_id, s.story, coalesce(nullif(trim(l.company_name), ''), l.full_name) as company,
                s.approved_at is not null as approved, s.declined_at is not null as declined
         from customer_stories s join deals d on d.id = s.deal_id join leads l on l.id = d.lead_id
         where s.token = $1 and d.status = 'won' and s.story is not null for update of s",
    )
    .bind(token)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound)
}

fn materials_url(public_url: &str, project_id: Uuid) -> String {
    format!("{}/app/p/{project_id}/materials", public_url.trim_end_matches('/'))
}

/// Saves the customer's fixes, adds the case study to the project's Materials and posts it to the project's chats. A
/// customer who said no first can still approve. Returns the material.
pub async fn approve(db: &PgPool, public_url: &str, token: &str, form: &Approval) -> AppResult<Uuid> {
    let mut tx = db.begin().await?;
    let s = open(&mut tx, token).await?;
    if s.approved {
        return Err(AppError::BadRequest("You already approved this".into()));
    }
    let name = form.name.trim();
    let job = form.title.trim();
    let story = Draft { quote: form.quote.clone(), results: form.results.clone(), ..s.story.0 }.tidy();
    let (title, content) = render(&story, &s.company, form.named, name, job);
    let material: Uuid = sqlx::query_scalar("insert into materials (project_id, kind, title, content) values ($1, 'case_study', $2, $3) returning id")
        .bind(s.project_id)
        .bind(&title)
        .bind(&content)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query(
        "update customer_stories set story = $2, approved_at = now(), approved_name = $3, approved_title = $4, named = $5, declined_at = null,
                material_id = $6 where id = $1",
    )
    .bind(s.id)
    .bind(Json(&story))
    .bind(name)
    .bind(job)
    .bind(form.named)
    .bind(material)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let how = if form.named { "with their name on it" } else { "to share without their name" };
    let summary = format!("{name} approved the {} case study, {how}. It's in Materials now, for the agent to use as proof.", s.company);
    let link = materials_url(public_url, s.project_id);
    let alert = ChatAlert { label: "Case study approved", summary: &summary, quote: (!story.quote.is_empty()).then_some(story.quote.as_str()), link: &link, link_text: "Open Materials" };
    hooks::post_chat(db, s.project_id, &alert).await;
    Ok(material)
}

/// Records that the customer would rather not, and tells the project's chats once.
pub async fn decline(db: &PgPool, public_url: &str, token: &str) -> AppResult<()> {
    let mut tx = db.begin().await?;
    let s = open(&mut tx, token).await?;
    if s.approved {
        return Err(AppError::BadRequest("You already approved this. Write to us to take it down".into()));
    }
    if s.declined {
        return Ok(());
    }
    sqlx::query("update customer_stories set declined_at = now() where id = $1").bind(s.id).execute(&mut *tx).await?;
    tx.commit().await?;
    let summary = format!("{} would rather not have a case study. Nothing was shared.", s.company);
    let link = materials_url(public_url, s.project_id);
    hooks::post_chat(db, s.project_id, &ChatAlert { label: "Case study declined", summary: &summary, quote: None, link: &link, link_text: "Open Materials" }).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    fn draft() -> Draft {
        Draft {
            title: "How Lumenfold cut onboarding to a week".into(),
            anonymous_title: "How a design studio cut onboarding to a week".into(),
            anonymous_name: "a design studio in Lisbon".into(),
            summary: "They onboard new clients in a week.".into(),
            challenge: "Lumenfold spent a month on every client.".into(),
            solution: "They started with the templates.".into(),
            results: String::new(),
            quote: "It just works.".into(),
        }
    }

    #[test]
    fn the_anonymous_version_names_neither_the_company_nor_the_person() {
        let (title, text) = render(&draft(), "Lumenfold", false, "Sam Ito", "");
        assert_eq!(title, "How a design studio cut onboarding to a week");
        assert!(!text.contains("Lumenfold") && !text.contains("Sam") && !text.contains("The results"), "{text}");
        assert!(text.contains("a design studio in Lisbon spent a month") && text.ends_with("\"It just works.\"\nA design studio in Lisbon"), "{text}");
        let (_, text) = render(&draft(), "Lumenfold", false, "Sam Ito", "COO");
        assert!(text.ends_with("COO at a design studio in Lisbon"), "{text}");

        let (title, text) = render(&draft(), "Lumenfold", true, "Sam Ito", "COO");
        assert_eq!(title, "How Lumenfold cut onboarding to a week");
        assert!(text.contains("The challenge\nLumenfold spent") && text.ends_with("Sam Ito, COO, Lumenfold"), "{text}");
    }

    async fn run_next(db: &PgPool, answer: Value) -> String {
        let started = sqlx::query_as::<_, AgentRun>(
            "update agent_runs set status = 'running' where id = (select id from agent_runs where kind = 'customer_story' and status = 'queued' limit 1) returning *",
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

    async fn current(db: &PgPool, deal: Uuid) -> Story {
        sqlx::query_as::<_, Story>(&format!("{SELECT} where s.deal_id = $1")).bind(deal).fetch_one(db).await.unwrap()
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_won_customer_approves_their_story_anonymously_and_it_becomes_a_case_study(db: PgPool) {
        let t = crate::agent::practice::fixture::team(&db).await;
        sqlx::query("update deals set status = 'won', closed_at = now() - interval '30 days' where id = $1").bind(t.deal).execute(&db).await.unwrap();
        sqlx::query("update leads set email = 'ines@lumenfold.example'").execute(&db).await.unwrap();
        let answer = |write: bool| {
            json!({ "write": write, "why": "They said onboarding went from a month to a week.", "story": draft(),
                    "subject": "Your story", "body": "Hi Ines, thanks again.\n{link}\nNo is fine." })
        };

        request(&db, t.deal, t.project, t.org).await.unwrap();
        let prompt = run_next(&db, answer(false)).await;
        assert!(prompt.contains("Ines Duarte at Lumenfold is our customer") && prompt.contains("never name Lumenfold"), "{prompt}");
        let s = current(&db, t.deal).await;
        assert_eq!((s.status.as_str(), s.skipped.as_str()), ("skipped", "They said onboarding went from a month to a week."));
        assert!(public(&db, &s.token).await.unwrap().is_none(), "no page without a story");

        request(&db, t.deal, t.project, t.org).await.unwrap();
        run_next(&db, answer(true)).await;
        let s = current(&db, t.deal).await;
        assert_eq!(s.status, "drafted");
        let body = s.email_body.unwrap();
        assert!(body.contains(&format!("/story/{}\nNo is fine", s.token)), "{body}");
        let kind: String = sqlx::query_scalar("select kind from email_messages where id = $1").bind(s.email_id).fetch_one(&db).await.unwrap();
        assert_eq!(kind, "story_ask");

        // Written again, the unapproved draft is replaced and the page keeps its link.
        request(&db, t.deal, t.project, t.org).await.unwrap();
        run_next(&db, answer(true)).await;
        let again = current(&db, t.deal).await;
        assert_eq!((again.status.as_str(), again.token.as_str()), ("drafted", s.token.as_str()));
        let drafts: i64 = sqlx::query_scalar("select count(*) from email_messages where kind = 'story_ask'").fetch_one(&db).await.unwrap();
        assert_eq!(drafts, 1);

        let page = public(&db, &s.token).await.unwrap().unwrap();
        assert_eq!((page.full_name.as_str(), page.company.as_str(), page.status.as_str()), ("Ines Duarte", "Lumenfold", "waiting"));
        decline(&db, "https://app.example", &s.token).await.unwrap();
        assert_eq!(current(&db, t.deal).await.status, "declined");

        let form = Approval { name: " Ines Duarte ".into(), title: "COO".into(), named: false, quote: " Onboarding takes a week now. ".into(), results: "From 30 days to 7.".into() };
        let material = approve(&db, "https://app.example", &s.token, &form).await.unwrap();
        let (kind, title, content): (String, String, String) =
            sqlx::query_as("select kind, title, content from materials where id = $1").bind(material).fetch_one(&db).await.unwrap();
        assert_eq!((kind.as_str(), title.as_str()), ("case_study", "How a design studio cut onboarding to a week"));
        assert!(content.contains("The results\nFrom 30 days to 7.") && content.ends_with("\"Onboarding takes a week now.\"\nCOO at a design studio in Lisbon"), "{content}");
        assert!(!content.contains("Lumenfold") && !content.contains("Ines"), "{content}");
        let s = current(&db, t.deal).await;
        assert_eq!((s.status.as_str(), s.named, s.approved_name.as_str(), s.material_id), ("approved", Some(false), "Ines Duarte", Some(material)));
        assert!(approve(&db, "https://app.example", &s.token, &form).await.is_err(), "once");
        assert!(decline(&db, "https://app.example", &s.token).await.is_err(), "nor taken back from the page");
        assert!(matches!(approve(&db, "https://app.example", "nope", &form).await, Err(AppError::NotFound)));
    }
}
