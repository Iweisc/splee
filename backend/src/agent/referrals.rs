//! Referrals: about six weeks after a deal is won, the agent reads whether the buyer is happy. If they are, it picks up to
//! three companies like theirs from the company database and drafts an email asking whether they know someone there, with
//! a page of their own to forward (/r/{token}). Whoever leaves their details on that page becomes a lead credited to the
//! customer, and the project's chats hear about it.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::types::Json;
use uuid::Uuid;

use super::explore::load_project;
use super::nudges::NUDGEABLE;
use super::sdk::Call;
use super::write::{WriterSettings, draft_in_thread};
use super::{Ctx, Outcome, briefs, kind, meetings};
use crate::hooks::{self, ChatAlert};
use crate::models::{Lead, Ts};
use crate::state::AppState;

const PER_PASS: i64 = 20;
/// Long enough to have seen it work, recent enough to remember why they bought.
const AFTER_DAYS: i32 = 45;
const WITHIN_DAYS: i32 = 365;
/// An email either way this recently means they're already talking, so an automatic ask would cut in.
const TALKING_DAYS: i32 = 14;
/// Companies like theirs the agent picks from.
const CANDIDATES: usize = 12;
/// Where the email's page link goes, replaced with the real link.
pub(super) const LINK: &str = "{link}";

const WRITER: &str = "You help a B2B sales team ask happy customers for introductions, for Splee, an outbound sales tool, \
like a considerate account manager. Use only what you're given. Never invent facts, results, numbers, names or quotes, \
never pressure, and never offer rewards. Plain text only, no markdown.";

/// A company the ask names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Company {
    pub name: String,
    pub domain: String,
    pub why: String,
}

/// A referral as the Pipeline's Referrals view and a won deal's window show it. `status` is writing, drafted, sent,
/// skipped or failed. One whose email was deleted, cancelled or failed counts as skipped, saying so.
pub const SELECT: &str = "select f.id, f.deal_id, d.lead_id, l.full_name as lead_name, l.company_name,
        nullif(u.name, '') as owner_name, d.value_cents, d.currency, d.closed_at as won_at, f.why, f.companies,
        case when f.skipped <> '' then f.skipped when e.status = 'failed' then 'The email failed to send'
             when e.status = 'cancelled' then 'The email was cancelled' when f.written_at is not null and e.id is null then 'The draft was deleted'
             else '' end as skipped,
        f.email_id, e.subject as email_subject, e.body as email_body, e.status as email_status,
        case when r.status in ('queued', 'running') then 'writing' when f.written_at is null then 'failed'
             when f.skipped <> '' or e.id is null or e.status in ('failed', 'cancelled') then 'skipped'
             when e.status = 'draft' then 'drafted' else 'sent' end as status,
        f.token, f.views, f.last_viewed_at, (select count(*) from leads n where n.referral_id = f.id) as referred, f.created_at
     from deal_referrals f join deals d on d.id = f.deal_id join leads l on l.id = d.lead_id
     left join users u on u.id = l.owner_id left join agent_runs r on r.id = f.run_id
     left join email_messages e on e.id = f.email_id";

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Referral {
    pub id: Uuid,
    pub deal_id: Uuid,
    pub lead_id: Uuid,
    pub lead_name: String,
    pub company_name: String,
    pub owner_name: Option<String>,
    pub value_cents: i64,
    pub currency: String,
    pub won_at: Option<Ts>,
    pub why: String,
    pub companies: Json<Vec<Company>>,
    pub skipped: String,
    pub email_id: Option<Uuid>,
    pub email_subject: Option<String>,
    pub email_body: Option<String>,
    pub email_status: Option<String>,
    pub status: String,
    pub token: String,
    pub views: i32,
    pub last_viewed_at: Option<Ts>,
    /// Leads who left their details on the page.
    pub referred: i64,
    pub created_at: Ts,
}

/// Asks each customer won AFTER_DAYS ago, once, in projects that keep this on. Returns how many runs it queued.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "with due as (
           insert into deal_referrals (project_id, deal_id)
           select d.project_id, d.id from deals d
           join projects p on p.id = d.project_id and p.org_id is not null
           join project_settings s on s.project_id = d.project_id and s.ask_for_referrals
           where d.status = 'won' and d.closed_at < now() - make_interval(days => $1) and d.closed_at > now() - make_interval(days => $2)
             and not exists (select 1 from deal_referrals f where f.deal_id = d.id)
           order by d.closed_at desc limit $3
           on conflict (deal_id) do nothing
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id",
    )
    .bind(AFTER_DAYS)
    .bind(WITHIN_DAYS)
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
    let run = super::enqueue(&mut *tx, kind::REFERRAL_ASK, Some(project_id), Some(org_id), json!({ "referral_id": id, "auto": auto })).await?;
    sqlx::query("update deal_referrals set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

/// Has the agent write the ask now, from the won deal's window, replacing a draft nobody approved and keeping the page's
/// link. One being written is reused. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "insert into deal_referrals (project_id, deal_id) values ($1, $2) on conflict (deal_id) do update set project_id = excluded.project_id returning id",
    )
    .bind(project_id)
    .bind(deal_id)
    .fetch_one(&mut *tx)
    .await?;
    let writing: Option<Uuid> =
        sqlx::query_scalar("select r.id from deal_referrals f join agent_runs r on r.id = f.run_id where f.id = $1 and r.status in ('queued', 'running')")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(run) = writing {
        tx.commit().await?;
        return Ok(run);
    }
    sqlx::query("delete from email_messages e using deal_referrals f where f.id = $1 and e.id = f.email_id and e.status = 'draft'")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update deal_referrals set email_id = null, why = '', companies = '[]', skipped = '', written_at = null, dismissed_at = null where id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let run = start(&mut tx, id, project_id, org_id, false).await?;
    tx.commit().await?;
    Ok(run)
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        crate::mail::every(Duration::from_secs(3600), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "referrals").await
    });
}

pub fn page_url(public_url: &str, token: &str) -> String {
    format!("{}/r/{token}", public_url.trim_end_matches('/'))
}

#[derive(Deserialize)]
struct Input {
    referral_id: Uuid,
    auto: bool,
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
struct Pick {
    number: i64,
    why: String,
}

#[derive(Deserialize)]
struct Answer {
    ask: bool,
    why: String,
    companies: Vec<Pick>,
    subject: String,
    body: String,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ask": { "type": "boolean" },
            "why": { "type": "string" },
            "companies": { "type": "array", "items": { "type": "object", "properties": { "number": { "type": "integer" }, "why": { "type": "string" } }, "required": ["number", "why"] } },
            "subject": { "type": "string" },
            "body": { "type": "string" }
        },
        "required": ["ask", "why", "companies", "subject", "body"]
    })
}

/// The picked companies, by their number in the list, each once, at most three.
fn picked(picks: &[Pick], candidates: &[crate::lookalikes::Match]) -> Vec<Company> {
    let mut out: Vec<Company> = vec![];
    for p in picks {
        let Some(m) = usize::try_from(p.number).ok().and_then(|n| n.checked_sub(1)).and_then(|i| candidates.get(i)) else { continue };
        if out.len() < 3 && !out.iter().any(|c| c.domain == m.domain) {
            out.push(Company { name: m.name.clone(), domain: m.domain.clone(), why: p.why.trim().chars().take(160).collect() });
        }
    }
    out
}

/// The body with the page's link where the writer put it, or at the end when it left it out.
pub(super) fn with_link(body: &str, url: &str) -> String {
    let body = body.trim();
    if body.contains(LINK) { body.replacen(LINK, url, 1).replace(LINK, "") } else { format!("{body}\n\n{url}") }
}

/// Marks the ask done without an email, saying why.
async fn skip(ctx: &Ctx, id: Uuid, why: &str) -> anyhow::Result<Outcome> {
    sqlx::query("update deal_referrals set skipped = $2, written_at = now() where id = $1 and run_id = $3 and written_at is null")
        .bind(id)
        .bind(why)
        .bind(ctx.run.id)
        .execute(ctx.db())
        .await?;
    Ok(Outcome::new(format!("No referral ask: {why}")))
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let id = input.referral_id;
    let row = sqlx::query_as::<_, Row>(
        "select f.deal_id, f.token, d.status, d.closed_at, d.lead_id, d.value_cents, d.currency, d.notes, d.created_at
         from deal_referrals f join deals d on d.id = f.deal_id where f.id = $1 and f.project_id = $2",
    )
    .bind(id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the referral was deleted")?;
    let Some(won_at) = row.closed_at.filter(|_| row.status == "won") else {
        return skip(ctx, id, "the deal is no longer won").await;
    };
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(row.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    if input.auto {
        let on: bool = sqlx::query_scalar("select coalesce((select ask_for_referrals from project_settings where project_id = $1), true)")
            .bind(lead.project_id)
            .fetch_one(db)
            .await?;
        if !on {
            return skip(ctx, id, "asking for referrals was switched off").await;
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
        return skip(ctx, id, "you've written to each other in the last two weeks, so ask in the thread").await;
    }

    let candidates: Vec<crate::lookalikes::Match> = match lead.company_domain.as_deref() {
        Some(domain) => crate::lookalikes::find(db, lead.project_id, Some(&[domain.to_string()])).await?.results.into_iter().take(CANDIDATES).collect(),
        None => vec![],
    };
    let review: Option<(Option<String>, String)> =
        sqlx::query_as("select reason, story from deal_reviews where deal_id = $1 and outcome = 'won' and created_at >= $2")
            .bind(row.deal_id)
            .bind(won_at)
            .fetch_optional(db)
            .await?;
    let settings = WriterSettings::load(ctx, lead.project_id).await?;
    let lang = crate::language::for_lead(db, lead.id).await?;
    let context = briefs::gather(db, &lead, org_id).await?;
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
    if let Some((reason, story)) = &review {
        deal.push(format!("Why they bought: {}. {}", reason.as_deref().unwrap_or("").replace('_', " "), story.trim()));
    }
    let list = if candidates.is_empty() {
        "None found.".to_string()
    } else {
        candidates
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let place = [m.city.as_deref(), m.region.as_deref(), Some(m.country_code.as_str())].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
                format!("{}. {} ({}), {}{}. {}", i + 1, m.name, m.domain, m.category.as_deref().unwrap_or("company"), if place.is_empty() { String::new() } else { format!(" in {place}") }, m.reasons.join(". "))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let prompt = format!(
        "{who} is our customer.\n{deal}\n\nWhat we sell: {name}. {description}\n\n\
         The conversation and what else Splee knows:\n{context}\n\n\
         Companies like theirs that we don't work with yet:\n{list}\n\n\
         Decide whether to ask them for an introduction now.\n\
         1. ask: true only if the conversation shows they're happy with us: they praised it, said it works, or bought more. \
         False when they're quiet since buying, unhappy, cutting back, or still setting up.\n\
         2. why: under 25 words, the evidence they're happy, or why not to ask.\n\
         3. companies: up to 3 from the list they'd most likely know people at, like the same city, region or industry, by \
         number, each with why (under 15 words). Empty if none fit.\n\
         4. subject: a short subject. It goes in the existing thread when there is one.\n\
         5. body: if ask, a 50 to 100 word email from {us}. Thank them for something specific from the conversation, ask \
         whether they know someone at the companies you picked, or anyone else with the same problem, and say they can \
         forward this page, written as {LINK} on a line of its own. Make it easy to say no. No sign-off. {language} Empty if \
         not ask.",
        who = [lead.full_name.trim(), lead.company_name.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" at "),
        deal = deal.join("\n"),
        name = profile.name,
        description = profile.description,
        language = crate::language::instruction(lang),
    );
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    ctx.log(format!("reading how {first} feels about us and who they might know…")).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.3, ..Call::writing(WRITER, prompt, schema()) };
    let answer: Answer = ctx.ask(&call).await?;
    if !answer.ask || answer.body.trim().is_empty() {
        let why = if answer.why.trim().is_empty() { "the agent says it's not the time to ask" } else { answer.why.trim() };
        return skip(ctx, id, why).await;
    }
    let companies = picked(&answer.companies, &candidates);
    let url = page_url(&ctx.state.cfg.public_url, &row.token);
    let body = settings.sign(&with_link(&answer.body, &url));

    let mut tx = db.begin().await?;
    let email_id = draft_in_thread(&mut tx, &lead, "referral_ask", &answer.subject, &body, lang.code).await?;
    let saved = sqlx::query(
        "update deal_referrals set email_id = $2, why = $3, companies = $4, written_at = now() where id = $1 and run_id = $5 and written_at is null",
    )
    .bind(id)
    .bind(email_id)
    .bind(answer.why.trim())
    .bind(Json(&companies))
    .bind(ctx.run.id)
    .execute(&mut *tx)
    .await?;
    if saved.rows_affected() == 0 {
        return Ok(Outcome::new("The referral ask is already written"));
    }
    tx.commit().await?;

    if input.auto {
        let link = hooks::lead_url(&ctx.state.cfg.public_url, lead.project_id, lead.id);
        let summary = format!("{} looks happy with you. {} An email asking for introductions is waiting for your approval.", lead.full_name, answer.why.trim());
        let alert = ChatAlert { label: "Referral", summary: &summary, quote: Some(&body), link: &link, link_text: "Review the email" };
        hooks::post_chat(db, lead.project_id, &alert).await;
    }
    Ok(Outcome { message: format!("Drafted a referral ask to {}", lead.full_name), output: json!({ "email_id": email_id }) })
}

/// What the referral page shows: who sent it and what we do. Nothing about the deal itself.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Public {
    pub first_name: String,
    pub full_name: String,
    pub company: String,
    pub company_domain: Option<String>,
    pub from_name: String,
    pub from_domain: Option<String>,
    pub description: String,
    pub value_props: Json<Vec<String>>,
    pub sender_name: String,
    /// The booking link of the mailbox that wrote to the customer, else its Splee booking page, else the project's booking
    /// link. Null without any.
    pub booking_url: Option<String>,
}

pub async fn public(db: &PgPool, public_url: &str, token: &str) -> sqlx::Result<Option<Public>> {
    sqlx::query_as::<_, Public>(
        "select l.first_name, l.full_name, l.company_name as company, l.company_domain::text as company_domain,
                coalesce(nullif(pr.company->>'name', ''), pr.name) as from_name, pr.domain::text as from_domain,
                coalesce(pr.company->>'description', '') as description,
                coalesce(pr.company->'value_props', '[]') as value_props,
                coalesce(nullif(m.from_name, ''), s.sender_name, '') as sender_name,
                coalesce(nullif(m.booking_link, ''), $2 || '/book/' || b.slug, nullif(s.booking_link, '')) as booking_url
         from deal_referrals f join deals d on d.id = f.deal_id join leads l on l.id = d.lead_id join projects pr on pr.id = f.project_id
         left join project_settings s on s.project_id = f.project_id
         left join lateral (select e.mailbox_id from email_messages e where e.lead_id = l.id and e.direction = 'outbound' and e.mailbox_id is not null
                            order by e.created_at desc limit 1) e on true
         left join mailboxes m on m.id = e.mailbox_id
         left join booking_pages b on b.mailbox_id = m.id
         where f.token = $1 and d.status = 'won'",
    )
    .bind(token)
    .bind(public_url.trim_end_matches('/'))
    .fetch_optional(db)
    .await
}

pub async fn viewed(db: &PgPool, token: &str) -> sqlx::Result<()> {
    sqlx::query("update deal_referrals set views = views + 1, last_viewed_at = now() where token = $1").bind(token).execute(db).await?;
    Ok(())
}

/// What someone left on a referral page.
#[derive(Debug, Deserialize)]
pub struct SignUp {
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub company: String,
    #[serde(default)]
    pub note: String,
}

/// Makes the person a lead of the project credited to the referral, or credits the lead they already are when no one
/// else has. Returns the lead, or None for an unknown page. Posts to the project's chats.
pub async fn sign_up(db: &PgPool, public_url: &str, token: &str, form: &SignUp) -> anyhow::Result<Option<Uuid>> {
    let Some((referral_id, project_id, from)): Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "select f.id, f.project_id, l.full_name || coalesce(nullif(' at ' || l.company_name, ' at '), '')
         from deal_referrals f join deals d on d.id = f.deal_id join leads l on l.id = d.lead_id where f.token = $1 and d.status = 'won'",
    )
    .bind(token)
    .fetch_optional(db)
    .await?
    else {
        return Ok(None);
    };
    let email = form.email.trim().to_lowercase();
    let name = form.name.trim();
    let domain = email.rsplit_once('@').map(|(_, d)| d.to_string()).filter(|d| !crate::payments::free_mail(d));
    let note = form.note.trim();
    let notes = if note.is_empty() { format!("Referred by {from}.") } else { format!("Referred by {from}. They wrote: {note}") };
    let lead_id: Uuid = sqlx::query_scalar(
        "with found as (
           update leads set referral_id = coalesce(referral_id, $2) where project_id = $1 and email = $3::citext returning id),
         made as (
           insert into leads (project_id, full_name, first_name, email, email_status, company_name, company_domain, source, notes, referral_id)
           select $1, $4, split_part($4, ' ', 1), $3, 'found', $5, $6, 'referral', $7, $2 where not exists (select 1 from found)
           returning id)
         select id from found union all select id from made limit 1",
    )
    .bind(project_id)
    .bind(referral_id)
    .bind(&email)
    .bind(name)
    .bind(form.company.trim())
    .bind(domain)
    .bind(&notes)
    .fetch_one(db)
    .await?;
    let who = if form.company.trim().is_empty() { name.to_string() } else { format!("{name} ({})", form.company.trim()) };
    let summary = format!("{who} left their details on the page {from} forwarded.");
    let link = hooks::lead_url(public_url, project_id, lead_id);
    let alert = ChatAlert { label: "New referral", summary: &summary, quote: (!note.is_empty()).then_some(note), link: &link, link_text: "Open the lead" };
    hooks::post_chat(db, project_id, &alert).await;
    Ok(Some(lead_id))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    fn m(domain: &str) -> crate::lookalikes::Match {
        crate::lookalikes::Match {
            domain: domain.into(),
            name: domain.split('.').next().unwrap().into(),
            website: String::new(),
            city: None,
            region: None,
            country_code: "DE".into(),
            category: None,
            score: 80,
            reasons: vec![],
        }
    }

    #[test]
    fn picks_are_taken_by_number_once_each_and_at_most_three() {
        let candidates = [m("a.example"), m("b.example"), m("c.example"), m("d.example")];
        let p = |number, why: &str| Pick { number, why: why.into() };
        let got = picked(&[p(2, " Same city "), p(0, "none"), p(2, "again"), p(9, "past the end"), p(1, "x"), p(3, "y"), p(4, "z")], &candidates);
        assert_eq!(got.iter().map(|c| c.domain.as_str()).collect::<Vec<_>>(), ["b.example", "a.example", "c.example"]);
        assert_eq!(got[0].why, "Same city");
    }

    #[test]
    fn the_page_link_goes_where_the_writer_put_it_or_at_the_end() {
        assert_eq!(with_link("Hi\n{link}\nThanks", "https://s/r/t"), "Hi\nhttps://s/r/t\nThanks");
        assert_eq!(with_link("Hi {link} and {link}", "u"), "Hi u and ");
        assert_eq!(with_link("Hi there ", "u"), "Hi there\n\nu");
    }

    async fn won(db: &PgPool, org: Uuid, project: Uuid, name: &str, domain: &str, days_ago: i32) -> (Uuid, Uuid) {
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, full_name, first_name, company_name, company_domain, email) values ($1, $2, split_part($2, ' ', 1), $3, $3 || '.example', lower(split_part($2, ' ', 1)) || '@' || $3 || '.example') returning id",
        )
        .bind(project)
        .bind(name)
        .bind(domain)
        .fetch_one(db)
        .await
        .unwrap();
        let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Won', 'won', 8) returning id").bind(org).fetch_one(db).await.unwrap();
        let deal: Uuid = sqlx::query_scalar(
            "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, closed_at, created_at)
             values ($1, $2, $3, $4, 'won', 960000, now() - make_interval(days => $5), now() - interval '200 days') returning id",
        )
        .bind(org)
        .bind(project)
        .bind(lead)
        .bind(stage)
        .bind(days_ago)
        .fetch_one(db)
        .await
        .unwrap();
        (lead, deal)
    }

    async fn run_next(db: &PgPool, answer: Value) -> String {
        let started = sqlx::query_as::<_, AgentRun>(
            "update agent_runs set status = 'running' where id = (select id from agent_runs where kind = 'referral_ask' and status = 'queued' limit 1) returning *",
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

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_happy_customer_is_asked_once_and_whoever_signs_up_on_their_page_is_credited_to_them(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar(
            "insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\", \"description\": \"Call notes in your CRM.\", \"value_props\": [\"No typing\"]}') returning id",
        )
        .bind(org)
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(&db).await.unwrap();
        let (lead, deal) = won(&db, org, project, "Daniel Okafor", "tallyho", 60).await;
        won(&db, org, project, "Mara Lind", "northpine", 10).await;
        assert_eq!(queue_due(&db).await.unwrap(), 1, "only the customer of six weeks");
        assert_eq!(queue_due(&db).await.unwrap(), 0, "once");

        let prompt = run_next(
            &db,
            json!({ "ask": true, "why": "Said the German transcripts were spot on.", "companies": [{ "number": 1, "why": "x" }],
                    "subject": "Thanks", "body": "Hi Daniel, thanks for the kind words.\n{link}\nNo worries if not." }),
        )
        .await;
        assert!(prompt.contains("Daniel Okafor at tallyho is our customer") && prompt.contains("None found."), "{prompt}");
        let r = sqlx::query_as::<_, Referral>(&format!("{SELECT} where f.deal_id = $1")).bind(deal).fetch_one(&db).await.unwrap();
        assert_eq!((r.status.as_str(), r.companies.0.len()), ("drafted", 0), "a pick past the list is dropped");
        let body = r.email_body.unwrap();
        assert!(body.contains(&format!("/r/{}\nNo worries", r.token)) && !body.contains("{link}"), "{body}");
        let kind: String = sqlx::query_scalar("select kind from email_messages where id = $1").bind(r.email_id).fetch_one(&db).await.unwrap();
        assert_eq!(kind, "referral_ask");

        let page = public(&db, "https://app.example", &r.token).await.unwrap().unwrap();
        assert_eq!((page.first_name.as_str(), page.from_name.as_str(), page.value_props.0.len()), ("Daniel", "Fieldnote", 1));
        assert!(public(&db, "https://app.example", "nope").await.unwrap().is_none());

        let form = |email: &str| SignUp { name: "Lea Vogel".into(), email: email.into(), company: "Brightline".into(), note: "Daniel said to look.".into() };
        let new = sign_up(&db, "https://app.example", &r.token, &form("lea@brightline.example")).await.unwrap().unwrap();
        assert_eq!(sign_up(&db, "https://app.example", &r.token, &form("LEA@brightline.example ")).await.unwrap(), Some(new), "the same person once");
        let (source, domain, referral): (String, Option<String>, Option<Uuid>) =
            sqlx::query_as("select source, company_domain::text, referral_id from leads where id = $1").bind(new).fetch_one(&db).await.unwrap();
        assert_eq!((source.as_str(), domain.as_deref(), referral), ("referral", Some("brightline.example"), Some(r.id)));
        let known = sign_up(&db, "https://app.example", &r.token, &form("daniel@tallyho.example")).await.unwrap();
        assert_eq!(known, Some(lead), "a lead already in the project is credited, not copied");
        let private = sign_up(&db, "https://app.example", &r.token, &form("lea@gmail.com")).await.unwrap().unwrap();
        let domain: Option<String> = sqlx::query_scalar("select company_domain::text from leads where id = $1").bind(private).fetch_one(&db).await.unwrap();
        assert_eq!(domain, None, "a personal address says nothing about the company");
        assert!(sign_up(&db, "https://app.example", "nope", &form("x@y.example")).await.unwrap().is_none());

        // Asked again from the deal's window, the draft is replaced and the page keeps its link.
        request(&db, deal, project, org).await.unwrap();
        run_next(&db, json!({ "ask": false, "why": "They're still setting up.", "companies": [], "subject": "", "body": "" })).await;
        let again = sqlx::query_as::<_, Referral>(&format!("{SELECT} where f.deal_id = $1")).bind(deal).fetch_one(&db).await.unwrap();
        assert_eq!((again.status.as_str(), again.skipped.as_str(), again.token.as_str(), again.referred), ("skipped", "They're still setting up.", r.token.as_str(), 3));
        let drafts: i64 = sqlx::query_scalar("select count(*) from email_messages where kind = 'referral_ask'").fetch_one(&db).await.unwrap();
        assert_eq!(drafts, 0, "the unapproved draft was replaced");
    }
}
