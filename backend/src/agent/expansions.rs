//! Account expansion: once a customer has been with us a month, the agent watches their company. News about them, new
//! job openings, or someone there reading one of our pricing pages has it decide whether there's room to sell more, like
//! more seats, another team or a bigger plan, and if so draft the email to the person who bought, in the thread. The draft
//! waits on the Emails page for approval like a nudge, and the project's chats hear about it.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::nudges::NUDGEABLE;
use super::sdk::Call;
use super::write::{WriterSettings, company_signals, draft_in_thread, lead_brief, materials};
use super::{Ctx, Outcome, briefs, kind, meetings};
use crate::hooks::{self, ChatAlert};
use crate::models::{Lead, Ts};
use crate::state::AppState;

const PER_PASS: i64 = 20;
/// A customer this new is still settling in, and this long ago is gone quiet.
const AFTER_DAYS: i32 = 30;
const WITHIN_DAYS: i32 = 730;
/// Between two looks at one account, and how old a sign may be when it's first noticed.
const EVERY_DAYS: i32 = 30;
/// An email either way this recently means they're already talking, so an automatic one would cut in.
const TALKING_DAYS: i32 = 14;

const WRITER: &str = "You help a B2B sales team grow its existing customers, for Splee, an outbound sales tool, like a \
thoughtful account manager. Use only what you're given. Never invent facts, numbers, names, usage or quotes, never pressure, \
and never write \"just checking in\", \"circling back\" or \"following up\". Plain text only, no markdown.";

/// An expansion as the Pipeline's Expansion view and a won deal's window show it. `status` is writing, drafted, sent,
/// skipped or failed. One whose email was deleted, cancelled or failed counts as skipped, saying so.
pub const SELECT: &str = "select x.id, x.deal_id, d.lead_id, l.full_name as lead_name, l.company_name,
        nullif(u.name, '') as owner_name, d.value_cents, d.currency, d.closed_at as won_at, x.signal, x.why,
        case when x.skipped <> '' then x.skipped when e.status = 'failed' then 'The email failed to send'
             when e.status = 'cancelled' then 'The email was cancelled' when x.written_at is not null and e.id is null then 'The draft was deleted'
             else '' end as skipped,
        x.email_id, e.subject as email_subject, e.body as email_body, e.status as email_status,
        case when r.status in ('queued', 'running') then 'writing' when x.written_at is null then 'failed'
             when x.skipped <> '' or e.id is null or e.status in ('failed', 'cancelled') then 'skipped'
             when e.status = 'draft' then 'drafted' else 'sent' end as status,
        x.created_at
     from deal_expansions x join deals d on d.id = x.deal_id join leads l on l.id = d.lead_id
     left join users u on u.id = l.owner_id left join agent_runs r on r.id = x.run_id
     left join email_messages e on e.id = x.email_id";

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Expansion {
    pub id: Uuid,
    pub deal_id: Uuid,
    pub lead_id: Uuid,
    pub lead_name: String,
    pub company_name: String,
    pub owner_name: Option<String>,
    pub value_cents: i64,
    pub currency: String,
    pub won_at: Option<Ts>,
    pub signal: String,
    pub why: String,
    pub skipped: String,
    pub email_id: Option<Uuid>,
    pub email_subject: Option<String>,
    pub email_body: Option<String>,
    pub email_status: Option<String>,
    pub status: String,
    pub created_at: Ts,
}

/// What changed at a won deal's company, as up to three sentences, from `w` with `project_id`, `company_domain` and
/// `since`: the newest news, the job openings, and the newest read of a pricing page. Empty when nothing did.
const SIGNAL: &str = "concat_ws(' ',
    (select 'In the news: ' || cs.title || ' (' || to_char(cs.happened_on, 'FMDD Mon YYYY') || ').' from company_signals cs
     where cs.project_id = w.project_id and cs.domain = w.company_domain and cs.first_seen > w.since and cs.title <> ''
     order by cs.happened_on desc limit 1),
    (select 'Hiring for ' || count(*) || case when count(*) = 1 then ' role' else ' roles' end || ' posted since, like '
            || (array_agg(jo.title order by jo.first_seen desc))[1] || '.'
     from job_openings jo where jo.project_id = w.project_id and jo.domain = w.company_domain and jo.closed_at is null
       and jo.first_seen > w.since having count(*) > 0),
    (select 'Someone there read ' || h.path || ' on our website on ' || to_char(h.created_at, 'FMDD Mon') || '.'
     from visitor_hits h join visitor_sites vs on vs.project_id = h.project_id, unnest(vs.intent_paths) as i(path)
     where h.project_id = w.project_id and h.domain = w.company_domain and h.created_at > w.since
       and (h.path = i.path or starts_with(h.path, i.path || '/'))
     order by h.created_at desc limit 1))";

/// Looks at each customer whose company showed a sign of growth since the last look, in projects that keep this on.
/// Returns how many runs it queued.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(&format!(
        "with won as (
           select d.id, d.project_id, l.company_domain,
                  greatest(d.closed_at, now() - make_interval(days => $3),
                           (select max(x.created_at) from deal_expansions x where x.deal_id = d.id)) as since
           from deals d join leads l on l.id = d.lead_id
           join projects p on p.id = d.project_id and p.org_id is not null
           join project_settings s on s.project_id = d.project_id and s.grow_accounts
           where d.status = 'won' and d.closed_at < now() - make_interval(days => $1) and d.closed_at > now() - make_interval(days => $2)
             and l.company_domain is not null
             and not exists (select 1 from deal_expansions x where x.deal_id = d.id and x.created_at > now() - make_interval(days => $3))),
         signals as (
           select w.id, w.project_id, {SIGNAL} as signal
           from won w),
         due as (
           insert into deal_expansions (project_id, deal_id, signal)
           select project_id, id, signal from signals where signal <> '' limit $4
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id"
    ))
    .bind(AFTER_DAYS)
    .bind(WITHIN_DAYS)
    .bind(EVERY_DAYS)
    .bind(PER_PASS)
    .fetch_all(&mut *tx)
    .await?;
    for (id, project_id, org_id) in &due {
        start(&mut tx, *id, *project_id, *org_id, true).await?;
    }
    tx.commit().await?;
    Ok(due.len())
}

async fn start(tx: &mut sqlx::PgConnection, id: Uuid, project_id: Uuid, org_id: Uuid, auto: bool) -> sqlx::Result<Uuid> {
    let run = super::enqueue(&mut *tx, kind::EXPANSION, Some(project_id), Some(org_id), json!({ "expansion_id": id, "auto": auto })).await?;
    sqlx::query("update deal_expansions set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

/// Has the agent look at the won deal now, from its window, replacing a draft nobody approved. One being written is
/// reused. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    // Takes the deal's lock, so two clicks don't both start one.
    sqlx::query("select 1 from deals where id = $1 for update").bind(deal_id).execute(&mut *tx).await?;
    let writing: Option<Uuid> = sqlx::query_scalar(
        "select r.id from deal_expansions x join agent_runs r on r.id = x.run_id where x.deal_id = $1 and r.status in ('queued', 'running')",
    )
    .bind(deal_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(run) = writing {
        tx.commit().await?;
        return Ok(run);
    }
    sqlx::query("delete from email_messages e using deal_expansions x where x.deal_id = $1 and e.id = x.email_id and e.status = 'draft'")
        .bind(deal_id)
        .execute(&mut *tx)
        .await?;
    // Reads what changed at their company in the last 30 days, even what the last look read: Write another on a draft
    // comes here, and it needs the news the draft was written from.
    let id: Uuid = sqlx::query_scalar(&format!(
        "insert into deal_expansions (project_id, deal_id, signal)
         select w.project_id, w.id, {SIGNAL} from (
           select d.id, $1::uuid as project_id, l.company_domain, greatest(d.closed_at, now() - make_interval(days => $3)) as since
           from deals d join leads l on l.id = d.lead_id where d.id = $2) w
         returning id"
    ))
    .bind(project_id)
    .bind(deal_id)
    .bind(EVERY_DAYS)
    .fetch_one(&mut *tx)
    .await?;
    let run = start(&mut tx, id, project_id, org_id, false).await?;
    tx.commit().await?;
    Ok(run)
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        crate::mail::every(Duration::from_secs(3600), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "expansions").await
    });
}

#[derive(Deserialize)]
struct Input {
    expansion_id: Uuid,
    auto: bool,
}

#[derive(sqlx::FromRow)]
struct Row {
    deal_id: Uuid,
    signal: String,
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
    expand: bool,
    why: String,
    subject: String,
    body: String,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": { "expand": { "type": "boolean" }, "why": { "type": "string" }, "subject": { "type": "string" }, "body": { "type": "string" } },
        "required": ["expand", "why", "subject", "body"]
    })
}

/// Marks the look done without an email, saying why.
async fn skip(ctx: &Ctx, id: Uuid, why: &str) -> anyhow::Result<Outcome> {
    sqlx::query("update deal_expansions set skipped = $2, written_at = now() where id = $1 and run_id = $3 and written_at is null")
        .bind(id)
        .bind(why)
        .bind(ctx.run.id)
        .execute(ctx.db())
        .await?;
    Ok(Outcome::new(format!("No expansion: {why}")))
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let row = sqlx::query_as::<_, Row>(
        "select x.deal_id, x.signal, d.status, d.closed_at, d.lead_id, d.value_cents, d.currency, d.notes, d.created_at
         from deal_expansions x join deals d on d.id = x.deal_id where x.id = $1 and x.project_id = $2",
    )
    .bind(input.expansion_id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the expansion was deleted")?;
    let id = input.expansion_id;
    let Some(won_at) = row.closed_at.filter(|_| row.status == "won") else {
        return skip(ctx, id, "the deal is no longer won").await;
    };
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(row.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    if input.auto {
        let on: bool = sqlx::query_scalar("select coalesce((select grow_accounts from project_settings where project_id = $1), true)")
            .bind(lead.project_id)
            .fetch_one(db)
            .await?;
        if !on {
            return skip(ctx, id, "growing accounts was switched off").await;
        }
    }
    if let Some(blocker) = meetings::follow_up_blocker(&mut *db.acquire().await?, org_id, &lead).await? {
        return skip(ctx, id, blocker).await;
    }
    let (talking, waiting): (bool, bool) = sqlx::query_as(&format!(
        "select exists (select 1 from email_messages e where e.lead_id = l.id and coalesce(e.sent_at, e.created_at) > now() - make_interval(days => $2)
                          and (e.direction = 'inbound' or e.status = 'sent')),
                not ({NUDGEABLE})
         from leads l where l.id = $1"
    ))
    .bind(lead.id)
    .bind(TALKING_DAYS)
    .fetch_one(db)
    .await?;
    if waiting {
        return skip(ctx, id, "an email to them is already waiting to go out, or a call is booked").await;
    }
    if talking && input.auto {
        return skip(ctx, id, "you've written to each other in the last two weeks, so bring it up in the thread").await;
    }

    let review: Option<(Option<String>, String, String)> =
        sqlx::query_as("select reason, story, lesson from deal_reviews where deal_id = $1 and outcome = 'won' and created_at >= $2")
            .bind(row.deal_id)
            .bind(won_at)
            .fetch_optional(db)
            .await?;
    let settings = WriterSettings::load(ctx, lead.project_id).await?;
    let lang = crate::language::for_lead(db, lead.id).await?;
    let signals = company_signals(ctx, &[&lead]).await?;
    let about = lead_brief(&lead, &signals);
    let context = briefs::gather(db, &lead, org_id).await?;
    let materials = materials(ctx, lead.project_id).await?;
    let us = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) };
    let mut deal = vec![format!(
        "They became a customer on {}{}, after {} days.",
        won_at.format("%-d %b %Y"),
        if row.value_cents > 0 { format!(", worth {} {}", row.value_cents / 100, row.currency.to_uppercase()) } else { String::new() },
        (won_at - row.created_at).num_days(),
    )];
    if !row.notes.trim().is_empty() {
        deal.push(format!("The deal's notes: {}", row.notes.trim()));
    }
    if let Some((reason, story, lesson)) = &review {
        deal.push(format!("Why they bought: {}. {} {}", reason.as_deref().unwrap_or("").replace('_', " "), story.trim(), lesson.trim()));
    }
    let prompt = format!(
        "{who} is our customer.\n{deal}\n\n{signal}\n\n\
         What we sell: {name}. {description}\nOur materials:\n{materials}\n\n\
         The lead and their company now:\n{about}\n\nThe conversation and what else Splee knows:\n{context}\n\n\
         Decide whether there's a real chance to sell them more now: more seats as a team grows, another team or office, or a \
         bigger plan, from what they bought and what changed.\n\
         1. expand: true only if the evidence points to one. False when nothing changed that bears on what we sell, or the \
         conversation shows they're unhappy, cutting back or leaving.\n\
         2. why: under 25 words, the opening and its evidence, or why there isn't one.\n\
         3. subject: a short subject. It goes in the existing thread when there is one.\n\
         4. body: if expand, a 50 to 100 word email from {us} to them. Open with what changed at their company, in a sentence \
         that shows we noticed and are glad, connect it to how they use us, and offer one concrete next step, like adding the \
         new team or a short call to plan it. Never mention that they read our website or pages, or guess why: use that only to \
         pick the angle. No sign-off. {language} Empty if not expand.",
        who = [lead.full_name.trim(), lead.company_name.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" at "),
        deal = deal.join("\n"),
        signal = if row.signal.is_empty() {
            "Our team asked you to look for a chance to grow the account now.".to_string()
        } else {
            format!("What changed at their company lately: {}", row.signal)
        },
        name = profile.name,
        description = profile.description,
        materials = if materials.is_empty() { "(none)".to_string() } else { materials },
        about = serde_json::to_string_pretty(&about)?,
        language = crate::language::instruction(lang),
    );
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    ctx.log(format!("looking for room to grow {first}'s account…")).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.3, ..Call::writing(WRITER, prompt, schema()) };
    let answer: Answer = ctx.ask(&call).await?;
    if !answer.expand || answer.body.trim().is_empty() {
        let why = if answer.why.trim().is_empty() { "the agent found no opening" } else { answer.why.trim() };
        return skip(ctx, id, why).await;
    }

    let mut tx = db.begin().await?;
    let email_id = draft_in_thread(&mut tx, &lead, "expansion", &answer.subject, &settings.sign(answer.body.trim()), lang.code).await?;
    let saved = sqlx::query("update deal_expansions set email_id = $2, why = $3, written_at = now() where id = $1 and run_id = $4 and written_at is null")
        .bind(id)
        .bind(email_id)
        .bind(answer.why.trim())
        .bind(ctx.run.id)
        .execute(&mut *tx)
        .await?;
    if saved.rows_affected() == 0 {
        return Ok(Outcome::new("The expansion is already written"));
    }
    tx.commit().await?;

    if input.auto {
        let link = hooks::lead_url(&ctx.state.cfg.public_url, lead.project_id, lead.id);
        let summary = format!("{} may be ready to buy more. {} An email is waiting for your approval.", lead.full_name, answer.why.trim());
        let alert = ChatAlert { label: "Expansion", summary: &summary, quote: Some(answer.body.trim()), link: &link, link_text: "Review the email" };
        hooks::post_chat(db, lead.project_id, &alert).await;
    }
    Ok(Outcome { message: format!("Drafted an expansion email to {}", lead.full_name), output: json!({ "email_id": email_id }) })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    struct Setup {
        project: Uuid,
        lead: Uuid,
        deal: Uuid,
    }

    async fn setup(db: &PgPool, won_days_ago: i32) -> Setup {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(db)
            .await
            .unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, full_name, first_name, company_name, company_domain, email) values ($1, 'Ines Duarte', 'Ines', 'Lumenfold', 'lumenfold.example', 'ines@lumenfold.example') returning id",
        )
        .bind(project)
        .fetch_one(db)
        .await
        .unwrap();
        let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Won', 'won', 8) returning id")
            .bind(org)
            .fetch_one(db)
            .await
            .unwrap();
        let deal: Uuid = sqlx::query_scalar(
            "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, notes, closed_at, created_at)
             values ($1, $2, $3, $4, 'won', 480000, 'usd', '4 seats for the SDR team', now() - make_interval(days => $5), now() - interval '200 days') returning id",
        )
        .bind(org)
        .bind(project)
        .bind(lead)
        .bind(stage)
        .bind(won_days_ago)
        .fetch_one(db)
        .await
        .unwrap();
        Setup { project, lead, deal }
    }

    async fn run_next(db: &PgPool, answer: Value) -> (Outcome, String) {
        let started = sqlx::query_as::<_, AgentRun>(
            "update agent_runs set status = 'running' where id = (select id from agent_runs where kind = 'expansion' and status = 'queued' limit 1) returning *",
        )
        .fetch_one(db)
        .await
        .unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, move |_| answer.clone());
        let outcome = run(&ctx).await.unwrap();
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(ctx.run.id).execute(db).await.unwrap();
        (outcome, super::super::fake::prompts(ctx.run.id).first().cloned().unwrap_or_default())
    }

    async fn latest(db: &PgPool, deal: Uuid) -> Expansion {
        sqlx::query_as::<_, Expansion>(&format!("{SELECT} where x.deal_id = $1 order by x.created_at desc limit 1")).bind(deal).fetch_one(db).await.unwrap()
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn news_at_a_customer_drafts_one_email_in_the_thread_and_waits_a_month_before_looking_again(db: PgPool) {
        let s = setup(&db, 90).await;
        let fresh = setup(&db, 10).await;
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, to_email, sent_at) values ($1, $2, 'outbound', 'initial', 'Calls in HubSpot', 'Hi', 'sent', 'ines@lumenfold.example', now() - interval '120 days')",
        )
        .bind(s.project)
        .bind(s.lead)
        .execute(&db)
        .await
        .unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "nothing changed at their company");

        for project in [s.project, fresh.project] {
            sqlx::query("insert into company_signals (project_id, domain, kind, source, key, happened_on, title, url) values ($1, 'lumenfold.example', 'funding_post', 'feed', 'k', current_date, 'Lumenfold raises a $20M Series B', 'https://lumenfold.example/news')")
                .bind(project)
                .execute(&db)
                .await
                .unwrap();
        }
        assert_eq!(queue_due(&db).await.unwrap(), 1, "only the customer of more than a month");
        assert_eq!(queue_due(&db).await.unwrap(), 0, "once");
        let (_, prompt) = run_next(
            &db,
            json!({ "expand": true, "why": "Series B means a bigger sales team.", "subject": "Congrats", "body": "Hi Ines, congrats on the Series B." }),
        )
        .await;
        assert!(prompt.contains("What changed at their company lately: In the news: Lumenfold raises a $20M Series B") && prompt.contains("4 seats for the SDR team"), "{prompt}");
        let x = latest(&db, s.deal).await;
        assert_eq!((x.status.as_str(), x.why.as_str(), x.email_subject.as_deref()), ("drafted", "Series B means a bigger sales team.", Some("Re: Calls in HubSpot")));
        let kind: String = sqlx::query_scalar("select kind from email_messages where id = $1").bind(x.email_id).fetch_one(&db).await.unwrap();
        assert_eq!(kind, "expansion");

        sqlx::query("insert into job_openings (project_id, domain, provider, title, url) values ($1, 'lumenfold.example', 'ashby', 'Account Executive', 'https://jobs/1')")
            .bind(s.project)
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "a month between looks");
        // A month on, the news the last look read is older than it.
        sqlx::query("update deal_expansions set created_at = now() - interval '31 days'").execute(&db).await.unwrap();
        sqlx::query("update company_signals set first_seen = now() - interval '32 days'").execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 1, "a new opening after the last look");
        let signal: String = sqlx::query_scalar("select signal from deal_expansions where deal_id = $1 order by created_at desc limit 1").bind(s.deal).fetch_one(&db).await.unwrap();
        assert_eq!(signal, "Hiring for 1 role posted since, like Account Executive.", "the news was already read");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn nothing_is_drafted_without_an_opening_while_they_are_talking_or_when_switched_off(db: PgPool) {
        let s = setup(&db, 90).await;
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(s.project).fetch_one(&db).await.unwrap();

        request(&db, s.deal, s.project, org).await.unwrap();
        let (_, prompt) = run_next(&db, json!({ "expand": false, "why": "They cut the team last month.", "subject": "", "body": "" })).await;
        assert!(prompt.contains("Our team asked you to look"), "{prompt}");
        let x = latest(&db, s.deal).await;
        assert_eq!((x.status.as_str(), x.skipped.as_str()), ("skipped", "They cut the team last month."));

        // Asked by the team, a recent conversation doesn't hold it back. Found by the worker, it does.
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, created_at) values ($1, $2, 'inbound', 'inbound', 'Re', 'Thanks!', 'received', now() - interval '3 days')")
            .bind(s.project)
            .bind(s.lead)
            .execute(&db)
            .await
            .unwrap();
        let first = request(&db, s.deal, s.project, org).await.unwrap();
        assert_eq!(request(&db, s.deal, s.project, org).await.unwrap(), first, "one being written is reused");
        run_next(&db, json!({ "expand": true, "why": "Hiring.", "subject": "More seats", "body": "Hi Ines." })).await;
        assert_eq!(latest(&db, s.deal).await.status, "drafted");

        let id: Uuid = sqlx::query_scalar("insert into deal_expansions (project_id, deal_id, signal) values ($1, $2, 'Hiring.') returning id")
            .bind(s.project)
            .bind(s.deal)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("delete from email_messages where kind = 'expansion'").execute(&db).await.unwrap();
        let mut tx = db.begin().await.unwrap();
        start(&mut tx, id, s.project, org, true).await.unwrap();
        tx.commit().await.unwrap();
        run_next(&db, json!({ "expand": true, "why": "Hiring.", "subject": "More seats", "body": "Hi Ines." })).await;
        assert_eq!(latest(&db, s.deal).await.skipped, "you've written to each other in the last two weeks, so bring it up in the thread");

        sqlx::query("update project_settings set grow_accounts = false").execute(&db).await.unwrap();
        let id: Uuid = sqlx::query_scalar("insert into deal_expansions (project_id, deal_id) values ($1, $2) returning id").bind(s.project).bind(s.deal).fetch_one(&db).await.unwrap();
        let mut tx = db.begin().await.unwrap();
        start(&mut tx, id, s.project, org, true).await.unwrap();
        tx.commit().await.unwrap();
        run_next(&db, json!({ "expand": true, "why": "", "subject": "", "body": "Hi" })).await;
        assert_eq!(latest(&db, s.deal).await.skipped, "growing accounts was switched off");
    }
}
