//! Win-backs: once a deal is lost, the agent reads why (the deal's review, the reason the team gave and the conversation)
//! and picks the day to try again, with what to lead with. On that day, or as soon as the buyer comes back (they open the
//! page we wrote for them, click a link in our emails, or someone at their company reads our website), it drafts the email
//! in the thread. The draft waits on the Emails page for approval like a nudge, and the project's chats hear about it.

use std::time::Duration;

use anyhow::Context;
use chrono::{Days, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::nudges::NUDGEABLE;
use super::sdk::Call;
use super::write::{WriterSettings, company_signals, lead_brief, materials};
use super::{Ctx, Outcome, briefs, kind, meetings};
use crate::hooks::{self, ChatAlert};
use crate::models::{Lead, Ts};
use crate::state::AppState;

/// Deals lost this far back get a plan, so turning it on covers last year's losses too.
const DAYS: i32 = 365;
const PER_PASS: i64 = 20;
/// The earliest a plan tries again after the loss.
const MIN_WAIT_DAYS: u64 = 30;
/// Activity this soon after the loss is the buyer reading what we sent before deciding, not coming back.
const COME_BACK_AFTER_DAYS: i32 = 14;

const PLANNER: &str = "You plan when to try a lost B2B deal again, for Splee, an outbound sales tool, like a patient, \
honest sales manager. Use only what you're given, and never invent dates or facts.";
const WRITER: &str = "You write short emails that reopen lost sales conversations, for Splee, an outbound sales tool. Use only \
what you're given. Never invent facts, numbers, names or quotes. Never guilt-trip, never mention that we lost the deal, and \
never write \"just checking in\", \"circling back\" or \"following up\". Plain text only, no markdown.";

/// A win-back as the Pipeline's Win-back view and the deal's window show it. `status` is planning, waiting, writing,
/// drafted, skipped or failed. A drafted one's email says whether it went out. One whose email was deleted, cancelled or
/// failed counts as skipped, saying so, so it can be planned or written again.
pub const SELECT: &str = "select w.id, w.deal_id, d.lead_id, l.full_name as lead_name, l.company_name,
        nullif(u.name, '') as owner_name, d.value_cents, d.currency, d.status as deal_status, w.lost_at, rv.reason,
        w.reach_out_on, w.why, w.angle, w.came_back,
        case when w.skipped <> '' then w.skipped when e.status = 'failed' then 'The email failed to send'
             when e.status = 'cancelled' then 'The email was cancelled' when w.written_at is not null then 'The draft was deleted'
             else '' end as skipped,
        w.email_id, e.subject as email_subject, e.body as email_body,
        e.status as email_status,
        case when r.status in ('queued', 'running') then case when w.reach_out_on is null then 'planning' else 'writing' end
             when w.written_at is not null then case when e.status in ('failed', 'cancelled') or e.id is null then 'skipped' else 'drafted' end
             when w.reach_out_on is null or coalesce((r.input->>'write')::boolean, false) then 'failed'
             else 'waiting' end as status
     from deal_win_backs w join deals d on d.id = w.deal_id join leads l on l.id = d.lead_id
     left join users u on u.id = l.owner_id left join agent_runs r on r.id = w.run_id
     left join email_messages e on e.id = w.email_id
     left join deal_reviews rv on rv.deal_id = w.deal_id and rv.outcome = 'lost' and rv.created_at >= w.lost_at";

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct WinBack {
    pub id: Uuid,
    pub deal_id: Uuid,
    pub lead_id: Uuid,
    pub lead_name: String,
    pub company_name: String,
    pub owner_name: Option<String>,
    pub value_cents: i64,
    pub currency: String,
    pub deal_status: String,
    pub lost_at: Ts,
    /// The deal review's reason, once it's written. See agent/reviews.rs.
    pub reason: Option<String>,
    pub reach_out_on: Option<NaiveDate>,
    pub why: String,
    pub angle: String,
    pub came_back: String,
    pub skipped: String,
    pub email_id: Option<Uuid>,
    pub email_subject: Option<String>,
    pub email_body: Option<String>,
    pub email_status: Option<String>,
    pub status: String,
}

/// Plans lost deals, brings the day forward for buyers who came back, and writes the ones due. Returns how many runs it queued.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let planned: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "with due as (
           insert into deal_win_backs (project_id, deal_id, lost_at)
           select d.project_id, d.id, d.closed_at from deals d
           join projects p on p.id = d.project_id and p.org_id is not null
           join project_settings s on s.project_id = d.project_id and s.win_back_lost_deals
           left join deal_reviews rv on rv.deal_id = d.id and rv.outcome = 'lost' and rv.created_at >= d.closed_at
           left join deal_win_backs w on w.deal_id = d.id
           where d.status = 'lost' and d.closed_at > now() - make_interval(days => $1) and (w.id is null or w.lost_at < d.closed_at)
             -- Waits a day for the review, so the plan knows why it was lost.
             and (rv.reason is not null or d.closed_at < now() - interval '1 day')
           order by d.closed_at desc limit $2
           on conflict (deal_id) do update set lost_at = excluded.lost_at, run_id = null, email_id = null, reach_out_on = null,
             why = '', angle = '', came_back = '', skipped = '', written_at = null, dismissed_at = null, created_at = now()
           where deal_win_backs.lost_at < excluded.lost_at
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id",
    )
    .bind(DAYS)
    .bind(PER_PASS)
    .fetch_all(&mut *tx)
    .await?;
    for (id, project_id, org_id) in &planned {
        start(&mut tx, *id, *project_id, *org_id, false, true).await?;
    }

    // The latest sign the buyer came back, in a sentence for the team and the writer.
    sqlx::query(
        "update deal_win_backs w set reach_out_on = current_date, came_back = c.what
         from (select w.id, coalesce(
                 (select 'They opened the page we wrote for them on ' || to_char(max(v.created_at), 'FMDD Mon') from lead_page_views v
                  where v.lead_id = d.lead_id and v.filter_reason is null and v.created_at > w.lost_at + make_interval(days => $1) having count(*) > 0),
                 (select 'They clicked a link in one of our emails on ' || to_char(max(t.created_at), 'FMDD Mon') from tracking_hits t
                  where t.lead_id = d.lead_id and t.kind = 'click' and t.filter_reason is null and t.created_at > w.lost_at + make_interval(days => $1)
                  having count(*) > 0),
                 (select 'Someone at their company read ' || h.path || ' on our website on ' || to_char(h.created_at, 'FMDD Mon') from visitor_hits h
                  where h.project_id = w.project_id and h.domain = l.company_domain and h.created_at > w.lost_at + make_interval(days => $1)
                  order by h.created_at desc limit 1)) as what
               from deal_win_backs w join deals d on d.id = w.deal_id join leads l on l.id = d.lead_id
               where w.reach_out_on > current_date and w.written_at is null and w.dismissed_at is null
                 and d.status = 'lost' and d.closed_at = w.lost_at) c
         where c.id = w.id and c.what is not null",
    )
    .bind(COME_BACK_AFTER_DAYS)
    .execute(&mut *tx)
    .await?;

    // A write that failed waits for someone to ask again, rather than failing every hour.
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "select w.id, w.project_id, p.org_id from deal_win_backs w
         join projects p on p.id = w.project_id and p.org_id is not null
         join project_settings s on s.project_id = w.project_id and s.win_back_lost_deals
         join deals d on d.id = w.deal_id left join agent_runs r on r.id = w.run_id
         where w.reach_out_on <= current_date and w.written_at is null and w.dismissed_at is null
           and d.status = 'lost' and d.closed_at = w.lost_at
           and (r.id is null or (r.status = 'succeeded' and not coalesce((r.input->>'write')::boolean, false)))
         order by w.reach_out_on limit $1
         for update of w skip locked",
    )
    .bind(PER_PASS)
    .fetch_all(&mut *tx)
    .await?;
    for (id, project_id, org_id) in &due {
        start(&mut tx, *id, *project_id, *org_id, true, true).await?;
    }
    tx.commit().await?;
    Ok(planned.len() + due.len())
}

async fn start(tx: &mut sqlx::PgConnection, id: Uuid, project_id: Uuid, org_id: Uuid, write: bool, auto: bool) -> sqlx::Result<Uuid> {
    let input = json!({ "win_back_id": id, "write": write, "auto": auto });
    let run = super::enqueue(&mut *tx, kind::WIN_BACK, Some(project_id), Some(org_id), input).await?;
    sqlx::query("update deal_win_backs set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

/// Writes the email for a lost deal now, from the deal's window, replacing a draft nobody approved. Keeps the plan's
/// reasons for the same loss. An email already being written is reused, and a plan being written gives way, since its
/// save checks the run. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid, lost_at: Ts) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "insert into deal_win_backs (project_id, deal_id, lost_at) values ($1, $2, $3)
         on conflict (deal_id) do update set
           why = case when deal_win_backs.lost_at = excluded.lost_at then deal_win_backs.why else '' end,
           angle = case when deal_win_backs.lost_at = excluded.lost_at then deal_win_backs.angle else '' end,
           came_back = case when deal_win_backs.lost_at = excluded.lost_at then deal_win_backs.came_back else '' end,
           lost_at = excluded.lost_at
         returning id",
    )
    .bind(project_id)
    .bind(deal_id)
    .bind(lost_at)
    .fetch_one(&mut *tx)
    .await?;
    let writing: Option<Uuid> = sqlx::query_scalar(
        "select r.id from deal_win_backs w join agent_runs r on r.id = w.run_id
         where w.id = $1 and r.status in ('queued', 'running') and coalesce((r.input->>'write')::boolean, false)",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(run) = writing {
        tx.commit().await?;
        return Ok(run);
    }
    sqlx::query("delete from email_messages e using deal_win_backs w where w.id = $1 and e.id = w.email_id and e.status = 'draft'")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "update deal_win_backs set reach_out_on = current_date, email_id = null, skipped = '', written_at = null, dismissed_at = null
         where id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let run = start(&mut tx, id, project_id, org_id, true, false).await?;
    tx.commit().await?;
    Ok(run)
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        crate::mail::every(Duration::from_secs(3600), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "win-backs").await
    });
}

#[derive(Deserialize)]
struct Input {
    win_back_id: Uuid,
    write: bool,
    auto: bool,
}

#[derive(sqlx::FromRow)]
struct Row {
    deal_id: Uuid,
    lost_at: Ts,
    why: String,
    angle: String,
    came_back: String,
    status: String,
    closed_at: Option<Ts>,
    lead_id: Uuid,
    value_cents: i64,
    currency: String,
    notes: String,
    lost_reason: String,
    created_at: Ts,
    /// When the plan was queued. The plan read the conversation up to then.
    planned_at: Ts,
}

#[derive(sqlx::FromRow)]
struct Review {
    reason: Option<String>,
    story: String,
    turning_point: String,
    competitor: String,
    lesson: String,
}

#[derive(Deserialize)]
struct Plan {
    reach_out_on: String,
    why: String,
    angle: String,
}

#[derive(Deserialize)]
struct Email {
    subject: String,
    body: String,
}

fn plan_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "reach_out_on": { "type": "string" }, "why": { "type": "string" }, "angle": { "type": "string" } },
        "required": ["reach_out_on", "why", "angle"]
    })
}

fn email_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "subject": { "type": "string" }, "body": { "type": "string" } },
        "required": ["subject", "body"]
    })
}

/// The day the plan picked, kept between MIN_WAIT_DAYS and a year after the loss and never before today. None when the
/// agent says not to try again.
fn reach_out_on(answer: &str, lost_on: NaiveDate, today: NaiveDate) -> anyhow::Result<Option<NaiveDate>> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(None);
    }
    let day = NaiveDate::parse_from_str(answer, "%Y-%m-%d").with_context(|| format!("the agent picked {answer:?}, not a date"))?;
    let earliest = lost_on + Days::new(MIN_WAIT_DAYS);
    let latest = lost_on + Days::new(DAYS as u64);
    Ok(Some(day.clamp(earliest, latest).max(today)))
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let row = sqlx::query_as::<_, Row>(
        "select w.deal_id, w.lost_at, w.why, w.angle, w.came_back, d.status, d.closed_at, d.lead_id, d.value_cents, d.currency,
                d.notes, d.lost_reason, d.created_at, w.created_at as planned_at
         from deal_win_backs w join deals d on d.id = w.deal_id where w.id = $1 and w.project_id = $2",
    )
    .bind(input.win_back_id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the win-back was deleted")?;
    if row.status != "lost" || row.closed_at != Some(row.lost_at) {
        return skip(ctx, input.win_back_id, "the deal was reopened").await;
    }
    if input.auto {
        let on: bool = sqlx::query_scalar("select coalesce((select win_back_lost_deals from project_settings where project_id = $1), true)")
            .bind(ctx.project_id()?)
            .fetch_one(db)
            .await?;
        if !on {
            return skip(ctx, input.win_back_id, "winning back lost deals was switched off").await;
        }
    }
    let review = sqlx::query_as::<_, Review>(
        "select reason, story, turning_point, competitor, lesson from deal_reviews
         where deal_id = $1 and outcome = 'lost' and reason is not null and created_at >= $2",
    )
    .bind(row.deal_id)
    .bind(row.lost_at)
    .fetch_optional(db)
    .await?;
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(row.lead_id).fetch_one(db).await?;
    if input.write { write(ctx, input.win_back_id, &row, review.as_ref(), &lead).await } else { plan(ctx, input.win_back_id, &row, review.as_ref(), &lead).await }
}

/// Marks the win-back done without an email, saying why.
async fn skip(ctx: &Ctx, id: Uuid, why: &str) -> anyhow::Result<Outcome> {
    sqlx::query("update deal_win_backs set skipped = $2, written_at = now() where id = $1 and run_id = $3 and written_at is null")
        .bind(id)
        .bind(why)
        .bind(ctx.run.id)
        .execute(ctx.db())
        .await?;
    Ok(Outcome::new(format!("No win-back: {why}")))
}

/// What we know about the loss, as lines for the prompt.
fn the_loss(row: &Row, review: Option<&Review>) -> String {
    let mut lines = vec![format!(
        "We lost the deal on {}{}, {} days after it opened.",
        row.lost_at.format("%-d %b %Y"),
        if row.value_cents > 0 { format!(", worth {} {}", row.value_cents / 100, row.currency.to_uppercase()) } else { String::new() },
        (row.lost_at - row.created_at).num_days(),
    )];
    let mut line = |label: &str, text: &str| {
        if !text.trim().is_empty() {
            lines.push(format!("{label}: {}", text.trim()));
        }
    };
    line("The reason our team gave", &row.lost_reason);
    line("The deal's notes", &row.notes);
    if let Some(r) = review {
        line("Our review's main reason", r.reason.as_deref().unwrap_or(""));
        line("How it went", &r.story);
        line("The turning point", &r.turning_point);
        line("The competitor they chose", &r.competitor);
        line("The lesson", &r.lesson);
    }
    lines.join("\n")
}

async fn plan(ctx: &Ctx, id: Uuid, row: &Row, review: Option<&Review>, lead: &Lead) -> anyhow::Result<Outcome> {
    let db = ctx.db();
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    let context = briefs::gather(db, lead, org_id).await?;
    let today = Utc::now().date_naive();
    let lost_on = row.lost_at.date_naive();
    let prompt = format!(
        "{us} lost a deal with {who}.\n{loss}\n\nThe conversation and what else Splee knows:\n{context}\n\n\
         Today is {today}. Plan when to try again.\n\
         1. reach_out_on: the day to write to them again, as YYYY-MM-DD, between {earliest} and {latest}. Tie it to what they \
         said: a month before a contract they mentioned renews, the quarter or budget cycle they pointed to, or when they said \
         to come back. With no such clue, about 90 days after the loss. Empty only if trying again makes no sense: they asked \
         us not to contact them, the company closed, or we can't serve them at all.\n\
         2. why: under 25 words, why that day, naming the clue and its date from the evidence. If empty, why not.\n\
         3. angle: under 30 words, an instruction for the email that reopens it, given why we lost, like \"Lead with …\".",
        us = profile.name,
        who = [lead.full_name.trim(), lead.company_name.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" at "),
        loss = the_loss(row, review),
        earliest = lost_on + Days::new(MIN_WAIT_DAYS),
        latest = lost_on + Days::new(DAYS as u64),
    );
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    ctx.log(format!("reading why we lost {first}'s deal and planning when to try again…")).await?;
    let call = Call { timeout: Duration::from_secs(2 * 60), budget_usd: 0.15, ..Call::writing(PLANNER, prompt, plan_schema()) };
    let answer: Plan = ctx.ask(&call).await?;
    let Some(day) = reach_out_on(&answer.reach_out_on, lost_on, today)? else {
        let why = if answer.why.trim().is_empty() { "the agent says not to try again" } else { answer.why.trim() };
        return skip(ctx, id, why).await;
    };
    sqlx::query("update deal_win_backs set reach_out_on = $2, why = $3, angle = $4 where id = $1 and run_id = $5 and written_at is null")
        .bind(id)
        .bind(day)
        .bind(answer.why.trim())
        .bind(answer.angle.trim())
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    Ok(Outcome { message: format!("Planned to try {}'s deal again on {day}", lead.full_name), output: json!({ "reach_out_on": day }) })
}

async fn write(ctx: &Ctx, id: Uuid, row: &Row, review: Option<&Review>, lead: &Lead) -> anyhow::Result<Outcome> {
    let db = ctx.db();
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    if let Some(blocker) = meetings::follow_up_blocker(&mut *db.acquire().await?, org_id, lead).await? {
        return skip(ctx, id, blocker).await;
    }
    let (wrote, waiting): (bool, bool) = sqlx::query_as(&format!(
        "select exists (select 1 from email_messages i where i.lead_id = l.id and i.direction = 'inbound'
                  and i.classification is distinct from 'bounce' and i.created_at > $2),
                not ({NUDGEABLE})
         from leads l where l.id = $1"
    ))
    .bind(lead.id)
    .bind(row.planned_at)
    .fetch_one(db)
    .await?;
    if wrote {
        return skip(ctx, id, "they wrote to us since it was planned, so answer them in the thread").await;
    }
    if waiting {
        return skip(ctx, id, "an email to them is already waiting to go out, or a call is booked").await;
    }

    let settings = WriterSettings::load(ctx, lead.project_id).await?;
    let lang = crate::language::for_lead(db, lead.id).await?;
    let signals = company_signals(ctx, &[lead]).await?;
    let about = lead_brief(lead, &signals);
    let context = briefs::gather(db, lead, org_id).await?;
    let materials = materials(ctx, lead.project_id).await?;
    let us = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) };
    let mut plan = vec![];
    if !row.why.trim().is_empty() {
        plan.push(format!("We planned to write today because: {}", row.why.trim()));
    }
    if !row.came_back.trim().is_empty() {
        plan.push(format!("What brought it forward: {}.", row.came_back.trim()));
    }
    if !row.angle.trim().is_empty() {
        plan.push(format!("The plan for the email: {}", row.angle.trim()));
    }
    let prompt = format!(
        "{loss}\n{plan}\n\n\
         What we sell: {name}. {description}\nOur materials:\n{materials}\n\n\
         The lead and their company now:\n{about}\n\nThe conversation and what else Splee knows:\n{context}\n\n\
         1. subject: a short subject. It goes in the existing thread when there is one.\n\
         2. body: a 50 to 100 word email from {us} that reopens the conversation. Lead with what changed since: something new \
         at their company, what they read of ours lately, or something new on our side from the materials that answers why \
         they said no. Never mention that they read our website or pages or clicked our links, or guess why: use that only to \
         pick what to lead with. Don't mention losing the deal or ask why they chose someone else. End with one easy \
         question. No sign-off. {language}",
        loss = the_loss(row, review),
        plan = plan.join("\n"),
        name = profile.name,
        description = profile.description,
        materials = if materials.is_empty() { "(none)".to_string() } else { materials },
        about = serde_json::to_string_pretty(&about)?,
        language = crate::language::instruction(lang),
    );
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    ctx.log(format!("writing to {first} to reopen the lost deal…")).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.3, ..Call::writing(WRITER, prompt, email_schema()) };
    let email: Email = ctx.ask(&call).await?;
    anyhow::ensure!(!email.body.trim().is_empty(), "the agent wrote an empty email");

    let mut tx = db.begin().await?;
    let last: Option<(Option<Uuid>, String)> = sqlx::query_as(
        "select mailbox_id, subject from email_messages where lead_id = $1 and direction = 'outbound' and status = 'sent' order by sent_at desc limit 1",
    )
    .bind(lead.id)
    .fetch_optional(&mut *tx)
    .await?;
    let (mailbox, subject) = match last {
        Some((mailbox, s)) if !s.trim().is_empty() => (mailbox, if s.to_lowercase().starts_with("re:") { s } else { format!("Re: {s}") }),
        Some((mailbox, _)) => (mailbox, email.subject.trim().to_string()),
        None => (None, email.subject.trim().to_string()),
    };
    let email_id: Uuid = sqlx::query_scalar(
        "insert into email_messages (project_id, lead_id, segment_id, mailbox_id, direction, kind, subject, body, status, to_email, language)
         values ($1, $2, $3, $4, 'outbound', 'win_back', $5, $6, 'draft', $7, $8) returning id",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(lead.segment_id)
    .bind(mailbox)
    .bind(&subject)
    .bind(settings.sign(email.body.trim()))
    .bind(&lead.email)
    .bind(lang.code)
    .fetch_one(&mut *tx)
    .await?;
    let saved = sqlx::query("update deal_win_backs set email_id = $2, written_at = now() where id = $1 and run_id = $3 and written_at is null")
        .bind(id)
        .bind(email_id)
        .bind(ctx.run.id)
        .execute(&mut *tx)
        .await?;
    if saved.rows_affected() == 0 {
        return Ok(Outcome::new("The win-back is already written"));
    }
    tx.commit().await?;

    if ctx.input::<Input>()?.auto {
        let link = hooks::lead_url(&ctx.state.cfg.public_url, lead.project_id, lead.id);
        let reason = if row.came_back.is_empty() { row.why.trim().to_string() } else { format!("{}.", row.came_back.trim()) };
        let summary = format!("Your lost deal with {} is worth another try. {reason} An email is waiting for your approval.", lead.full_name);
        let alert = ChatAlert { label: "Win-back", summary: &summary, quote: Some(email.body.trim()), link: &link, link_text: "Review the email" };
        hooks::post_chat(db, lead.project_id, &alert).await;
    }
    Ok(Outcome { message: format!("Drafted a win-back to {}", lead.full_name), output: json!({ "email_id": email_id }) })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    #[test]
    fn the_day_is_kept_between_a_month_and_a_year_after_the_loss_and_never_past() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let lost = d("2026-03-01");
        assert_eq!(reach_out_on("2026-03-05", lost, d("2026-03-02")).unwrap(), Some(d("2026-03-31")), "too soon");
        assert_eq!(reach_out_on("2028-01-01", lost, d("2026-03-02")).unwrap(), Some(d("2027-03-01")), "too late");
        assert_eq!(reach_out_on("2026-05-01", lost, d("2026-09-01")).unwrap(), Some(d("2026-09-01")), "a past day is today");
        assert_eq!(reach_out_on(" ", lost, d("2026-03-02")).unwrap(), None);
        assert!(reach_out_on("next spring", lost, d("2026-03-02")).is_err());
    }

    struct Setup {
        project: Uuid,
        lead: Uuid,
        deal: Uuid,
    }

    async fn setup(db: &PgPool, lost_days_ago: i32) -> Setup {
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
        let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Lost', 'lost', 9) returning id")
            .bind(org)
            .fetch_one(db)
            .await
            .unwrap();
        let deal: Uuid = sqlx::query_scalar(
            "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, lost_reason, closed_at, created_at)
             values ($1, $2, $3, $4, 'lost', 960000, 'usd', 'Staying on Gong until the March renewal', now() - make_interval(days => $5), now() - interval '400 days') returning id",
        )
        .bind(org)
        .bind(project)
        .bind(lead)
        .bind(stage)
        .bind(lost_days_ago)
        .fetch_one(db)
        .await
        .unwrap();
        Setup { project, lead, deal }
    }

    async fn run_next(db: &PgPool, answer: Value) -> (Outcome, String) {
        let started = sqlx::query_as::<_, AgentRun>(
            "update agent_runs set status = 'running' where id = (select id from agent_runs where kind = 'win_back' and status = 'queued' limit 1) returning *",
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

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_lost_deal_is_planned_once_brought_forward_when_they_come_back_and_drafted_in_the_thread(db: PgPool) {
        let s = setup(&db, 40).await;
        sqlx::query("insert into deal_reviews (project_id, deal_id, outcome, reason, story, competitor, lesson) values ($1, $2, 'lost', 'timing', 'Liked the trial, but their Gong contract runs to March.', 'Gong', 'Ask about renewal dates in discovery.')")
            .bind(s.project)
            .bind(s.deal)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, to_email, sent_at) values ($1, $2, 'outbound', 'initial', 'Calls in HubSpot', 'Hi', 'sent', 'ines@lumenfold.example', now() - interval '60 days')",
        )
        .bind(s.project)
        .bind(s.lead)
        .execute(&db)
        .await
        .unwrap();

        // A reply after the loss but before the plan is part of the story the plan reads, not a reason to hold back.
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, created_at) values ($1, $2, 'inbound', 'inbound', 'Re: Calls in HubSpot', 'Not a priority this year.', 'received', now() - interval '35 days')",
        )
        .bind(s.project)
        .bind(s.lead)
        .execute(&db)
        .await
        .unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 1, "the lost deal gets a plan");
        assert_eq!(queue_due(&db).await.unwrap(), 0, "once");
        let far = (Utc::now() + chrono::Duration::days(100)).date_naive();
        let (_, prompt) = run_next(&db, json!({ "reach_out_on": far.to_string(), "why": "Their Gong contract renews in March.", "angle": "Calls logged without a recorder." })).await;
        assert!(prompt.contains("Staying on Gong until the March renewal") && prompt.contains("The competitor they chose: Gong"), "{prompt}");
        let (day, why): (Option<NaiveDate>, String) = sqlx::query_as("select reach_out_on, why from deal_win_backs where deal_id = $1").bind(s.deal).fetch_one(&db).await.unwrap();
        assert_eq!((day, why.as_str()), (Some(far), "Their Gong contract renews in March."));
        assert_eq!(queue_due(&db).await.unwrap(), 0, "not due yet");

        sqlx::query("insert into visitor_hits (project_id, ip_hash, domain, path) values ($1, 'x', 'lumenfold.example', '/pricing')").bind(s.project).execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 1, "coming back to our website brings it forward");
        let came_back: String = sqlx::query_scalar("select came_back from deal_win_backs where deal_id = $1").bind(s.deal).fetch_one(&db).await.unwrap();
        assert!(came_back.starts_with("Someone at their company read /pricing on our website on "), "{came_back}");
        let (outcome, prompt) = run_next(&db, json!({ "subject": "New", "body": "Hi Ines, saw your team on our pricing page." })).await;
        assert!(prompt.contains("What brought it forward: Someone at their company read /pricing") && prompt.contains("The plan for the email: Calls logged"), "{prompt}");
        assert!(outcome.message.starts_with("Drafted"), "{}", outcome.message);
        let (kind, status, subject): (String, String, String) =
            sqlx::query_as("select e.kind, e.status, e.subject from deal_win_backs w join email_messages e on e.id = w.email_id where w.deal_id = $1")
                .bind(s.deal)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!((kind.as_str(), status.as_str(), subject.as_str()), ("win_back", "draft", "Re: Calls in HubSpot"));
        assert_eq!(queue_due(&db).await.unwrap(), 0, "written once");

        let item = sqlx::query_as::<_, WinBack>(&format!("{SELECT} where w.deal_id = $1")).bind(s.deal).fetch_one(&db).await.unwrap();
        assert_eq!((item.status.as_str(), item.reason.as_deref()), ("drafted", Some("timing")));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn no_win_back_when_the_agent_says_no_they_wrote_since_or_it_is_switched_off(db: PgPool) {
        let s = setup(&db, 40).await;
        queue_due(&db).await.unwrap();
        let (outcome, _) = run_next(&db, json!({ "reach_out_on": "", "why": "They asked us not to contact them again.", "angle": "" })).await;
        assert_eq!(outcome.message, "No win-back: They asked us not to contact them again.");
        let item = sqlx::query_as::<_, WinBack>(&format!("{SELECT} where w.deal_id = $1")).bind(s.deal).fetch_one(&db).await.unwrap();
        assert_eq!(item.status, "skipped");

        let other = setup(&db, 40).await;
        sqlx::query("update deal_win_backs set reach_out_on = current_date where deal_id = $1").bind(s.deal).execute(&db).await.unwrap();
        sqlx::query("update project_settings set win_back_lost_deals = false where project_id = $1").bind(other.project).execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "a skipped one stays skipped, and a switched-off project gets no plan");

        let run = request(&db, s.deal, s.project, sqlx::query_scalar("select org_id from projects where id = $1").bind(s.project).fetch_one(&db).await.unwrap(), sqlx::query_scalar("select closed_at from deals where id = $1").bind(s.deal).fetch_one(&db).await.unwrap()).await.unwrap();
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, to_email) values ($1, $2, 'inbound', 'inbound', 'Re', 'Back in budget', 'received', 'ines@lumenfold.example')")
            .bind(s.project)
            .bind(s.lead)
            .execute(&db)
            .await
            .unwrap();
        let (outcome, _) = run_next(&db, json!({ "subject": "x", "body": "y" })).await;
        assert!(outcome.message.starts_with("No win-back: they wrote to us since it was planned"), "{}", outcome.message);
        let ran: Uuid = sqlx::query_scalar("select run_id from deal_win_backs where deal_id = $1").bind(s.deal).fetch_one(&db).await.unwrap();
        assert_eq!(ran, run);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_fresh_loss_waits_for_its_own_review_and_write_now_overrides_a_plan_in_flight(db: PgPool) {
        let s = setup(&db, 0).await;
        sqlx::query("insert into deal_reviews (project_id, deal_id, outcome, reason, created_at) values ($1, $2, 'lost', 'price', now() - interval '50 days')")
            .bind(s.project)
            .bind(s.deal)
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "the review is from an earlier loss");
        sqlx::query("update deal_reviews set created_at = now() where deal_id = $1").bind(s.deal).execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 1, "planned once this loss is reviewed");

        let (org, lost_at): (Uuid, Ts) = sqlx::query_as("select org_id, closed_at from deals where id = $1").bind(s.deal).fetch_one(&db).await.unwrap();
        let plan: Uuid = sqlx::query_scalar("select run_id from deal_win_backs where deal_id = $1").bind(s.deal).fetch_one(&db).await.unwrap();
        let write = request(&db, s.deal, s.project, org, lost_at).await.unwrap();
        assert_ne!(write, plan, "writing now doesn't wait on the plan");
        assert_eq!(request(&db, s.deal, s.project, org, lost_at).await.unwrap(), write, "but reuses an email being written");
    }
}
