//! Customers at risk: the agent watches every won deal for warning signs. The person who bought leaving the company, a
//! refund, payments that stopped coming, or a reply read as not interested or unsubscribing has it read the account, say
//! how likely the customer is to leave and what the team should do, and draft an email in the thread when one would help.
//! A high risk found on its own is posted to the project's chats.

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
/// Between two looks at one customer, and how old a sign may be when it's first noticed.
const EVERY_DAYS: i32 = 7;
const SINCE_DAYS: i32 = 30;
/// Others at the customer the prompt names, for when the buyer left.
const OTHERS: i64 = 5;

const WRITER: &str = "You help a B2B sales team keep its customers, for Splee, an outbound sales tool, like a calm, honest \
account manager. Use only what you're given. Never invent facts, numbers, names, usage or quotes, never guilt or pressure, \
never offer discounts or promise anything, and never write \"just checking in\", \"circling back\" or \"following up\". \
Plain text only, no markdown.";

/// A look as the Customers page's At risk tab and a won deal's window show it. `status` is writing, failed, skipped (the deal
/// isn't won anymore, or the switch was off) or done. `email_status` is the draft's, or null when there's none or it was
/// deleted.
pub const SELECT: &str = "select c.id, c.deal_id, d.lead_id, l.full_name as lead_name, l.company_name,
        nullif(u.name, '') as owner_name, d.value_cents, d.currency, d.closed_at as won_at, c.signal, c.risk, c.why, c.play,
        case when e.status in ('failed', 'cancelled') then 'The email was ' || e.status else c.note end as note,
        c.skipped, c.email_id, e.subject as email_subject, e.body as email_body, e.status as email_status,
        case when r.status in ('queued', 'running') then 'writing' when c.skipped <> '' then 'skipped'
             when c.written_at is null then 'failed' else 'done' end as status,
        c.created_at
     from churn_risks c join deals d on d.id = c.deal_id join leads l on l.id = d.lead_id
     left join users u on u.id = l.owner_id left join agent_runs r on r.id = c.run_id
     left join email_messages e on e.id = c.email_id";

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Risk {
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
    pub risk: Option<String>,
    pub why: String,
    pub play: String,
    pub note: String,
    pub skipped: String,
    pub email_id: Option<Uuid>,
    pub email_subject: Option<String>,
    pub email_body: Option<String>,
    pub email_status: Option<String>,
    pub status: String,
    pub created_at: Ts,
}

/// The warning signs at a won deal noticed after `w.since`, as up to four sentences, from `w` with `project_id`, `lead_id`
/// and `company_domain`: the newest move of the buyer or another buyer there to a new company, the newest refund, payments
/// that came regularly and stopped, and the newest reply from someone there read as not interested or unsubscribing.
/// Empty when there's none. Someone at the customer is the deal's lead or anyone with its company's domain.
const SIGNAL: &str = "concat_ws(' ',
    (select wp.full_name || ' left ' || coalesce(nullif(jc.old_company, ''), 'the company') || ' for ' || jc.new_company
            || case when jc.new_title <> '' then ' as ' || jc.new_title else '' end || ' (seen ' || to_char(jc.seen_at, 'FMDD Mon') || ').'
     from job_changes jc join watched_people wp on wp.id = jc.watched_id
     where jc.project_id = w.project_id and jc.seen_at > w.since
       and (wp.lead_id = w.lead_id or (wp.relationship = 'won' and jc.old_domain = w.company_domain))
     order by jc.seen_at desc limit 1),
    (select 'They got a refund of ' || to_char(-py.amount_cents / 100.0, 'FM999G999G990D00') || ' ' || upper(py.currency)
            || ' on ' || to_char(py.paid_at, 'FMDD Mon') || '.'
     from payments py join leads pl on pl.id = py.lead_id
     where py.project_id = w.project_id and py.kind = 'refund' and py.created_at > w.since
       and (pl.id = w.lead_id or pl.company_domain = w.company_domain)
     order by py.paid_at desc limit 1),
    (select 'They paid about every ' || round(s.gap) || ' days, but nothing has come in since ' || to_char(s.last, 'FMDD Mon') || '.'
     from (select max(py.paid_at) as last, count(*) as n,
                  extract(epoch from max(py.paid_at) - min(py.paid_at)) / 86400 / greatest(count(*) - 1, 1) as gap
           from payments py join leads pl on pl.id = py.lead_id
           where py.project_id = w.project_id and py.kind = 'payment' and py.paid_at > now() - interval '400 days'
             and (pl.id = w.lead_id or pl.company_domain = w.company_domain)) s
     where s.n >= 3 and s.gap >= 20 and s.last + make_interval(secs => s.gap * 1.5 * 86400) between w.since and now()),
    (select coalesce(nullif(el.first_name, ''), el.full_name) || ' wrote on ' || to_char(e.created_at, 'FMDD Mon')
            || ', read as ' || replace(e.classification, '_', ' ')
            || case when e.classification_reason <> '' then ': ' || rtrim(e.classification_reason, '.') else '' end || '.'
     from email_messages e join leads el on el.id = e.lead_id
     where e.project_id = w.project_id and e.direction = 'inbound' and e.classification in ('not_interested', 'unsubscribe')
       and e.created_at > w.since and (el.id = w.lead_id or el.company_domain = w.company_domain)
     order by e.created_at desc limit 1))";

/// Looks at each customer with a warning sign since the last look, at most once a week each, in projects that keep this
/// on. Returns how many runs it queued.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(&format!(
        "with won as (
           select d.id, d.project_id, d.lead_id, l.company_domain,
                  greatest(d.closed_at, now() - make_interval(days => $2),
                           (select max(c.created_at) from churn_risks c where c.deal_id = d.id)) as since
           from deals d join leads l on l.id = d.lead_id
           join projects p on p.id = d.project_id and p.org_id is not null
           join project_settings s on s.project_id = d.project_id and s.watch_churn
           where d.status = 'won' and d.closed_at is not null
             and not exists (select 1 from churn_risks c where c.deal_id = d.id and c.created_at > now() - make_interval(days => $1))),
         signals as (
           select w.id, w.project_id, {SIGNAL} as signal
           from won w),
         due as (
           insert into churn_risks (project_id, deal_id, signal)
           select project_id, id, signal from signals where signal <> '' limit $3
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id"
    ))
    .bind(EVERY_DAYS)
    .bind(SINCE_DAYS)
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
    let run = super::enqueue(&mut *tx, kind::CHURN_RISK, Some(project_id), Some(org_id), json!({ "risk_id": id, "auto": auto })).await?;
    sqlx::query("update churn_risks set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

/// Has the agent check on the customer now, from their deal's window, replacing a draft nobody approved. One being
/// written is reused. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    // Takes the deal's lock, so two clicks don't both start one.
    sqlx::query("select 1 from deals where id = $1 for update").bind(deal_id).execute(&mut *tx).await?;
    let writing: Option<Uuid> =
        sqlx::query_scalar("select r.id from churn_risks c join agent_runs r on r.id = c.run_id where c.deal_id = $1 and r.status in ('queued', 'running')")
            .bind(deal_id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(run) = writing {
        tx.commit().await?;
        return Ok(run);
    }
    sqlx::query("delete from email_messages e using churn_risks c where c.deal_id = $1 and e.id = c.email_id and e.status = 'draft'")
        .bind(deal_id)
        .execute(&mut *tx)
        .await?;
    // Reads the signs of the last 30 days, even ones the last look read, since checking again on a draft comes here too.
    let id: Uuid = sqlx::query_scalar(&format!(
        "insert into churn_risks (project_id, deal_id, signal)
         select w.project_id, w.id, {SIGNAL} from (
           select d.id, $1::uuid as project_id, d.lead_id, l.company_domain, greatest(d.closed_at, now() - make_interval(days => $3)) as since
           from deals d join leads l on l.id = d.lead_id where d.id = $2) w
         returning id"
    ))
    .bind(project_id)
    .bind(deal_id)
    .bind(SINCE_DAYS)
    .fetch_one(&mut *tx)
    .await?;
    let run = start(&mut tx, id, project_id, org_id, false).await?;
    tx.commit().await?;
    Ok(run)
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(3600), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "churn").await });
}

#[derive(Deserialize)]
struct Input {
    risk_id: Uuid,
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
}

#[derive(Deserialize)]
struct Answer {
    risk: String,
    why: String,
    play: String,
    subject: String,
    body: String,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "risk": { "type": "string", "enum": ["high", "medium", "low"] },
            "why": { "type": "string" },
            "play": { "type": "string" },
            "subject": { "type": "string" },
            "body": { "type": "string" }
        },
        "required": ["risk", "why", "play", "subject", "body"]
    })
}

/// Marks the look done without reading the account, saying why.
async fn skip(ctx: &Ctx, id: Uuid, why: &str) -> anyhow::Result<Outcome> {
    sqlx::query("update churn_risks set skipped = $2, written_at = now() where id = $1 and run_id = $3 and written_at is null")
        .bind(id)
        .bind(why)
        .bind(ctx.run.id)
        .execute(ctx.db())
        .await?;
    Ok(Outcome::new(format!("Not checked: {why}")))
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let row = sqlx::query_as::<_, Row>(
        "select c.deal_id, c.signal, d.status, d.closed_at, d.lead_id, d.value_cents, d.currency, d.notes
         from churn_risks c join deals d on d.id = c.deal_id where c.id = $1 and c.project_id = $2",
    )
    .bind(input.risk_id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the check was deleted")?;
    let id = input.risk_id;
    let Some(won_at) = row.closed_at.filter(|_| row.status == "won") else {
        return skip(ctx, id, "the deal is no longer won").await;
    };
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(row.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    if input.auto {
        let on: bool = sqlx::query_scalar("select coalesce((select watch_churn from project_settings where project_id = $1), true)")
            .bind(lead.project_id)
            .fetch_one(db)
            .await?;
        if !on {
            return skip(ctx, id, "watching customers was switched off").await;
        }
    }
    // Why there can't be an email, read before the call so the agent doesn't write one for nothing.
    let blocker = match meetings::follow_up_blocker(&mut *db.acquire().await?, org_id, &lead).await? {
        Some(blocker) => Some(format!("{} can't be emailed: {blocker}", lead.full_name)),
        None => {
            let waiting: bool = sqlx::query_scalar(&format!("select not ({NUDGEABLE}) from leads l where l.id = $1")).bind(lead.id).fetch_one(db).await?;
            waiting.then(|| "An email to them is already waiting to go out, or a call is booked".to_string())
        }
    };

    let review: Option<(Option<String>, String)> =
        sqlx::query_as("select reason, story from deal_reviews where deal_id = $1 and outcome = 'won' and created_at >= $2")
            .bind(row.deal_id)
            .bind(won_at)
            .fetch_optional(db)
            .await?;
    let others: Vec<(String, String)> = sqlx::query_as(
        "select full_name, title from leads where project_id = $1 and id <> $2 and company_domain = $3 and status not in ('bounced', 'do_not_contact')
         order by created_at desc limit $4",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(&lead.company_domain)
    .bind(OTHERS)
    .fetch_all(db)
    .await?;
    let settings = WriterSettings::load(ctx, lead.project_id).await?;
    let lang = crate::language::for_lead(db, lead.id).await?;
    let signals = company_signals(ctx, &[&lead]).await?;
    let about = lead_brief(&lead, &signals);
    let context = briefs::gather(db, &lead, org_id).await?;
    let materials = materials(ctx, lead.project_id).await?;
    let us = settings.sender_at(&profile.name);
    let mut deal = vec![format!(
        "They became a customer on {}{}.",
        won_at.format("%-d %b %Y"),
        if row.value_cents > 0 { format!(", worth {} {}", row.value_cents / 100, row.currency.to_uppercase()) } else { String::new() },
    )];
    if !row.notes.trim().is_empty() {
        deal.push(format!("The deal's notes: {}", row.notes.trim()));
    }
    if let Some((reason, story)) = &review {
        deal.push(format!("Why they bought: {}. {}", reason.as_deref().unwrap_or("").replace('_', " "), story.trim()));
    }
    if !others.is_empty() {
        let names: Vec<String> = others.iter().map(|(name, title)| if title.is_empty() { name.clone() } else { format!("{name} ({title})") }).collect();
        deal.push(format!("Others we know there: {}.", names.join(", ")));
    }
    let email = match &blocker {
        Some(reason) => format!("Write no email, leave subject and body empty: {reason}."),
        None => format!(
            "If the risk isn't low and an email from {us} to {first} would help, a 50 to 100 word one. Answer what they raised, or \
             if they wrote nothing, open with something useful about how they use us. Offer one concrete next step, like a short \
             call. Never mention refunds, payments or that someone left unless they told us themselves, and never ask why they \
             might leave. No sign-off. {language} Empty when the person who bought is the one who left, or an email wouldn't help.",
            first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name },
            language = crate::language::instruction(lang),
        ),
    };
    let prompt = format!(
        "{who} bought from us.\n{deal}\n\n{signal}\n\n\
         What we sell: {name}. {description}\nOur materials:\n{materials}\n\n\
         The lead and their company now:\n{about}\n\nThe conversation and what else Splee knows:\n{context}\n\n\
         Decide how likely this customer is to leave us, and what the team should do.\n\
         1. risk: high when the evidence says they're leaving or likely to, medium when there's a real warning sign to act on, \
         low when the signs are explained or harmless, like a refund for a billing mistake or someone leaving who wasn't the \
         buyer while the rest of the conversation is warm.\n\
         2. why: under 25 words, the evidence.\n\
         3. play: under 40 words, what the team should do this week, naming who to talk to. When the buyer left, who to reach \
         instead, from the others we know there or the role to find.\n\
         4. subject: a short subject. It goes in the existing thread when there is one.\n\
         5. body: {email}",
        who = [lead.full_name.trim(), lead.company_name.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" at "),
        deal = deal.join("\n"),
        signal = if row.signal.is_empty() {
            "Our team asked you to check on this customer now.".to_string()
        } else {
            format!("Warning signs we noticed lately: {}", row.signal)
        },
        name = profile.name,
        description = profile.description,
        materials = if materials.is_empty() { "(none)".to_string() } else { materials },
        about = serde_json::to_string_pretty(&about)?,
    );
    ctx.log(format!("checking on {}…", if lead.company_name.is_empty() { &lead.full_name } else { &lead.company_name })).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.3, ..Call::writing(WRITER, prompt, schema()) };
    let answer: Answer = ctx.ask(&call).await?;
    let risk = match answer.risk.as_str() {
        "high" | "medium" => answer.risk.as_str(),
        _ => "low",
    };
    let body = answer.body.trim();
    let write = risk != "low" && blocker.is_none() && !body.is_empty();
    let note = match &blocker {
        Some(reason) if risk != "low" => reason.as_str(),
        _ => "",
    };

    let mut tx = db.begin().await?;
    let email_id = if write { Some(draft_in_thread(&mut tx, &lead, "retention", &answer.subject, &settings.sign(body), lang.code).await?) } else { None };
    let saved = sqlx::query(
        "update churn_risks set risk = $2, why = $3, play = $4, note = $5, email_id = $6, written_at = now()
         where id = $1 and run_id = $7 and written_at is null",
    )
    .bind(id)
    .bind(risk)
    .bind(answer.why.trim())
    .bind(answer.play.trim())
    .bind(note)
    .bind(email_id)
    .bind(ctx.run.id)
    .execute(&mut *tx)
    .await?;
    if saved.rows_affected() == 0 {
        return Ok(Outcome::new("The check is already done"));
    }
    tx.commit().await?;

    if input.auto && risk == "high" {
        let link = hooks::lead_url(&ctx.state.cfg.public_url, lead.project_id, lead.id);
        let who = if lead.company_name.is_empty() { &lead.full_name } else { &lead.company_name };
        let summary = format!("{who} may be about to leave. {} {}", answer.why.trim(), answer.play.trim());
        let (quote, link_text) = if write { (Some(body), "Review the email") } else { (None, "Open the lead") };
        let alert = ChatAlert { label: "Customer at risk", summary: &summary, quote, link: &link, link_text };
        hooks::post_chat(db, lead.project_id, &alert).await;
    }
    let what = if write { "with an email" } else { "without an email" };
    Ok(Outcome { message: format!("{} is at {risk} risk, {what}", lead.full_name), output: json!({ "risk": risk, "email_id": email_id }) })
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

    async fn setup(db: &PgPool) -> Setup {
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
             values ($1, $2, $3, $4, 'won', 480000, 'usd', '4 seats for the SDR team', now() - interval '120 days', now() - interval '200 days') returning id",
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

    async fn run_next(db: &PgPool, answer: Value) -> String {
        let started = sqlx::query_as::<_, AgentRun>(
            "update agent_runs set status = 'running' where id = (select id from agent_runs where kind = 'churn_risk' and status = 'queued' limit 1) returning *",
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

    async fn latest(db: &PgPool, deal: Uuid) -> Risk {
        sqlx::query_as::<_, Risk>(&format!("{SELECT} where c.deal_id = $1 order by c.created_at desc limit 1")).bind(deal).fetch_one(db).await.unwrap()
    }

    async fn pay(db: &PgPool, s: &Setup, id: &str, kind: &str, cents: i64, days_ago: i32) {
        sqlx::query(
            "insert into payments (org_id, project_id, lead_id, provider, external_id, kind, amount_cents, currency, paid_at)
             values ($1, $2, $3, 'stripe', $4, $5, $6, 'usd', now() - make_interval(days => $7))",
        )
        .bind(s.org)
        .bind(s.project)
        .bind(s.lead)
        .bind(id)
        .bind(kind)
        .bind(cents)
        .bind(days_ago)
        .execute(db)
        .await
        .unwrap();
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn payments_that_stop_and_an_unhappy_reply_get_one_check_a_week_with_a_draft(db: PgPool) {
        let s = setup(&db).await;
        for (n, days) in [100, 70, 40].iter().enumerate() {
            pay(&db, &s, &format!("ch_{n}"), "payment", 40000, *days).await;
        }
        assert_eq!(queue_due(&db).await.unwrap(), 0, "the next payment isn't late yet");

        // Paid every 30 days, the last 60 days ago, so it's been late for 15 days.
        sqlx::query("update payments set paid_at = paid_at - interval '20 days'").execute(&db).await.unwrap();
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, classification, classification_reason)
             values ($1, $2, 'inbound', 'inbound', 'Re: Calls in HubSpot', 'We are looking at other options.', 'received', 'not_interested', 'They are looking at other options.')",
        )
        .bind(s.project)
        .bind(s.lead)
        .execute(&db)
        .await
        .unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 1);
        assert_eq!(queue_due(&db).await.unwrap(), 0, "once a week");
        let prompt = run_next(
            &db,
            json!({ "risk": "high", "why": "They're looking at other tools and stopped paying.", "play": "Call Ines this week.", "subject": "Re: Calls in HubSpot", "body": "Hi Ines, happy to help." }),
        )
        .await;
        assert!(
            prompt.contains("They paid about every 30 days, but nothing has come in since") && prompt.contains("Ines wrote on") && prompt.contains("read as not interested: They are looking at other options."),
            "{prompt}"
        );
        let r = latest(&db, s.deal).await;
        assert_eq!((r.status.as_str(), r.risk.as_deref(), r.email_status.as_deref(), r.play.as_str()), ("done", Some("high"), Some("draft"), "Call Ines this week."));
        let kind: String = sqlx::query_scalar("select kind from email_messages where id = $1").bind(r.email_id).fetch_one(&db).await.unwrap();
        assert_eq!(kind, "retention");

        // A week on, only what's new counts: nothing is, so there's no check.
        sqlx::query("update churn_risks set created_at = now() - interval '8 days'").execute(&db).await.unwrap();
        sqlx::query("update email_messages set created_at = now() - interval '9 days'").execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "the signs were already read");
        pay(&db, &s, "re_1", "refund", -40000, 1).await;
        assert_eq!(queue_due(&db).await.unwrap(), 1, "a new refund");
        let r = latest(&db, s.deal).await;
        assert!(r.signal.starts_with("They got a refund of 400.00 USD on"), "{}", r.signal);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn when_the_buyer_leaves_there_is_no_email_and_a_low_risk_or_a_switched_off_project_drafts_nothing(db: PgPool) {
        let s = setup(&db).await;
        let watched: Uuid = sqlx::query_scalar(
            "insert into watched_people (project_id, lead_id, relationship, full_name, company_name, company_domain) values ($1, $2, 'won', 'Ines Duarte', 'Lumenfold', 'lumenfold.example') returning id",
        )
        .bind(s.project)
        .bind(s.lead)
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query("insert into job_changes (project_id, watched_id, old_company, old_domain, new_company, new_title, move_number) values ($1, $2, 'Lumenfold', 'lumenfold.example', 'Brightwave', 'VP Sales', 1)")
            .bind(s.project)
            .bind(watched)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into leads (project_id, full_name, title, company_domain) values ($1, 'Tomas Berg', 'Head of Sales', 'lumenfold.example')")
            .bind(s.project)
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 1);
        let prompt = run_next(&db, json!({ "risk": "high", "why": "Ines left.", "play": "Reach Tomas Berg.", "subject": "", "body": "" })).await;
        assert!(prompt.contains("Ines Duarte left Lumenfold for Brightwave as VP Sales") && prompt.contains("Others we know there: Tomas Berg (Head of Sales)."), "{prompt}");
        let r = latest(&db, s.deal).await;
        assert_eq!((r.risk.as_deref(), r.email_id, r.play.as_str()), (Some("high"), None, "Reach Tomas Berg."));

        // Asked by the team with a draft waiting, the risk is read but no second email is written.
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, subject, body, status) values ($1, $2, 'outbound', 'manual_reply', 'Hi', 'Hi', 'draft')")
            .bind(s.project)
            .bind(s.lead)
            .execute(&db)
            .await
            .unwrap();
        let first = request(&db, s.deal, s.project, s.org).await.unwrap();
        assert_eq!(request(&db, s.deal, s.project, s.org).await.unwrap(), first, "one being written is reused");
        let prompt = run_next(&db, json!({ "risk": "medium", "why": "Ines left.", "play": "Reach Tomas.", "subject": "Hi", "body": "Hi Ines" })).await;
        assert!(prompt.contains("Write no email"), "{prompt}");
        let r = latest(&db, s.deal).await;
        assert_eq!((r.risk.as_deref(), r.email_id, r.note.as_str()), (Some("medium"), None, "An email to them is already waiting to go out, or a call is booked"));

        sqlx::query("delete from email_messages").execute(&db).await.unwrap();
        request(&db, s.deal, s.project, s.org).await.unwrap();
        run_next(&db, json!({ "risk": "low", "why": "All fine.", "play": "Nothing to do.", "subject": "Hi", "body": "Hi Ines" })).await;
        let r = latest(&db, s.deal).await;
        assert_eq!((r.risk.as_deref(), r.email_id, r.note.as_str()), (Some("low"), None, ""), "a low risk writes nothing");

        sqlx::query("update project_settings set watch_churn = false").execute(&db).await.unwrap();
        let id: Uuid = sqlx::query_scalar("insert into churn_risks (project_id, deal_id) values ($1, $2) returning id").bind(s.project).bind(s.deal).fetch_one(&db).await.unwrap();
        let mut tx = db.begin().await.unwrap();
        start(&mut tx, id, s.project, s.org, true).await.unwrap();
        tx.commit().await.unwrap();
        run_next(&db, json!({ "risk": "high", "why": "", "play": "", "subject": "", "body": "Hi" })).await;
        let r = latest(&db, s.deal).await;
        assert_eq!((r.status.as_str(), r.skipped.as_str()), ("skipped", "watching customers was switched off"));
        sqlx::query("update churn_risks set created_at = now() - interval '8 days'").execute(&db).await.unwrap();
        sqlx::query("update job_changes set seen_at = now()").execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "switched off");
    }
}
