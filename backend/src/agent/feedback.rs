//! Product feedback: the agent reads what buyers write and say (replies, meeting transcripts, win/loss reviews and deal
//! room questions) and files each thing they ask the product to do under a request, with their words. The Feedback page
//! ranks the requests by the open and lost deals of the people who asked (routes/feedback.rs). When one ships, the agent
//! drafts an email to each of them in their thread, for the team to approve.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::write::{WriterSettings, draft_in_thread};
use super::{Ctx, Outcome, kind, meetings};
use crate::models::{Lead, Ts};
use crate::state::AppState;
use crate::text::plain;

/// Characters the agent reads of a source, a meeting's transcript, and all of them in one run.
const PER_SOURCE: usize = 1500;
const PER_MEETING: usize = 5000;
const PER_RUN: usize = 60_000;
const MAX_SOURCES: i64 = 40;
/// The requests the agent matches against, the newest first.
const MAX_KNOWN: i64 = 200;
/// People a shipped request emails in one run.
const MAX_NOTICES: i64 = 50;
const MAX_TITLE: usize = 80;
const MAX_QUOTE: usize = 400;

const READER: &str = "You read what B2B buyers write and say to a sales team using Splee, an outbound sales tool, and pick out \
what they ask the seller's product to do. Use only what you're given, and copy their words exactly.";

const WRITER: &str = "You write short, plain emails for a B2B sales team using Splee, an outbound sales tool. Use only what \
you're given. Never invent features, dates, numbers or links. Plain text only, no markdown.";

/// What buyers said in a project ($1) after the source read last, at a time ($2) with an id ($4), and up to now, oldest
/// first, at most $3. Each is timed by when it could first be read: a review's row comes before the agent writes it, so it
/// counts from its run's end. The id breaks ties, so a run that stops partway through sources with the same time doesn't
/// skip the rest.
const SOURCES: &str = "select source, id, lead_id, at, text from (
    select 'email' as source, e.id, e.lead_id, e.created_at as at, e.body as text from email_messages e
    where e.project_id = $1 and e.direction = 'inbound' and coalesce(e.classification, '') not in ('ooo', 'unsubscribe', 'bounce')
    union all
    select 'meeting', m.id, m.lead_id, m.summarized_at, m.transcript from meetings m
    where m.project_id = $1 and m.summarized_at is not null and m.transcript <> ''
    union all
    select 'review', rv.id, d.lead_id, coalesce(r.finished_at, now()), concat_ws(E'\\n', rv.story, rv.turning_point, rv.lesson)
    from deal_reviews rv join deals d on d.id = rv.deal_id left join agent_runs r on r.id = rv.run_id
    where rv.project_id = $1 and rv.reason is not null
    union all
    select 'room', q.id, d.lead_id, q.created_at, q.question
    from deal_room_questions q join deal_rooms r on r.id = q.room_id join deals d on d.id = r.deal_id where r.project_id = $1
) s where (at, id) > ($2, $4) and at <= now() order by at, id limit $3";

#[derive(sqlx::FromRow)]
struct Source {
    source: String,
    id: Uuid,
    lead_id: Uuid,
    at: Ts,
    text: String,
}

impl Source {
    /// The words the agent reads: a reply without the thread it quotes, cut to size.
    fn words(&self) -> String {
        let text = if self.source == "email" { fresh(&self.text) } else { self.text.trim().to_string() };
        text.chars().take(if self.source == "meeting" { PER_MEETING } else { PER_SOURCE }).collect()
    }

    fn label(&self) -> &'static str {
        match self.source.as_str() {
            "email" => "their email",
            "meeting" => "a meeting transcript",
            "review" => "our review of the deal",
            _ => "a question on their deal room",
        }
    }
}

/// A reply's own lines, before the "On ... wrote:" or "-----Original Message-----" of the thread it quotes.
fn fresh(body: &str) -> String {
    let mut lines: Vec<&str> = vec![];
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("-----Original Message-----") || (t.starts_with("On ") && t.ends_with("wrote:")) {
            break;
        }
        // Gmail wraps a long "On <date> <name> <address> wrote:" line, so only its last piece ends with "wrote:".
        if t.ends_with("wrote:") && lines.last().is_some_and(|l| l.trim().starts_with("On ")) {
            lines.pop();
            break;
        }
        if !t.starts_with('>') {
            lines.push(line);
        }
    }
    lines.join("\n").trim().to_string()
}

/// Starts a scan of the project's new sources, or returns the one already running.
pub async fn request(db: &PgPool, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    sqlx::query("insert into feedback_scans (project_id) values ($1) on conflict do nothing").bind(project_id).execute(&mut *tx).await?;
    // The project's row is locked whether a scan runs or not, so two asks after one finished start one scan between them.
    // Its run is read in a second statement: one that waited for the lock still sees the runs as they were when it began,
    // so it would miss the run the other ask just started.
    let last: Option<Uuid> =
        sqlx::query_scalar("select run_id from feedback_scans where project_id = $1 for update").bind(project_id).fetch_one(&mut *tx).await?;
    let running: Option<Uuid> = sqlx::query_scalar("select id from agent_runs where id = $1 and status in ('queued', 'running')")
        .bind(last)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(run) = running {
        return Ok(run);
    }
    let run = start(&mut tx, project_id, org_id).await?;
    tx.commit().await?;
    Ok(run)
}

async fn start(tx: &mut sqlx::PgConnection, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let run = super::enqueue(&mut *tx, kind::FEEDBACK_SCAN, Some(project_id), Some(org_id), json!({})).await?;
    sqlx::query("update feedback_scans set run_id = $2 where project_id = $1").bind(project_id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

/// Scans the projects with deals that have something new to read, at most every 6 hours. Returns how many.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    sqlx::query(
        "insert into feedback_scans (project_id)
         select distinct d.project_id from deals d join projects p on p.id = d.project_id and p.org_id is not null on conflict do nothing",
    )
    .execute(&mut *tx)
    .await?;
    let due: Vec<(Uuid, Uuid, Ts, Uuid)> = sqlx::query_as(
        "select s.project_id, p.org_id, s.read_to, s.read_id from feedback_scans s join projects p on p.id = s.project_id and p.org_id is not null
         left join agent_runs r on r.id = s.run_id
         where coalesce(r.status, '') not in ('queued', 'running') and (s.scanned_at is null or s.scanned_at < now() - interval '6 hours')
         for update of s skip locked",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut started = 0;
    for (project_id, org_id, read_to, read_id) in due {
        let new: Option<Uuid> =
            sqlx::query_scalar(&format!("select id from ({SOURCES}) n")).bind(project_id).bind(read_to).bind(1i64).bind(read_id).fetch_optional(&mut *tx).await?;
        if new.is_some() {
            start(&mut tx, project_id, org_id).await?;
            started += 1;
        }
    }
    tx.commit().await?;
    Ok(started)
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(3600), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "feedback").await });
}

#[derive(Deserialize)]
struct Found {
    source: usize,
    /// The number of the request it matches, or 0 for a new one.
    request: usize,
    #[serde(default)]
    title: String,
    #[serde(default)]
    detail: String,
    quote: String,
}

#[derive(Deserialize)]
struct Scan {
    found: Vec<Found>,
}

fn scan_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "found": { "type": "array", "items": { "type": "object", "properties": {
                "source": { "type": "integer" },
                "request": { "type": "integer" },
                "title": { "type": "string" },
                "detail": { "type": "string" },
                "quote": { "type": "string" }
            }, "required": ["source", "request", "title", "detail", "quote"] } }
        },
        "required": ["found"]
    })
}

/// Reads the project's sources since the last scan and files what they ask for. A run reads up to 40 sources, and the
/// next pass picks up the rest.
pub async fn scan(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let project_id = ctx.project_id()?;
    let db = ctx.db();
    let (read_to, read_id): (Ts, Uuid) = sqlx::query_as("select read_to, read_id from feedback_scans where project_id = $1").bind(project_id).fetch_one(db).await?;
    let all: Vec<Source> = sqlx::query_as(SOURCES).bind(project_id).bind(read_to).bind(MAX_SOURCES).bind(read_id).fetch_all(db).await?;
    let (mut sources, mut size) = (vec![], 0);
    for s in all {
        let words = s.words();
        if !sources.is_empty() && size + words.len() > PER_RUN {
            break;
        }
        size += words.len();
        sources.push((s, words));
    }
    let Some((last, last_id)) = sources.last().map(|(s, _)| (s.at, s.id)) else {
        sqlx::query("update feedback_scans set scanned_at = now() where project_id = $1").bind(project_id).execute(db).await?;
        return Ok(Outcome::new("Nothing new from buyers to read"));
    };

    let known: Vec<(Uuid, String)> =
        sqlx::query_as("select id, title from feedback_requests where project_id = $1 order by created_at desc limit $2").bind(project_id).bind(MAX_KNOWN).fetch_all(db).await?;
    let (_, profile) = load_project(ctx, project_id).await?;
    let who: HashMap<Uuid, String> = sqlx::query_as::<_, (Uuid, String)>(
        "select id, concat_ws(' at ', nullif(trim(full_name), ''), nullif(trim(company_name), '')) from leads where id = any($1)",
    )
    .bind(sources.iter().map(|(s, _)| s.lead_id).collect::<Vec<_>>())
    .fetch_all(db)
    .await?
    .into_iter()
    .collect();
    let list = if known.is_empty() {
        "None yet.".to_string()
    } else {
        known.iter().enumerate().map(|(i, (_, title))| format!("{}. {title}", i + 1)).collect::<Vec<_>>().join("\n")
    };
    let read = sources
        .iter()
        .enumerate()
        .map(|(i, (s, words))| format!("[{}] {}, {}:\n{words}", i + 1, who.get(&s.lead_id).map(String::as_str).filter(|w| !w.is_empty()).unwrap_or("A buyer"), s.label()))
        .collect::<Vec<_>>()
        .join("\n\n");
    let prompt = format!(
        "We sell {name}. {description}\n\n\
         The requests buyers already made:\n{list}\n\n\
         What buyers wrote and said lately:\n{read}\n\n\
         List each thing a source asks our product to do that it doesn't, or names as missing or a reason it can't buy: a \
         feature, an integration, a security or compliance need, a limit. Only the buyer's side, never what our team offered. \
         Skip questions about what the product already does, pricing, timing and scheduling. Most sources ask for nothing.\n\
         - source: the source's number.\n\
         - request: the number of the request above it's the same as, or 0 for a new one.\n\
         - title: for a new one, under 8 words naming the capability, like \"Salesforce sync\" or \"SSO with Okta\". Give \
         sources asking for the same new thing the same title. Empty otherwise.\n\
         - detail: for a new one, one sentence on what they need it for. Empty otherwise.\n\
         - quote: their words that ask for it, copied exactly from the source, under 30 words.",
        name = profile.name,
        description = profile.description,
    );
    ctx.log(format!("reading {} things buyers wrote and said…", sources.len())).await?;
    let call = Call { timeout: Duration::from_secs(4 * 60), budget_usd: 0.6, ..Call::writing(READER, prompt, scan_schema()) };
    let answer: Scan = ctx.ask(&call).await?;

    let mut tx = db.begin().await?;
    let mut titles: HashMap<String, Uuid> = known.iter().map(|(id, title)| (plain(title), *id)).collect();
    let (mut filed, mut new, mut unquoted) = (0, 0, 0);
    for f in answer.found {
        let Some((source, words)) = f.source.checked_sub(1).and_then(|i| sources.get(i)) else { continue };
        let quote: String = f.quote.trim().trim_matches('"').trim().chars().take(MAX_QUOTE).collect();
        if plain(&quote).is_empty() || !plain(words).contains(&plain(&quote)) {
            unquoted += 1;
            continue;
        }
        let request = match f.request.checked_sub(1).and_then(|i| known.get(i)) {
            Some((id, _)) => *id,
            None => {
                let title: String = f.title.trim().chars().take(MAX_TITLE).collect();
                if plain(&title).is_empty() {
                    continue;
                }
                match titles.get(&plain(&title)) {
                    Some(id) => *id,
                    None => {
                        let id: Uuid = sqlx::query_scalar("insert into feedback_requests (project_id, title, detail) values ($1, $2, $3) returning id")
                            .bind(project_id)
                            .bind(&title)
                            .bind(f.detail.trim().chars().take(400).collect::<String>())
                            .fetch_one(&mut *tx)
                            .await?;
                        titles.insert(plain(&title), id);
                        new += 1;
                        id
                    }
                }
            }
        };
        let added = sqlx::query(
            "insert into feedback_mentions (request_id, lead_id, source, source_id, quote, said_at) values ($1, $2, $3, $4, $5, $6) on conflict do nothing",
        )
        .bind(request)
        .bind(source.lead_id)
        .bind(&source.source)
        .bind(source.id)
        .bind(&quote)
        .bind(source.at)
        .execute(&mut *tx)
        .await?;
        filed += added.rows_affected();
    }
    sqlx::query("update feedback_scans set read_to = $2, read_id = $3, scanned_at = now() where project_id = $1")
        .bind(project_id)
        .bind(last)
        .bind(last_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if unquoted > 0 {
        ctx.log(format!("left out {unquoted} that didn't quote the source")).await?;
    }
    Ok(Outcome {
        message: format!("Read {} things buyers wrote and said: {filed} asks, {new} of them new requests", sources.len()),
        output: json!({ "read": sources.len(), "filed": filed, "new": new }),
    })
}

/// Marks a request shipped with what shipped, and when `email` has the agent draft an email to each person who asked.
/// Returns the run, if any. Someone who already got one isn't emailed again.
pub async fn ship(db: &PgPool, request_id: Uuid, project_id: Uuid, org_id: Uuid, note: &str, email: bool) -> sqlx::Result<Option<Uuid>> {
    let mut tx = db.begin().await?;
    sqlx::query("update feedback_requests set status = 'shipped', shipped_note = $2, shipped_at = coalesce(shipped_at, now()) where id = $1")
        .bind(request_id)
        .bind(note.trim())
        .execute(&mut *tx)
        .await?;
    let run = if email {
        let run = super::enqueue(&mut *tx, kind::FEATURE_SHIPPED, Some(project_id), Some(org_id), json!({ "request_id": request_id })).await?;
        sqlx::query("update feedback_requests set announce_run_id = $2 where id = $1").bind(request_id).bind(run).execute(&mut *tx).await?;
        Some(run)
    } else {
        None
    };
    tx.commit().await?;
    Ok(run)
}

#[derive(Deserialize)]
struct ShipInput {
    request_id: Uuid,
}

#[derive(Deserialize)]
struct Email {
    subject: String,
    body: String,
}

fn email_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "subject": { "type": "string" }, "body": { "type": "string" } },
        "required": ["subject", "body"]
    })
}

/// The people who asked for a request and weren't told yet, each with what they said last and their latest deal.
#[derive(sqlx::FromRow)]
struct Asker {
    lead_id: Uuid,
    quote: String,
    said_at: Ts,
    deal_status: Option<String>,
}

/// Drafts the email telling each person who asked that the request shipped, in their thread and their language.
pub async fn announce(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: ShipInput = ctx.input()?;
    let db = ctx.db();
    let project_id = ctx.project_id()?;
    let (title, detail, note, status): (String, String, String, String) =
        sqlx::query_as("select title, detail, shipped_note, status from feedback_requests where id = $1 and project_id = $2")
            .bind(input.request_id)
            .bind(project_id)
            .fetch_optional(db)
            .await?
            .context("the request was deleted")?;
    if status != "shipped" {
        return Ok(Outcome::new("The request isn't shipped anymore"));
    }
    let askers: Vec<Asker> = sqlx::query_as(
        "select distinct on (m.lead_id) m.lead_id, m.quote, m.said_at,
                (select d.status from deals d where d.lead_id = m.lead_id order by d.created_at desc limit 1) as deal_status
         from feedback_mentions m
         where m.request_id = $1 and not exists (select 1 from feedback_notices n where n.request_id = $1 and n.lead_id = m.lead_id)
         order by m.lead_id, m.said_at desc limit $2",
    )
    .bind(input.request_id)
    .bind(MAX_NOTICES)
    .fetch_all(db)
    .await?;
    let (project, profile) = load_project(ctx, project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    let settings = WriterSettings::load(ctx, project_id).await?;
    let us = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) };

    let more = askers.len() as i64 == MAX_NOTICES;
    let (mut drafted, mut skipped) = (0, 0);
    for asker in askers {
        let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(asker.lead_id).fetch_one(db).await?;
        if let Some(blocker) = meetings::follow_up_blocker(&mut *db.acquire().await?, org_id, &lead).await? {
            sqlx::query("insert into feedback_notices (request_id, lead_id, skipped) values ($1, $2, $3) on conflict do nothing")
                .bind(input.request_id)
                .bind(lead.id)
                .bind(blocker)
                .execute(db)
                .await?;
            skipped += 1;
            continue;
        }
        let lang = crate::language::for_lead(db, lead.id).await?;
        let situation = match asker.deal_status.as_deref() {
            Some("won") => "They're a customer.",
            Some("open") => "We're still talking about them buying.",
            Some("lost") => "They didn't buy. This may be a reason to look again, but don't push.",
            _ => "They haven't bought.",
        };
        let prompt = format!(
            "{who} asked us for this on {said}: \"{quote}\"\n{situation}\n\n\
             What they asked for: {title}. {detail}\n\
             What shipped, from our team: {note}\n\n\
             Write a short email from {us} telling them it's live.\n\
             - subject: short. It goes in the existing thread when there is one.\n\
             - body: 40 to 90 words. Say they asked for it and it's here now, in a sentence or two on what it does for \
             them, only from what shipped. Include any link from what shipped as it's written. End with one easy question, \
             like whether they'd like a quick look. No sign-off.\n\
             {language}",
            who = [lead.full_name.trim(), lead.company_name.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" at "),
            said = asker.said_at.format("%-d %b %Y"),
            quote = asker.quote,
            note = if note.is_empty() { "(no note)" } else { note.as_str() },
            language = crate::language::instruction(lang),
        );
        let call = Call { timeout: Duration::from_secs(2 * 60), budget_usd: 0.1, ..Call::writing(WRITER, prompt, email_schema()) };
        let email: Email = ctx.ask(&call).await?;
        if email.body.trim().is_empty() {
            // Noted like anyone else skipped, so the next run starts after them rather than stopping on the same people.
            sqlx::query("insert into feedback_notices (request_id, lead_id, skipped) values ($1, $2, 'no email was written') on conflict do nothing")
                .bind(input.request_id)
                .bind(lead.id)
                .execute(db)
                .await?;
            skipped += 1;
            continue;
        }
        let mut tx = db.begin().await?;
        let email_id = draft_in_thread(&mut tx, &lead, "feature_shipped", &email.subject, &settings.sign(email.body.trim()), lang.code).await?;
        sqlx::query("insert into feedback_notices (request_id, lead_id, email_id) values ($1, $2, $3)")
            .bind(input.request_id)
            .bind(lead.id)
            .bind(email_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        drafted += 1;
    }
    if more && drafted + skipped > 0 {
        // More people asked than one run writes to, so the next run takes the rest.
        let mut tx = db.begin().await?;
        let run = super::enqueue(&mut *tx, kind::FEATURE_SHIPPED, Some(project_id), Some(org_id), json!({ "request_id": input.request_id })).await?;
        sqlx::query("update feedback_requests set announce_run_id = $2 where id = $1").bind(input.request_id).bind(run).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(Outcome {
        message: format!("Drafted {drafted} emails saying {title} shipped{}", if skipped > 0 { format!(", skipped {skipped}") } else { String::new() }),
        output: json!({ "drafted": drafted, "skipped": skipped }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::fake::run_next;

    #[test]
    fn a_reply_loses_the_thread_it_quotes() {
        assert_eq!(fresh("Need SSO.\n> earlier\nThanks\n\nOn Mon, 3 Nov 2026 at 10:00, Dana\n<dana@x.com> wrote:\n> old"), "Need SSO.\nThanks");
        assert_eq!(fresh("Yes\n-----Original Message-----\nFrom: us"), "Yes");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn asks_are_filed_with_their_words_and_a_shipped_one_emails_each_person_once(db: PgPool) {
        let t = crate::agent::practice::fixture::team(&db).await;
        let lead: Uuid = sqlx::query_scalar("update leads set email = 'ines@lumenfold.example' where project_id = $1 returning id").bind(t.project).fetch_one(&db).await.unwrap();
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, body, status, classification, created_at)
             values ($1, $2, 'inbound', 'inbound', $3, 'received', 'question', now() - interval '1 day'),
                    ($1, $2, 'inbound', 'inbound', 'Out until Monday.', 'received', 'ooo', now() - interval '1 day')",
        )
        .bind(t.project)
        .bind(lead)
        .bind("We'd move all twelve seats over if you had a Salesforce sync.\n\nOn Mon, Dana wrote:\n> Do you need HubSpot?")
        .execute(&db)
        .await
        .unwrap();

        let run = request(&db, t.project, t.org).await.unwrap();
        assert_eq!(request(&db, t.project, t.org).await.unwrap(), run, "one scan at a time");
        let (outcome, prompt) = run_next(
            &db,
            "feedback_scan",
            json!({ "found": [
                { "source": 1, "request": 0, "title": "Salesforce sync", "detail": "To move their seats over.", "quote": "\"if you had a Salesforce sync\"" },
                { "source": 1, "request": 0, "title": "salesforce sync!", "detail": "", "quote": "We'd move all twelve seats over" },
                { "source": 1, "request": 0, "title": "HubSpot", "detail": "", "quote": "Do you need HubSpot?" },
                { "source": 1, "request": 0, "title": "Reports", "detail": "", "quote": "they want better reports" },
                { "source": 7, "request": 0, "title": "Nothing", "detail": "", "quote": "x" }
            ] }),
        )
        .await;
        assert!(prompt.contains("[1] Ines Duarte at Lumenfold, their email:\nWe'd move") && !prompt.contains("Do you need HubSpot") && !prompt.contains("Out until"), "{prompt}");
        assert_eq!(outcome.output["filed"], 1, "the same new title is one request, a source counts once under it, and quotes not in the source are left out");
        let requests: Vec<(Uuid, String, String)> = sqlx::query_as("select id, title, detail from feedback_requests").fetch_all(&db).await.unwrap();
        assert_eq!(requests.iter().map(|r| (r.1.as_str(), r.2.as_str())).collect::<Vec<_>>(), vec![("Salesforce sync", "To move their seats over.")]);
        let request_id = requests[0].0;
        let quote: String = sqlx::query_scalar("select quote from feedback_mentions").fetch_one(&db).await.unwrap();
        assert_eq!(quote, "if you had a Salesforce sync");

        // The next scan starts after what was read, and the list it matches against has the request.
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, body, status) values ($1, $2, 'inbound', 'inbound', 'Any news on Salesforce?', 'received')")
            .bind(t.project)
            .bind(lead)
            .execute(&db)
            .await
            .unwrap();
        request(&db, t.project, t.org).await.unwrap();
        let (_, prompt) = run_next(&db, "feedback_scan", json!({ "found": [{ "source": 1, "request": 1, "title": "", "detail": "", "quote": "news on Salesforce" }] })).await;
        assert!(prompt.contains("1. Salesforce sync") && prompt.contains("Any news on Salesforce?") && !prompt.contains("twelve seats"), "{prompt}");
        let mentions: i64 = sqlx::query_scalar("select count(*) from feedback_mentions where request_id = $1").bind(request_id).fetch_one(&db).await.unwrap();
        assert_eq!(mentions, 2);
        request(&db, t.project, t.org).await.unwrap();
        let (outcome, _) = run_next(&db, "feedback_scan", json!({ "found": [] })).await;
        assert_eq!(outcome.message, "Nothing new from buyers to read");

        let run = ship(&db, request_id, t.project, t.org, "Two-way Salesforce sync is live: https://fieldnote.example/salesforce", true).await.unwrap();
        assert!(run.is_some());
        let (outcome, prompt) =
            run_next(&db, "feature_shipped", json!({ "subject": "Salesforce sync is here", "body": "Hi Ines, you asked for it and it's live." })).await;
        assert_eq!(outcome.output["drafted"], 1, "one email per person, however many times they asked");
        assert!(prompt.contains("\"news on Salesforce\"") && prompt.contains("https://fieldnote.example/salesforce") && prompt.contains("still talking"), "{prompt}");
        let (kind, to): (String, String) = sqlx::query_as("select kind, to_email::text from email_messages where status = 'draft'").fetch_one(&db).await.unwrap();
        assert_eq!((kind.as_str(), to.as_str()), ("feature_shipped", "ines@lumenfold.example"));

        ship(&db, request_id, t.project, t.org, "Again", true).await.unwrap();
        let (outcome, _) = run_next(&db, "feature_shipped", json!({ "subject": "x", "body": "y" })).await;
        assert_eq!(outcome.output["drafted"], 0, "nobody hears twice");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_scan_that_stops_among_sources_from_the_same_moment_reads_the_rest_next(db: PgPool) {
        let t = crate::agent::practice::fixture::team(&db).await;
        // One statement, so all of them have the same created_at, like emails imported together.
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, body, status)
             select $1, d.lead_id, 'inbound', 'inbound', 'Ask ' || n, 'received' from deals d, generate_series(1, $2::int + 1) n where d.id = $3",
        )
        .bind(t.project)
        .bind(MAX_SOURCES as i32)
        .bind(t.deal)
        .execute(&db)
        .await
        .unwrap();
        request(&db, t.project, t.org).await.unwrap();
        let (outcome, _) = run_next(&db, "feedback_scan", json!({ "found": [] })).await;
        assert_eq!(outcome.output["read"], MAX_SOURCES);
        request(&db, t.project, t.org).await.unwrap();
        let (outcome, _) = run_next(&db, "feedback_scan", json!({ "found": [] })).await;
        assert_eq!(outcome.output["read"], 1);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn two_asks_for_a_scan_at_once_start_one(db: PgPool) {
        let t = crate::agent::practice::fixture::team(&db).await;
        let first = request(&db, t.project, t.org).await.unwrap();
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(first).execute(&db).await.unwrap();
        // Both asks wait on the project's row, held here, before either goes on.
        let mut hold = db.begin().await.unwrap();
        sqlx::query("select 1 from feedback_scans where project_id = $1 for update").bind(t.project).execute(&mut *hold).await.unwrap();
        let ask = || {
            let (db, project, org) = (db.clone(), t.project, t.org);
            tokio::spawn(async move { request(&db, project, org).await })
        };
        let (a, b) = (ask(), ask());
        let mut waiting = 0;
        for _ in 0..250 {
            waiting = sqlx::query_scalar::<_, i64>("select count(*) from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock'")
                .fetch_one(&db)
                .await
                .unwrap();
            if waiting == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(waiting, 2, "both asks wait on the lock, so the test races them");
        hold.commit().await.unwrap();
        assert_eq!(a.await.unwrap().unwrap(), b.await.unwrap().unwrap());
        let queued: i64 = sqlx::query_scalar("select count(*) from agent_runs where kind = 'feedback_scan' and status = 'queued'").fetch_one(&db).await.unwrap();
        assert_eq!(queued, 1);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn people_past_the_first_run_hear_it_shipped_even_when_its_emails_came_back_empty(db: PgPool) {
        let t = crate::agent::practice::fixture::team(&db).await;
        let request_id: Uuid =
            sqlx::query_scalar("insert into feedback_requests (project_id, title) values ($1, 'SSO') returning id").bind(t.project).fetch_one(&db).await.unwrap();
        sqlx::query(
            "with l as (
               insert into leads (project_id, full_name, email, status)
               select $1, 'Asker ' || n, 'asker' || n || '@example.com', 'new' from generate_series(1, $3::int + 1) n returning id)
             insert into feedback_mentions (request_id, lead_id, source, source_id, quote, said_at)
             select $2, id, 'email', gen_random_uuid(), 'Do you have SSO?', now() from l",
        )
        .bind(t.project)
        .bind(request_id)
        .bind(MAX_NOTICES as i32)
        .execute(&db)
        .await
        .unwrap();
        ship(&db, request_id, t.project, t.org, "SSO is live", true).await.unwrap();
        let (outcome, _) = run_next(&db, "feature_shipped", json!({ "subject": "SSO", "body": " " })).await;
        assert_eq!(outcome.output, json!({ "drafted": 0, "skipped": MAX_NOTICES }));
        let (outcome, _) = run_next(&db, "feature_shipped", json!({ "subject": "SSO is here", "body": "It's live." })).await;
        assert_eq!(outcome.output, json!({ "drafted": 1, "skipped": 0 }), "the next run takes whoever is left");
    }
}
