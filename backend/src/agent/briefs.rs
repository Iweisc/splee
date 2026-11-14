//! Meeting briefs: a day before a lead's booked call, the agent writes the team a short page on who they are, what their
//! company is up to, what was said so far, what they did lately and what to ask. One Claude call per brief, from what
//! Splee already knows, without searching the web. A brief written on its own goes to the project's chats and to the
//! lead's owner by email, or to the team when nobody owns the lead. One asked for from the lead's window just shows there.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::write::{WriterSettings, company_signals, lead_brief, materials};
use super::{Ctx, Outcome, kind};
use crate::hooks::{self, ChatAlert};
use crate::models::{Lead, Ts};
use crate::state::AppState;

/// How long before the call the brief is written: the morning before a morning call, so there's time to read it.
pub const AHEAD_HOURS: i32 = 24;
const BRIEFER: &str = "You prepare salespeople for calls with leads for Splee, an outbound sales tool. Use only what you're given. \
Never invent facts, numbers, names or quotes. When something isn't known, leave it out. Address the salesperson as \"you\" and \
never write as \"I\". Plain text only, no markdown.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Brief {
    /// Who they are and why they're on the call, in 2 or 3 sentences.
    pub summary: String,
    /// Facts about them and their company.
    pub about: Vec<String>,
    /// The conversation so far, or empty when there was none.
    pub so_far: String,
    /// What they or their company did lately: pages read, funding, hiring.
    pub signals: Vec<String>,
    pub talking_points: Vec<String>,
    pub questions: Vec<String>,
    pub watch_outs: Vec<String>,
}

fn schema() -> Value {
    let list = json!({ "type": "array", "items": { "type": "string" } });
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" }, "about": list, "so_far": { "type": "string" }, "signals": list,
            "talking_points": list, "questions": list, "watch_outs": list,
        },
        "required": ["summary", "about", "so_far", "signals", "talking_points", "questions", "watch_outs"]
    })
}

/// The lead's latest brief, for their window: `status` is ready, writing or failed.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Saved {
    pub id: Uuid,
    pub meeting_at: Option<Ts>,
    pub brief: Option<Value>,
    pub written_at: Option<Ts>,
    pub status: String,
}

pub async fn latest(db: &PgPool, lead_id: Uuid) -> sqlx::Result<Option<Saved>> {
    sqlx::query_as(
        "select b.id, b.meeting_at, b.brief, b.written_at,
                case when b.brief is not null then 'ready' when r.status in ('queued', 'running') then 'writing' else 'failed' end as status
         from meeting_briefs b left join agent_runs r on r.id = b.run_id
         where b.lead_id = $1 order by b.created_at desc limit 1",
    )
    .bind(lead_id)
    .fetch_optional(db)
    .await
}

/// Writes the lead a brief now, for their booked call if they have one, replacing the one for that call. Not posted.
pub async fn request(db: &PgPool, lead: &Lead, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let posted: bool = sqlx::query_scalar("delete from meeting_briefs where lead_id = $1 and meeting_at is not distinct from $2 returning posted")
        .bind(lead.id)
        .bind(lead.meeting_at)
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(false);
    let id: Uuid = sqlx::query_scalar("insert into meeting_briefs (project_id, lead_id, meeting_at, auto, posted) values ($1, $2, $3, false, $4) returning id")
        .bind(lead.project_id)
        .bind(lead.id)
        .bind(lead.meeting_at)
        .bind(posted)
        .fetch_one(&mut *tx)
        .await?;
    start(&mut tx, id, lead.project_id, org_id).await?;
    tx.commit().await?;
    Ok(id)
}

async fn start(tx: &mut sqlx::PgConnection, brief_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<()> {
    let run = super::enqueue(&mut *tx, kind::MEETING_BRIEF, Some(project_id), Some(org_id), json!({ "brief_id": brief_id })).await?;
    sqlx::query("update meeting_briefs set run_id = $2 where id = $1").bind(brief_id).bind(run).execute(&mut *tx).await?;
    Ok(())
}

/// Queues a brief for every call in the next AHEAD_HOURS that has none, in projects that keep briefs on. Returns how many.
/// A brief asked for by hand isn't posted anywhere, so unless it replaced one that was, it's written again and posted.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "with due as (
           insert into meeting_briefs (project_id, lead_id, meeting_at)
           select l.project_id, l.id, l.meeting_at from leads l
           join projects p on p.id = l.project_id and p.org_id is not null
           join project_settings s on s.project_id = l.project_id and s.meeting_briefs
           where l.meeting_at > now() and l.meeting_at < now() + make_interval(hours => $1)
           on conflict (lead_id, meeting_at) do update set auto = true, brief = null, written_at = null
             where not meeting_briefs.auto and not meeting_briefs.posted
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id",
    )
    .bind(AHEAD_HOURS)
    .fetch_all(&mut *tx)
    .await?;
    for (id, project_id, org_id) in &due {
        start(&mut tx, *id, *project_id, *org_id).await?;
    }
    tx.commit().await?;
    Ok(due.len())
}

pub fn spawn_worker(db: PgPool) {
    tokio::spawn(async move {
        crate::mail::every(Duration::from_secs(300), || async { queue_due(&db).await.map(|_| ()).map_err(Into::into) }, "meeting briefs").await
    });
}

#[derive(Deserialize)]
struct Input {
    brief_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct Row {
    lead_id: Uuid,
    meeting_at: Option<Ts>,
    auto: bool,
    written: bool,
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let row = sqlx::query_as::<_, Row>("select lead_id, meeting_at, auto, brief is not null as written from meeting_briefs where id = $1 and project_id = $2")
        .bind(input.brief_id)
        .bind(ctx.project_id()?)
        .fetch_optional(db)
        .await?
        .context("the brief was replaced")?;
    if row.written {
        return Ok(Outcome::new("The brief is already written"));
    }
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(row.lead_id).fetch_one(db).await?;
    if row.auto && lead.meeting_at != row.meeting_at {
        return Ok(Outcome::new("The call was moved or cancelled, so this brief isn't needed"));
    }
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    let settings = WriterSettings::load(ctx, lead.project_id).await?;
    let timezone: String = sqlx::query_scalar("select coalesce((select timezone from project_settings where project_id = $1), 'UTC')")
        .bind(lead.project_id)
        .fetch_one(db)
        .await?;

    let signals = company_signals(ctx, &[&lead]).await?;
    let about = lead_brief(&lead, &signals);
    let context = gather(db, &lead, org_id).await?;
    let materials = materials(ctx, lead.project_id).await?;
    let us = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) };
    let when = row.meeting_at.map_or("soon".to_string(), |at| hooks::meeting_time(at, &timezone));
    let prompt = format!(
        "{us} has a call with a lead {when}. Write them a brief to read before it.\n\n\
         What we sell: {name}. {description}\nOur materials:\n{materials}\n\n\
         The lead and their company:\n{about}\n\nWhat else Splee knows:\n{context}\n\n\
         1. summary: who they are and why they're on the call, in 2 or 3 sentences.\n\
         2. about: up to 5 short facts about them and their company that matter for the call.\n\
         3. so_far: what was said between us so far in 2 to 4 sentences, with their own words where they matter. Empty if nothing.\n\
         4. signals: up to 4 things they or their company did lately, like pages they read, funding or hiring, each with when.\n\
         5. talking_points: 3 things to bring up, each tying what we sell to their situation, with proof from our materials where it fits.\n\
         6. questions: 3 to 5 open questions to ask them.\n\
         7. watch_outs: up to 3 objections or risks to be ready for, from what they said or their situation. Empty if none.",
        name = profile.name,
        description = profile.description,
        materials = if materials.is_empty() { "(none)".to_string() } else { materials },
        about = serde_json::to_string_pretty(&about)?,
    );
    ctx.log(format!("writing the brief for the call with {}…", if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name })).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.3, ..Call::writing(BRIEFER, prompt, schema()) };
    let brief = tidy(ctx.ask::<Brief>(&call).await?);

    // Only the run the brief is waiting on saves it. The worker can take a brief asked for by hand over while its run writes.
    let saved = sqlx::query("update meeting_briefs set brief = $2, written_at = now(), posted = posted or $3 where id = $1 and brief is null and run_id = $4")
        .bind(input.brief_id)
        .bind(json!(brief))
        .bind(row.auto)
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    if saved.rows_affected() == 0 {
        return Ok(Outcome::new("The brief is already written"));
    }
    if row.auto {
        deliver(&ctx.state, &lead, org_id, &brief, row.meeting_at).await;
    }
    Ok(Outcome { message: format!("Wrote the brief for the call with {}", lead.full_name), output: json!({ "brief_id": input.brief_id }) })
}

fn tidy(b: Brief) -> Brief {
    let list = |v: Vec<String>, max: usize| v.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).take(max).collect();
    Brief {
        summary: b.summary.trim().to_string(),
        about: list(b.about, 5),
        so_far: b.so_far.trim().to_string(),
        signals: list(b.signals, 4),
        talking_points: list(b.talking_points, 3),
        questions: list(b.questions, 5),
        watch_outs: list(b.watch_outs, 3),
    }
}

/// The conversation, what the lead did, their deal, the team's notes, what they wrote when booking and earlier calls, as
/// lines for the prompt.
async fn gather(db: &PgPool, lead: &Lead, org_id: Uuid) -> sqlx::Result<String> {
    let mut lines = vec![];
    let emails: Vec<(String, String, String, Ts)> = sqlx::query_as(
        "select direction, subject, left(body, 1500), coalesce(sent_at, created_at) from email_messages
         where lead_id = $1 and status in ('sent', 'received') order by coalesce(sent_at, created_at) desc limit 8",
    )
    .bind(lead.id)
    .fetch_all(db)
    .await?;
    if !emails.is_empty() {
        lines.push("Emails, oldest first:".to_string());
        for (direction, subject, body, at) in emails.into_iter().rev() {
            let who = if direction == "inbound" { "They wrote" } else { "We wrote" };
            lines.push(format!("- {who} on {} (\"{subject}\"):\n{}", at.format("%-d %b"), body.trim()));
        }
    }
    let (page_views, clicks): (i64, i64) = sqlx::query_as(
        "select (select count(*) from lead_page_views where lead_id = $1 and filter_reason is null),
                (select count(*) from tracking_hits where lead_id = $1 and kind = 'click' and filter_reason is null)",
    )
    .bind(lead.id)
    .fetch_one(db)
    .await?;
    if page_views > 0 {
        lines.push(format!("They opened the page we wrote for them {page_views} times."));
    }
    if clicks > 0 {
        lines.push(format!("They clicked links in our emails {clicks} times."));
    }
    if let Some(domain) = lead.company_domain.as_deref().filter(|d| !d.is_empty()) {
        let pages: Vec<(String, i64)> = sqlx::query_as(
            "select path, count(*) from visitor_hits where project_id = $1 and domain = $2 and created_at > now() - interval '30 days'
             group by path order by count(*) desc limit 5",
        )
        .bind(lead.project_id)
        .bind(domain)
        .fetch_all(db)
        .await?;
        if !pages.is_empty() {
            let list = pages.iter().map(|(path, n)| format!("{path} ({n})")).collect::<Vec<_>>().join(", ");
            lines.push(format!("People at their company read these pages of our website in the last 30 days: {list}."));
        }
    }
    if let Some(deal) = crate::deals::for_lead(db, lead.id).await? {
        let value = if deal.value_cents > 0 { format!(", worth {} {}", deal.value_cents / 100, deal.currency.to_uppercase()) } else { String::new() };
        lines.push(format!("Their deal is {} in {}{value}.", deal.status, deal.stage));
    }
    if !lead.notes.trim().is_empty() {
        lines.push(format!("Our team's notes on them: {}", lead.notes.trim()));
    }
    if let Some(email) = lead.email.as_deref() {
        let note: Option<String> = sqlx::query_scalar(
            "select b.note from bookings b join booking_pages p on p.id = b.page_id
             where p.org_id = $1 and b.email = $2 and b.cancelled_at is null and b.note <> '' order by b.created_at desc limit 1",
        )
        .bind(org_id)
        .bind(email)
        .fetch_optional(db)
        .await?;
        if let Some(note) = note {
            lines.push(format!("When booking, they wrote: {}", note.trim()));
        }
    }
    let calls: Vec<(Ts, String)> = sqlx::query_as(
        "select started_at, summary from meetings where lead_id = $1 and summary <> ''
         union all select coalesce(called_at, closed_at, due_at), summary from calls where lead_id = $1 and summary <> ''
         order by 1 desc limit 3",
    )
    .bind(lead.id)
        .fetch_all(db)
        .await?;
    for (at, summary) in calls {
        lines.push(format!("Our call on {}: {}", at.format("%-d %b"), summary.trim()));
    }
    Ok(if lines.is_empty() { "Nothing yet: this is the first conversation.".to_string() } else { lines.join("\n") })
}

/// The brief as plain text, for email.
pub fn text(b: &Brief) -> String {
    let mut out = b.summary.clone();
    let mut section = |title: &str, items: &[String]| {
        if !items.is_empty() {
            out += &format!("\n\n{title}\n{}", items.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n"));
        }
    };
    section("About them", &b.about);
    if !b.so_far.is_empty() {
        section("So far", std::slice::from_ref(&b.so_far));
    }
    section("Lately", &b.signals);
    section("Bring up", &b.talking_points);
    section("Ask", &b.questions);
    section("Be ready for", &b.watch_outs);
    out
}

/// Posts the brief to the project's chats, and emails it in full to the lead's owner, or to the team when nobody owns them.
async fn deliver(state: &AppState, lead: &Lead, org_id: Uuid, brief: &Brief, at: Option<Ts>) {
    let link = hooks::lead_url(&state.cfg.public_url, lead.project_id, lead.id);
    let when = hooks::meeting_summary(&state.db, lead, at).await;
    let summary = format!("{when}. {}", brief.summary);
    let questions = brief.questions.iter().take(3).map(|q| format!("- {q}")).collect::<Vec<_>>().join("\n");
    let quote = (!questions.is_empty()).then(|| format!("Ask:\n{questions}"));
    let alert = ChatAlert { label: "Meeting brief", summary: &summary, quote: quote.as_deref(), link: &link, link_text: "Read the brief" };
    let owner: Option<String> = match lead.owner_id {
        Some(id) => sqlx::query_scalar("select email from users where id = $1").bind(id).fetch_optional(&state.db).await.unwrap_or(None),
        None => None,
    };
    let Some(owner) = owner else { return crate::mail::notify_team(state, lead.project_id, org_id, &alert).await };
    hooks::post_chat(&state.db, lead.project_id, &alert).await;
    let subject = format!("Brief: your call with {}", when);
    let body = format!("{}\n\nThe lead in Splee: {link}", text(brief));
    if let Err(e) = crate::mail::send_system_email(state, org_id, &owner, &subject, &body).await {
        tracing::warn!(error = ?e, "couldn't email the brief");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    async fn setup(db: &PgPool, meeting_in: &str) -> (Uuid, Uuid, Uuid) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar(
            "insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\", \"description\": \"Call notes for sales teams\"}') returning id",
        )
        .bind(org)
        .fetch_one(db)
        .await
        .unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, full_name, first_name, email, title, company_name, company_domain, status, meeting_at, notes)
             values ($1, 'Ana Ruiz', 'Ana', 'ana@posthog.example', 'Head of Sales', 'PostHog', 'posthog.example', 'meeting_booked', now() + $2::interval, 'Met at SaaStr')
             returning id",
        )
        .bind(project)
        .bind(meeting_in)
        .fetch_one(db)
        .await
        .unwrap();
        (org, project, lead)
    }

    async fn started(db: &PgPool) -> Ctx {
        let run = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'meeting_brief' returning *").fetch_one(db).await.unwrap();
        Ctx { state: crate::autopilot::tests::state(db.clone()), run, step: AtomicI32::new(0) }
    }

    fn answer() -> Value {
        json!({
            "summary": " Ana runs sales at PostHog and booked a call after our second email. ",
            "about": ["Hiring their first AEs", " "], "so_far": "She asked how notes sync to HubSpot.",
            "signals": ["Opened her page twice"], "talking_points": ["HubSpot sync", "Brightline case", "Onboarding", "One too many"],
            "questions": ["How do reps take notes today?"], "watch_outs": []
        })
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_call_in_the_next_day_gets_one_brief_from_what_splee_knows(db: PgPool) {
        let (_, project, lead) = setup(&db, "3 hours").await;
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, sent_at) values ($1, $2, 'inbound', 'inbound', 'Re: notes', 'Does it sync to HubSpot?', 'received', now())")
            .bind(project)
            .bind(lead)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into calls (project_id, lead_id, kind, status, summary, called_at) values ($1, $2, 'manual', 'done', 'They run 8 reps on HubSpot', now() - interval '2 days')")
            .bind(project)
            .bind(lead)
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 1);
        assert_eq!(queue_due(&db).await.unwrap(), 0, "never twice for the same call");
        assert_eq!(latest(&db, lead).await.unwrap().unwrap().status, "writing");

        let ctx = started(&db).await;
        super::super::fake::answer_with(ctx.run.id, |_| answer());
        run(&ctx).await.unwrap();
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("Does it sync to HubSpot?") && prompt.contains("They run 8 reps on HubSpot") && prompt.contains("Met at SaaStr") && prompt.contains("Call notes for sales teams"), "{prompt}");

        let saved = latest(&db, lead).await.unwrap().unwrap();
        assert_eq!(saved.status, "ready");
        let brief: Brief = serde_json::from_value(saved.brief.unwrap()).unwrap();
        assert_eq!(brief.summary, "Ana runs sales at PostHog and booked a call after our second email.");
        assert_eq!(brief.about, vec!["Hiring their first AEs"], "blank items are dropped");
        assert_eq!(brief.talking_points.len(), 3);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn only_calls_in_the_next_day_in_projects_with_briefs_on_are_queued(db: PgPool) {
        let (_, project, lead) = setup(&db, "3 days").await;
        assert_eq!(queue_due(&db).await.unwrap(), 0, "too early");
        sqlx::query("update leads set meeting_at = now() - interval '1 hour' where id = $1").bind(lead).execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "already happened");
        sqlx::query("update leads set meeting_at = now() + interval '2 hours' where id = $1").bind(lead).execute(&db).await.unwrap();
        sqlx::query("update project_settings set meeting_briefs = false where project_id = $1").bind(project).execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "switched off");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_moved_call_skips_the_old_brief_and_gets_a_new_one(db: PgPool) {
        let (_, _, lead) = setup(&db, "3 hours").await;
        queue_due(&db).await.unwrap();
        sqlx::query("update leads set meeting_at = meeting_at + interval '1 hour' where id = $1").bind(lead).execute(&db).await.unwrap();
        let ctx = started(&db).await;
        super::super::fake::answer_with(ctx.run.id, |_| answer());
        let outcome = run(&ctx).await.unwrap();
        assert!(outcome.message.contains("moved"), "{}", outcome.message);
        assert!(super::super::fake::prompts(ctx.run.id).is_empty(), "no model call for a call that moved");
        assert_eq!(queue_due(&db).await.unwrap(), 1, "the new time gets its own brief");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_brief_asked_for_by_hand_is_written_again_and_posted_unless_one_was_posted(db: PgPool) {
        let (org, _, lead) = setup(&db, "3 hours").await;
        let row = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(lead).fetch_one(&db).await.unwrap();
        request(&db, &row, org).await.unwrap();
        let by_hand = started(&db).await;
        assert_eq!(queue_due(&db).await.unwrap(), 1, "nothing was posted for this call yet");
        super::super::fake::answer_with(by_hand.run.id, |_| answer());
        run(&by_hand).await.unwrap();
        assert_eq!(latest(&db, lead).await.unwrap().unwrap().status, "writing", "the run taken over doesn't save, so the worker's run posts it");
        assert_eq!(queue_due(&db).await.unwrap(), 0, "never twice for the same call");
        let auto: bool = sqlx::query_scalar("select auto and brief is null from meeting_briefs").fetch_one(&db).await.unwrap();
        assert!(auto);

        sqlx::query("update meeting_briefs set brief = '{}', posted = true").execute(&db).await.unwrap();
        request(&db, &row, org).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "a rewrite of a posted brief isn't posted again");
    }

    #[test]
    fn the_email_lists_each_part_under_its_heading_and_skips_empty_ones() {
        let b = Brief {
            summary: "Ana runs sales at PostHog.".into(),
            about: vec!["Hiring AEs".into()],
            so_far: String::new(),
            signals: vec![],
            talking_points: vec!["HubSpot sync".into()],
            questions: vec!["How do reps take notes?".into()],
            watch_outs: vec![],
        };
        assert_eq!(text(&b), "Ana runs sales at PostHog.\n\nAbout them\n- Hiring AEs\n\nBring up\n- HubSpot sync\n\nAsk\n- How do reps take notes?");
    }
}
