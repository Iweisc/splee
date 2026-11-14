//! Nudges on quiet deals: when an open deal goes QUIET_DAYS without an email either way, a call or a stage change, the agent
//! reads the conversation and what changed since, says what likely stalled it, and drafts a short email in the thread with a
//! reason to answer now. The draft waits on the Emails page for approval like any other, and the project's chats hear about
//! it. One nudge per quiet stretch: once the nudge or anything else goes out, the clock starts again.

use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::write::{WriterSettings, company_signals, lead_brief, materials};
use super::{Ctx, Outcome, briefs, kind, meetings};
use crate::deals::LAST_TOUCH;
use crate::hooks::{self, ChatAlert};
use crate::models::{Lead, Ts};
use crate::state::AppState;

pub const QUIET_DAYS: i32 = 14;
const NUDGER: &str = "You write short follow-up emails that restart stalled sales conversations, for Splee, an outbound sales tool. \
Use only what you're given. Never invent facts, numbers, names or quotes. Never guilt-trip, and never write \"just checking in\", \
\"bumping this\", \"circling back\" or \"following up\". Plain text only, no markdown.";

/// Leads a nudge must not go to: opted out, a call coming up, or an email to them already waiting to go out.
const NUDGEABLE: &str = "l.email is not null and l.status not in ('unsubscribed', 'bounced', 'do_not_contact')
    and (l.meeting_at is null or l.meeting_at < now())
    and not exists (select 1 from email_messages w where w.lead_id = l.id and w.direction = 'outbound' and w.status in ('draft', 'approved', 'scheduled', 'sending'))";

/// Queues a nudge for every open deal quiet for QUIET_DAYS that hasn't had one this quiet stretch, in projects that keep
/// nudges on. Returns how many.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(&format!(
        "with quiet as (
           select d.id, d.project_id, {LAST_TOUCH} as quiet_since from deals d
           join leads l on l.id = d.lead_id
           join projects p on p.id = d.project_id and p.org_id is not null
           join project_settings s on s.project_id = d.project_id and s.nudge_quiet_deals
           where d.status = 'open' and {NUDGEABLE}),
         due as (
           insert into deal_nudges (project_id, deal_id, quiet_since)
           select project_id, id, quiet_since from quiet where quiet_since < now() - make_interval(days => $1)
           on conflict (deal_id, quiet_since) where auto do nothing
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id"
    ))
    .bind(QUIET_DAYS)
    .fetch_all(&mut *tx)
    .await?;
    for (id, project_id, org_id) in &due {
        start(&mut tx, *id, *project_id, *org_id).await?;
    }
    tx.commit().await?;
    Ok(due.len())
}

/// Writes a nudge now, from the deal's window, replacing a draft the last one left that nobody approved.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    sqlx::query(
        "delete from email_messages e using deal_nudges n where n.deal_id = $1 and e.id = n.email_id and e.status = 'draft'",
    )
    .bind(deal_id)
    .execute(&mut *tx)
    .await?;
    let id: Uuid = sqlx::query_scalar(&format!(
        "insert into deal_nudges (project_id, deal_id, quiet_since, auto) select d.project_id, d.id, {LAST_TOUCH}, false from deals d where d.id = $1
         returning id"
    ))
    .bind(deal_id)
    .fetch_one(&mut *tx)
    .await?;
    start(&mut tx, id, project_id, org_id).await?;
    tx.commit().await?;
    Ok(id)
}

async fn start(tx: &mut sqlx::PgConnection, nudge_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<()> {
    let run = super::enqueue(&mut *tx, kind::DEAL_NUDGE, Some(project_id), Some(org_id), json!({ "nudge_id": nudge_id })).await?;
    sqlx::query("update deal_nudges set run_id = $2 where id = $1").bind(nudge_id).bind(run).execute(&mut *tx).await?;
    Ok(())
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        crate::mail::every(Duration::from_secs(3600), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "deal nudges").await
    });
}

#[derive(Deserialize)]
struct Input {
    nudge_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct Row {
    deal_id: Uuid,
    quiet_since: Ts,
    auto: bool,
    written: bool,
}

#[derive(sqlx::FromRow)]
struct DealNow {
    lead_id: Uuid,
    status: String,
    stage: String,
    value_cents: i64,
    currency: String,
    notes: String,
    last_touch: Ts,
}

#[derive(Deserialize)]
struct Nudge {
    why: String,
    subject: String,
    body: String,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": { "why": { "type": "string" }, "subject": { "type": "string" }, "body": { "type": "string" } },
        "required": ["why", "subject", "body"]
    })
}

/// Marks the nudge done without a draft, saying why.
async fn skip(db: &PgPool, id: Uuid, why: &str) -> anyhow::Result<Outcome> {
    sqlx::query("update deal_nudges set why = $2, written_at = now() where id = $1").bind(id).bind(why).execute(db).await?;
    Ok(Outcome::new(format!("No nudge: {why}")))
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let row = sqlx::query_as::<_, Row>(
        "select deal_id, quiet_since, auto, written_at is not null as written from deal_nudges where id = $1 and project_id = $2",
    )
    .bind(input.nudge_id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the nudge was deleted")?;
    if row.written {
        return Ok(Outcome::new("The nudge is already written"));
    }
    if row.auto {
        let on: bool = sqlx::query_scalar("select coalesce((select nudge_quiet_deals from project_settings where project_id = $1), true)")
            .bind(ctx.project_id()?)
            .fetch_one(db)
            .await?;
        if !on {
            return skip(db, input.nudge_id, "nudging quiet deals was switched off").await;
        }
    }
    let deal = sqlx::query_as::<_, DealNow>(&format!(
        "select d.lead_id, d.status, s.name as stage, d.value_cents, d.currency, d.notes, {LAST_TOUCH} as last_touch
         from deals d join deal_stages s on s.id = d.stage_id where d.id = $1"
    ))
    .bind(row.deal_id)
    .fetch_one(db)
    .await?;
    if deal.status != "open" {
        return skip(db, input.nudge_id, "the deal closed").await;
    }
    if row.auto && deal.last_touch != row.quiet_since {
        return skip(db, input.nudge_id, "the conversation picked up again").await;
    }
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(deal.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    if let Some(blocker) = meetings::follow_up_blocker(&mut *db.acquire().await?, org_id, &lead).await? {
        return skip(db, input.nudge_id, blocker).await;
    }
    let waiting: bool = sqlx::query_scalar(&format!("select not ({NUDGEABLE}) from leads l where l.id = $1")).bind(lead.id).fetch_one(db).await?;
    if waiting {
        return skip(db, input.nudge_id, "an email to them is already waiting to go out, or a call is booked").await;
    }

    let settings = WriterSettings::load(ctx, lead.project_id).await?;
    let lang = crate::language::for_lead(db, lead.id).await?;
    let signals = company_signals(ctx, &[&lead]).await?;
    let about = lead_brief(&lead, &signals);
    let context = briefs::gather(db, &lead, org_id).await?;
    let materials = materials(ctx, lead.project_id).await?;
    let days = (chrono::Utc::now() - deal.last_touch).num_days();
    let value = if deal.value_cents > 0 { format!(", worth {} {}", deal.value_cents / 100, deal.currency.to_uppercase()) } else { String::new() };
    let notes = if deal.notes.trim().is_empty() { String::new() } else { format!("\nThe deal's notes: {}", deal.notes.trim()) };
    let us = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) };
    let prompt = format!(
        "{us} has an open deal with this lead in the {stage} stage{value}, and nobody has written, called or moved it in {days} \
         days.{notes}\n\n\
         What we sell: {name}. {description}\nOur materials:\n{materials}\n\n\
         The lead and their company:\n{about}\n\nWhat else Splee knows:\n{context}\n\n\
         1. why: in one short sentence of under 25 words, what most likely stalled it, from the conversation. Say so when nothing shows why.\n\
         2. subject: a short subject. It goes in the existing thread when there is one.\n\
         3. body: a 40 to 90 word email from {us} that gives them a reason to answer now. Lead with something new: what \
         their company did lately, a page of ours they read, or a fact from our materials that answers what they asked or \
         worried about. End with one easy question or a concrete next step. Mention earlier conversation only as far as it's \
         given. No sign-off. {language}",
        stage = deal.stage,
        name = profile.name,
        description = profile.description,
        materials = if materials.is_empty() { "(none)".to_string() } else { materials },
        about = serde_json::to_string_pretty(&about)?,
        language = crate::language::instruction(lang),
    );
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    ctx.log(format!("reading the deal with {first} and writing a nudge…")).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.3, ..Call::writing(NUDGER, prompt, schema()) };
    let nudge: Nudge = ctx.ask(&call).await?;
    anyhow::ensure!(!nudge.body.trim().is_empty(), "the agent wrote an empty email");

    let mut tx = db.begin().await?;
    let last: Option<(Option<Uuid>, String)> = sqlx::query_as(
        "select mailbox_id, subject from email_messages where lead_id = $1 and direction = 'outbound' and status = 'sent' order by sent_at desc limit 1",
    )
    .bind(lead.id)
    .fetch_optional(&mut *tx)
    .await?;
    let (mailbox, subject) = match last {
        Some((mailbox, s)) if !s.trim().is_empty() => (mailbox, if s.to_lowercase().starts_with("re:") { s } else { format!("Re: {s}") }),
        Some((mailbox, _)) => (mailbox, nudge.subject.trim().to_string()),
        None => (None, nudge.subject.trim().to_string()),
    };
    let email_id: Uuid = sqlx::query_scalar(
        "insert into email_messages (project_id, lead_id, segment_id, mailbox_id, direction, kind, subject, body, status, to_email, language)
         values ($1, $2, $3, $4, 'outbound', 'deal_nudge', $5, $6, 'draft', $7, $8) returning id",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(lead.segment_id)
    .bind(mailbox)
    .bind(&subject)
    .bind(settings.sign(nudge.body.trim()))
    .bind(&lead.email)
    .bind(lang.code)
    .fetch_one(&mut *tx)
    .await?;
    // A run started twice for the nudge drafts once.
    let saved = sqlx::query("update deal_nudges set email_id = $2, why = $3, written_at = now() where id = $1 and written_at is null")
        .bind(input.nudge_id)
        .bind(email_id)
        .bind(nudge.why.trim())
        .execute(&mut *tx)
        .await?;
    if saved.rows_affected() == 0 {
        return Ok(Outcome::new("The nudge is already written"));
    }
    tx.commit().await?;

    if row.auto {
        let link = hooks::lead_url(&ctx.state.cfg.public_url, lead.project_id, lead.id);
        let summary = format!("Your deal with {} went quiet {days} days ago. {} A nudge is waiting for your approval.", lead.full_name, nudge.why.trim());
        let alert = ChatAlert { label: "Quiet deal", summary: &summary, quote: Some(nudge.body.trim()), link: &link, link_text: "Review the nudge" };
        hooks::post_chat(db, lead.project_id, &alert).await;
    }
    Ok(Outcome { message: format!("Drafted a nudge to {}", lead.full_name), output: json!({ "email_id": email_id }) })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    struct Setup {
        org: Uuid,
        project: Uuid,
        lead: Uuid,
        deal: Uuid,
    }

    async fn setup(db: &PgPool, quiet_days: i32) -> Setup {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Ours') returning id").fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, domain) values ($1, 'Fieldnote', 'fieldnote.example') returning id")
            .bind(org)
            .fetch_one(db)
            .await
            .unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1) on conflict do nothing").bind(project).execute(db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, full_name, first_name, title, email, company_name, company_domain, status)
             values ($1, 'Ana Ruiz', 'Ana', 'Head of Sales', 'ana@acme.example', 'Acme', 'acme.example', 'hot') returning id",
        )
        .bind(project)
        .fetch_one(db)
        .await
        .unwrap();
        let at = format!("{quiet_days} days");
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, sent_at)
             values ($1, $2, 'outbound', 'initial', 'Calls in HubSpot', 'Hi Ana', 'sent', now() - $3::interval - interval '2 days'),
                    ($1, $2, 'inbound', 'inbound', 'Re: Calls in HubSpot', 'What does it cost for 5?', 'received', now() - $3::interval)",
        )
        .bind(project)
        .bind(lead)
        .bind(&at)
        .execute(db)
        .await
        .unwrap();
        let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Talking', 'open', 0) returning id")
            .bind(org)
            .fetch_one(db)
            .await
            .unwrap();
        let deal: Uuid = sqlx::query_scalar(
            "insert into deals (org_id, project_id, lead_id, stage_id, stage_changed_at, created_at) values ($1, $2, $3, $4, now() - interval '30 days', now() - interval '30 days') returning id",
        )
        .bind(org)
        .bind(project)
        .bind(lead)
        .bind(stage)
        .fetch_one(db)
        .await
        .unwrap();
        Setup { org, project, lead, deal }
    }

    async fn run_queued(db: &PgPool, nudge: Nudge) -> (Outcome, String) {
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'deal_nudge' and status = 'queued' returning *")
            .fetch_one(db)
            .await
            .unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        let answer = json!({ "why": nudge.why, "subject": nudge.subject, "body": nudge.body });
        super::super::fake::answer_with(ctx.run.id, move |_| answer.clone());
        let outcome = run(&ctx).await.unwrap();
        let prompt = super::super::fake::prompts(ctx.run.id).first().cloned().unwrap_or_default();
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(ctx.run.id).execute(db).await.unwrap();
        (outcome, prompt)
    }

    fn nudge() -> Nudge {
        Nudge { why: " She asked about price and never heard back. ".into(), subject: "Pricing".into(), body: "Ana, saw you hired 3 AEs.".into() }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_quiet_deal_gets_one_nudge_per_quiet_stretch_in_its_thread(db: PgPool) {
        let s = setup(&db, 15).await;
        assert_eq!(queue_due(&db).await.unwrap(), 1);
        assert_eq!(queue_due(&db).await.unwrap(), 0, "once per quiet stretch");

        let (_, prompt) = run_queued(&db, nudge()).await;
        assert!(prompt.contains("in the Talking stage, and nobody has written, called or moved it in 15 days"), "{prompt}");
        assert!(prompt.contains("They wrote on") && prompt.contains("What does it cost for 5?"), "the thread is in the prompt: {prompt}");
        let (kind, status, subject): (String, String, String) =
            sqlx::query_as("select kind, status, subject from email_messages where lead_id = $1 and direction = 'outbound' and status = 'draft'")
                .bind(s.lead)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!((kind.as_str(), status.as_str(), subject.as_str()), ("deal_nudge", "draft", "Re: Calls in HubSpot"));
        let deal = crate::deals::get(&db, s.org, s.deal).await.unwrap();
        let shown = deal.nudge.unwrap();
        assert_eq!((shown["status"].as_str(), shown["why"].as_str()), (Some("done"), Some("She asked about price and never heard back.")));

        // The draft waiting to go out holds off another nudge even after the stretch would allow one.
        sqlx::query("delete from deal_nudges").execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "an email is already waiting");

        // Sending it starts the clock again.
        sqlx::query("update email_messages set status = 'sent', sent_at = now() where kind = 'deal_nudge'").execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "just touched");
        let deal = crate::deals::get(&db, s.org, s.deal).await.unwrap();
        assert!(chrono::Utc::now() - deal.last_touch_at < chrono::Duration::minutes(1));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn no_nudge_for_a_recent_deal_a_booked_call_or_when_switched_off(db: PgPool) {
        let s = setup(&db, 5).await;
        assert_eq!(queue_due(&db).await.unwrap(), 0, "talked 5 days ago");

        sqlx::query("update email_messages set sent_at = sent_at - interval '20 days'").execute(&db).await.unwrap();
        sqlx::query("update leads set meeting_at = now() + interval '2 days' where id = $1").bind(s.lead).execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "a call is booked");

        sqlx::query("update leads set meeting_at = null").execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 1);
        sqlx::query("update project_settings set nudge_quiet_deals = false where project_id = $1").bind(s.project).execute(&db).await.unwrap();
        let (outcome, _) = run_queued(&db, nudge()).await;
        assert_eq!(outcome.message, "No nudge: nudging quiet deals was switched off", "switched off after it was queued");
        sqlx::query("delete from deal_nudges").execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "switched off");

        // Asking from the deal's window works even when it's switched off.
        request(&db, s.deal, s.project, s.org).await.unwrap();
        let (outcome, _) = run_queued(&db, nudge()).await;
        assert!(outcome.message.starts_with("Drafted a nudge"), "{}", outcome.message);
    }
}
