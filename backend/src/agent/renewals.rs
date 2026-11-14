//! Renewals: a won deal renews on the date the team set, or else on the anniversary of the win, a year at a time. 90 days
//! before the next renewal, the agent reads the account (what they paid, the latest check on whether they might leave, room
//! to grow, the conversation) and writes how likely they are to renew, the steps to get there and an email that opens the
//! renewal conversation, as a draft for the team to approve. Each one is posted to the project's chats.

use std::time::Duration;

use anyhow::Context;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::nudges::NUDGEABLE;
use super::sdk::Call;
use super::write::{WriterSettings, company_signals, draft_in_thread, lead_brief, materials};
use super::{Ctx, Outcome, briefs, kind, meetings};
use crate::deals::amount;
use crate::hooks::{self, ChatAlert};
use crate::models::{Lead, Ts};
use crate::state::AppState;

/// How long before a renewal the agent preps it.
pub const AHEAD_DAYS: i32 = 90;
const PER_PASS: i64 = 20;
const MAX_STEPS: usize = 4;
const OTHERS: i64 = 5;

const WRITER: &str = "You help a B2B sales team renew its customers, for Splee, an outbound sales tool, like a calm, honest \
account manager. Use only what you're given. Never invent facts, numbers, names, usage or quotes, never pressure, never offer \
discounts or promise anything, and never write \"just checking in\", \"circling back\" or \"following up\". Plain text only, \
no markdown.";

/// A won deal's next renewal, with the deal as `d`: its `renews_on`, or else a year after the win, moved on a year at a time
/// until it's today or later. Null for a won deal without a close date. A macro so SELECT can include it.
macro_rules! next_renewal {
    () => {
        "(select min(x)::date from generate_series(
            coalesce(d.renews_on::timestamp, (d.closed_at at time zone 'utc')::date + interval '1 year'),
            coalesce(d.renews_on::timestamp, (d.closed_at at time zone 'utc')::date + interval '1 year') + interval '50 years',
            interval '1 year') x where x >= current_date)"
    };
}
pub const NEXT: &str = next_renewal!();

/// A won deal's next renewal as the Customers page's Renewals tab and a won deal's window show it, with the agent's prep
/// for that date when there is one. `assumed` is true when the date comes from the win, not one the team set. `status` is
/// later (not prepped yet), writing, failed, skipped (the deal isn't won anymore, or the switch was off) or done.
/// `email_status` is the draft's, or null when there's none or it was deleted.
pub const SELECT: &str = concat!(
    "select d.id as deal_id, d.lead_id, l.full_name as lead_name, l.company_name, nullif(u.name, '') as owner_name,
            d.value_cents, d.currency, d.closed_at as won_at, n.renews_on, d.renews_on is null as assumed,
            r.id, r.risk, coalesce(r.why, '') as why, coalesce(r.steps, '[]') as steps,
            case when e.status in ('failed', 'cancelled') then 'The email was ' || e.status else coalesce(r.note, '') end as note,
            coalesce(r.skipped, '') as skipped, r.email_id, e.subject as email_subject, e.body as email_body, e.status as email_status,
            case when r.id is null then 'later' when ar.status in ('queued', 'running') then 'writing' when r.skipped <> '' then 'skipped'
                 when r.written_at is null then 'failed' else 'done' end as status
     from deals d join leads l on l.id = d.lead_id left join users u on u.id = l.owner_id
     cross join lateral (select ",
    next_renewal!(),
    " as renews_on) n
     left join renewals r on r.deal_id = d.id and r.renews_on = n.renews_on
     left join agent_runs ar on ar.id = r.run_id left join email_messages e on e.id = r.email_id"
);

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Renewal {
    pub deal_id: Uuid,
    pub lead_id: Uuid,
    pub lead_name: String,
    pub company_name: String,
    pub owner_name: Option<String>,
    pub value_cents: i64,
    pub currency: String,
    pub won_at: Option<Ts>,
    pub renews_on: Option<NaiveDate>,
    pub assumed: bool,
    pub id: Option<Uuid>,
    pub risk: Option<String>,
    pub why: String,
    pub steps: Value,
    pub note: String,
    pub skipped: String,
    pub email_id: Option<Uuid>,
    pub email_subject: Option<String>,
    pub email_body: Option<String>,
    pub email_status: Option<String>,
    pub status: String,
}

/// Preps each renewal coming up within 90 days that hasn't been, in projects that keep this on. Returns how many runs it
/// queued.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(&format!(
        "with won as (
           select d.id, d.project_id, p.org_id, {NEXT} as renews_on
           from deals d join projects p on p.id = d.project_id and p.org_id is not null
           join project_settings s on s.project_id = d.project_id and s.prep_renewals
           where d.status = 'won'),
         due as (
           insert into renewals (project_id, deal_id, renews_on)
           select w.project_id, w.id, w.renews_on from won w
           where w.renews_on <= current_date + $1
             and not exists (select 1 from renewals r where r.deal_id = w.id and r.renews_on = w.renews_on)
           order by w.renews_on limit $2
           on conflict (deal_id, renews_on) do nothing
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id"
    ))
    .bind(AHEAD_DAYS)
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
    let run = super::enqueue(&mut *tx, kind::RENEWAL, Some(project_id), Some(org_id), json!({ "renewal_id": id, "auto": auto })).await?;
    sqlx::query("update renewals set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

/// Has the agent prep the deal's next renewal now, however far off, replacing a prep for that date and a draft nobody
/// approved. One being written is reused. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    // Takes the deal's lock, so two clicks don't both start one.
    sqlx::query("select 1 from deals where id = $1 for update").bind(deal_id).execute(&mut *tx).await?;
    let renews_on: NaiveDate = sqlx::query_scalar(&format!("select {NEXT} from deals d where d.id = $1")).bind(deal_id).fetch_one(&mut *tx).await?;
    // Only a prep for this date is reused. One for a date the team has since changed runs on, and this one starts.
    let writing: Option<Uuid> = sqlx::query_scalar(
        "select ar.id from renewals r join agent_runs ar on ar.id = r.run_id where r.deal_id = $1 and r.renews_on = $2 and ar.status in ('queued', 'running')",
    )
    .bind(deal_id)
    .bind(renews_on)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(run) = writing {
        tx.commit().await?;
        return Ok(run);
    }
    sqlx::query(
        "delete from email_messages e using renewals r where r.deal_id = $1 and r.renews_on = $2 and e.id = r.email_id and e.status = 'draft'",
    )
    .bind(deal_id)
    .bind(renews_on)
    .execute(&mut *tx)
    .await?;
    sqlx::query("delete from renewals where deal_id = $1 and renews_on = $2").bind(deal_id).bind(renews_on).execute(&mut *tx).await?;
    let id: Uuid = sqlx::query_scalar("insert into renewals (project_id, deal_id, renews_on) values ($1, $2, $3) returning id")
        .bind(project_id)
        .bind(deal_id)
        .bind(renews_on)
        .fetch_one(&mut *tx)
        .await?;
    let run = start(&mut tx, id, project_id, org_id, false).await?;
    tx.commit().await?;
    Ok(run)
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(3600), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "renewals").await });
}

#[derive(Deserialize)]
struct Input {
    renewal_id: Uuid,
    auto: bool,
}

#[derive(sqlx::FromRow)]
struct Row {
    deal_id: Uuid,
    renews_on: NaiveDate,
    status: String,
    closed_at: Option<Ts>,
    lead_id: Uuid,
    value_cents: i64,
    currency: String,
    notes: String,
}

/// What Splee knows about the account beyond the conversation, from the Customers page's sources.
#[derive(sqlx::FromRow)]
struct Account {
    paid_cents: i64,
    payments: i64,
    last_paid_at: Option<Ts>,
    risk: Option<String>,
    risk_why: Option<String>,
    growth: Option<String>,
    referred: i64,
}

#[derive(Deserialize)]
struct Answer {
    risk: String,
    why: String,
    steps: Vec<String>,
    subject: String,
    body: String,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "risk": { "type": "string", "enum": ["high", "medium", "low"] },
            "why": { "type": "string" },
            "steps": { "type": "array", "items": { "type": "string" } },
            "subject": { "type": "string" },
            "body": { "type": "string" }
        },
        "required": ["risk", "why", "steps", "subject", "body"]
    })
}

/// Marks the prep done without reading the account, saying why.
async fn skip(ctx: &Ctx, id: Uuid, why: &str) -> anyhow::Result<Outcome> {
    sqlx::query("update renewals set skipped = $2, written_at = now() where id = $1 and run_id = $3 and written_at is null")
        .bind(id)
        .bind(why)
        .bind(ctx.run.id)
        .execute(ctx.db())
        .await?;
    Ok(Outcome::new(format!("Not prepped: {why}")))
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let id = input.renewal_id;
    let row = sqlx::query_as::<_, Row>(
        "select r.deal_id, r.renews_on, d.status, d.closed_at, d.lead_id, d.value_cents, d.currency, d.notes
         from renewals r join deals d on d.id = r.deal_id where r.id = $1 and r.project_id = $2",
    )
    .bind(id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the renewal was deleted")?;
    let Some(won_at) = row.closed_at.filter(|_| row.status == "won") else {
        return skip(ctx, id, "the deal is no longer won").await;
    };
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(row.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    if input.auto {
        let on: bool = sqlx::query_scalar("select coalesce((select prep_renewals from project_settings where project_id = $1), true)")
            .bind(lead.project_id)
            .fetch_one(db)
            .await?;
        if !on {
            return skip(ctx, id, "prepping renewals was switched off").await;
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

    let account = sqlx::query_as::<_, Account>(
        "select coalesce(sum(py.amount_cents), 0)::bigint as paid_cents, count(*) filter (where py.kind = 'payment') as payments,
                max(py.paid_at) filter (where py.kind = 'payment') as last_paid_at,
                (select case when c.dismissed_at is null then c.risk end from churn_risks c where c.deal_id = $1 and c.risk is not null order by c.created_at desc limit 1) as risk,
                (select case when c.dismissed_at is null then c.why end from churn_risks c where c.deal_id = $1 and c.risk is not null order by c.created_at desc limit 1) as risk_why,
                (select x.why from deal_expansions x where x.deal_id = $1 and x.dismissed_at is null and x.why <> '' order by x.created_at desc limit 1) as growth,
                (select count(*) from leads rl join deal_referrals f on f.id = rl.referral_id where f.deal_id = $1) as referred
         from leads l left join leads pl on pl.project_id = l.project_id and (pl.id = l.id or pl.company_domain = nullif(l.company_domain, ''))
         left join payments py on py.lead_id = pl.id and py.project_id = l.project_id and py.paid_at > now() - interval '12 months'
                              and lower(py.currency) = lower($3)
         where l.id = $2",
    )
    .bind(row.deal_id)
    .bind(lead.id)
    .bind(&row.currency)
    .fetch_one(db)
    .await?;
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
    let us = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) };
    let cur = row.currency.to_uppercase();
    let days = (row.renews_on - chrono::Utc::now().date_naive()).num_days();

    let mut facts = vec![format!(
        "They became a customer on {}{}. They renew on {}, in {days} days.",
        won_at.format("%-d %b %Y"),
        if row.value_cents > 0 { format!(", worth {} {cur}", amount(row.value_cents)) } else { String::new() },
        row.renews_on.format("%-d %b %Y"),
    )];
    if account.payments > 0 {
        facts.push(format!(
            "In the last 12 months they paid {} {cur} over {} payments, the last on {}.",
            amount(account.paid_cents),
            account.payments,
            account.last_paid_at.map(|t| t.format("%-d %b").to_string()).unwrap_or_default()
        ));
    }
    if !row.notes.trim().is_empty() {
        facts.push(format!("The deal's notes: {}", row.notes.trim()));
    }
    if let Some((reason, story)) = &review {
        facts.push(format!("Why they bought: {}. {}", reason.as_deref().unwrap_or("").replace('_', " "), story.trim()));
    }
    if let (Some(risk), Some(why)) = (&account.risk, &account.risk_why) {
        facts.push(format!("Our latest check on whether they might leave: {risk} risk. {why}"));
    }
    if let Some(growth) = &account.growth {
        facts.push(format!("Room to grow we noticed: {growth}"));
    }
    if account.referred > 0 {
        facts.push(format!("They referred {} people to us.", account.referred));
    }
    if !others.is_empty() {
        let names: Vec<String> = others.iter().map(|(name, title)| if title.is_empty() { name.clone() } else { format!("{name} ({title})") }).collect();
        facts.push(format!("Others we know there: {}.", names.join(", ")));
    }
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    let email = match &blocker {
        Some(reason) => format!("Write no email, leave subject and body empty: {reason}."),
        None => format!(
            "A 60 to 110 word email from {us} to {first} that opens the renewal conversation: one sentence on what they got from us, \
             only from the facts above, then a short call to plan the next year, around what they care about. Never mention \
             prices, payments or the risk check. No sign-off. {}",
            crate::language::instruction(lang)
        ),
    };
    let prompt = format!(
        "{who} bought from us.\n{facts}\n\n\
         What we sell: {name}. {description}\nOur materials:\n{materials}\n\n\
         The lead and their company now:\n{about}\n\nThe conversation and what else Splee knows:\n{context}\n\n\
         Prepare the team for this renewal.\n\
         1. risk: high when the evidence says they may not renew, medium when there's something to fix first, low when \
         they look set to renew.\n\
         2. why: under 25 words, the evidence.\n\
         3. steps: 2 to {MAX_STEPS} steps to a signed renewal by {date}, in order, each under 20 words, naming who to talk to and \
         by when, counting back from the renewal date.\n\
         4. subject: a short subject. It goes in the existing thread when there is one.\n\
         5. body: {email}",
        who = [lead.full_name.trim(), lead.company_name.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" at "),
        facts = facts.join("\n"),
        name = profile.name,
        description = profile.description,
        materials = if materials.is_empty() { "(none)".to_string() } else { materials },
        about = serde_json::to_string_pretty(&about)?,
        date = row.renews_on.format("%-d %b %Y"),
    );
    let company = if lead.company_name.is_empty() { lead.full_name.clone() } else { lead.company_name.clone() };
    ctx.log(format!("prepping {company}'s renewal…")).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.3, ..Call::writing(WRITER, prompt, schema()) };
    let answer: Answer = ctx.ask(&call).await?;
    let risk = match answer.risk.as_str() {
        "high" | "medium" => answer.risk.as_str(),
        _ => "low",
    };
    let steps: Vec<String> = answer.steps.iter().map(|s| s.trim().chars().take(200).collect::<String>()).filter(|s| !s.is_empty()).take(MAX_STEPS).collect();
    let body = answer.body.trim();
    let write = blocker.is_none() && !body.is_empty();
    let note = blocker.as_deref().unwrap_or_default();

    let mut tx = db.begin().await?;
    let email_id = if write { Some(draft_in_thread(&mut tx, &lead, "retention", &answer.subject, &settings.sign(body), lang.code).await?) } else { None };
    let saved = sqlx::query(
        "update renewals set risk = $2, why = $3, steps = $4, note = $5, email_id = $6, written_at = now()
         where id = $1 and run_id = $7 and written_at is null",
    )
    .bind(id)
    .bind(risk)
    .bind(answer.why.trim())
    .bind(json!(steps))
    .bind(note)
    .bind(email_id)
    .bind(ctx.run.id)
    .execute(&mut *tx)
    .await?;
    if saved.rows_affected() == 0 {
        return Ok(Outcome::new("The renewal is already prepped"));
    }
    tx.commit().await?;

    if input.auto {
        let link = format!("{}/app/customers?view=renewals&deal={}", ctx.state.cfg.public_url, row.deal_id);
        let worth = if row.value_cents > 0 { format!(", worth {} {cur}", amount(row.value_cents)) } else { String::new() };
        let label = match risk {
            "high" => "At risk",
            "medium" => "Something to fix first",
            _ => "Looks set to renew",
        };
        let summary = format!("{company} renews on {} in {days} days{worth}. {label}: {}", row.renews_on.format("%-d %b"), answer.why.trim());
        let (quote, link_text) = if write { (Some(body), "Review the email") } else { (None, "Open the renewal") };
        let alert = ChatAlert { label: "Renewal coming up", summary: &summary, quote, link: &link, link_text };
        hooks::post_chat(db, lead.project_id, &alert).await;
    }
    let what = if write { "with an email" } else { "without an email" };
    Ok(Outcome { message: format!("Prepped {company}'s renewal at {risk} risk, {what}"), output: json!({ "risk": risk, "email_id": email_id }) })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    async fn next(db: &PgPool, deal: Uuid) -> Option<NaiveDate> {
        sqlx::query_scalar(&format!("select {NEXT} from deals d where d.id = $1")).bind(deal).fetch_one(db).await.unwrap()
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_customer_is_prepped_once_90_days_before_the_anniversary_of_the_win_or_the_date_the_team_set(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(&db).await.unwrap();
        let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Won', 'won', 8) returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        let mut deals = vec![];
        // Won 300 days ago, so it renews in about 65 days. Won 100 days ago, renewing in about 265. Won two and a half years
        // ago, so last year's anniversary passed and the next is in about 185 days.
        for (name, domain, days_ago) in [("Ines Duarte", "lumenfold.example", 300), ("Noor Haddad", "quill.example", 100), ("Owen Fairley", "tidewell.example", 910)] {
            let lead: Uuid = sqlx::query_scalar(
                "insert into leads (project_id, full_name, first_name, company_name, company_domain, email)
                 values ($1, $2, split_part($2, ' ', 1), initcap(split_part($3, '.', 1)), $3, 'buyer@' || $3) returning id",
            )
            .bind(project)
            .bind(name)
            .bind(domain)
            .fetch_one(&db)
            .await
            .unwrap();
            let deal: Uuid = sqlx::query_scalar(
                "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, closed_at)
                 values ($1, $2, $3, $4, 'won', 960000, 'usd', now() - make_interval(days => $5)) returning id",
            )
            .bind(org)
            .bind(project)
            .bind(lead)
            .bind(stage)
            .bind(days_ago)
            .fetch_one(&db)
            .await
            .unwrap();
            deals.push((deal, lead));
        }
        let today = chrono::Utc::now().date_naive();
        let (lumenfold, quill, tidewell) = (deals[0].0, deals[1].0, deals[2].0);
        let days = |d: Option<NaiveDate>| (d.unwrap() - today).num_days();
        assert!((64..=66).contains(&days(next(&db, lumenfold).await)));
        assert!((264..=266).contains(&days(next(&db, quill).await)));
        assert!((183..=187).contains(&days(next(&db, tidewell).await)), "rolled on past last year's");

        // The team says Quill renews in 30 days instead, so both are due now.
        sqlx::query("update deals set renews_on = current_date + 30 where id = $1").bind(quill).execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 2);
        assert_eq!(queue_due(&db).await.unwrap(), 0, "once per renewal");

        sqlx::query(
            "insert into payments (org_id, project_id, lead_id, provider, external_id, kind, amount_cents, currency, paid_at)
             values ($1, $2, $3, 'stripe', 'ch_1', 'payment', 80000, 'usd', now() - interval '20 days')",
        )
        .bind(org)
        .bind(project)
        .bind(deals[0].1)
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("insert into churn_risks (project_id, deal_id, signal, risk, why, written_at) values ($1, $2, '', 'medium', 'Their champion went quiet.', now())")
            .bind(project)
            .bind(lumenfold)
            .execute(&db)
            .await
            .unwrap();
        let run_id: Uuid = sqlx::query_scalar("select r.run_id from renewals r where r.deal_id = $1").bind(lumenfold).fetch_one(&db).await.unwrap();
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where id = $1 returning *").bind(run_id).fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(run_id, |_| {
            json!({ "risk": "sure", "why": "Paying on time, but the champion went quiet.", "steps": ["Call Ines", " ", "Find a second contact", "Send the review", "Agree terms", "Sign"],
                    "subject": "The year ahead", "body": "Hi Ines, could we plan next year?" })
        });
        run(&ctx).await.unwrap();
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(run_id).execute(&db).await.unwrap();
        let prompt = super::super::fake::prompts(run_id).first().cloned().unwrap();
        assert!(prompt.contains("In the last 12 months they paid 800 USD over 1 payments") && prompt.contains("medium risk. Their champion went quiet."), "{prompt}");
        assert!(prompt.contains(", worth 9,600 USD. They renew on"), "{prompt}");

        let r = sqlx::query_as::<_, Renewal>(&format!("{SELECT} where d.id = $1")).bind(lumenfold).fetch_one(&db).await.unwrap();
        assert_eq!((r.status.as_str(), r.risk.as_deref(), r.assumed, r.email_status.as_deref()), ("done", Some("low"), true, Some("draft")), "an unknown risk is low");
        assert_eq!(r.steps, json!(["Call Ines", "Find a second contact", "Send the review", "Agree terms"]));
        let q = sqlx::query_as::<_, Renewal>(&format!("{SELECT} where d.id = $1")).bind(quill).fetch_one(&db).await.unwrap();
        assert_eq!((q.status.as_str(), q.assumed), ("writing", false));

        // Prepping again replaces the draft nobody approved.
        let draft = r.email_id.unwrap();
        request(&db, lumenfold, project, org).await.unwrap();
        let gone: bool = sqlx::query_scalar("select not exists (select 1 from email_messages where id = $1)").bind(draft).fetch_one(&db).await.unwrap();
        assert!(gone);

        // The team moves the date while that prep is queued, so prepping now starts one for the new date.
        let queued = request(&db, lumenfold, project, org).await.unwrap();
        sqlx::query("update deals set renews_on = current_date + 20 where id = $1").bind(lumenfold).execute(&db).await.unwrap();
        let moved = request(&db, lumenfold, project, org).await.unwrap();
        assert_ne!(queued, moved);
        let r = sqlx::query_as::<_, Renewal>(&format!("{SELECT} where d.id = $1")).bind(lumenfold).fetch_one(&db).await.unwrap();
        assert_eq!((r.status.as_str(), r.renews_on), ("writing", Some(chrono::Utc::now().date_naive() + chrono::Days::new(20))));
    }
}
