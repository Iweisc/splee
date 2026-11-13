//! Inbound replies: classify them, act on the result, and draft an answer with the booking link.

use anyhow::Context;
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::write::WriterSettings;
use super::{Ctx, Outcome, kind};
use crate::{hooks, mail};
use crate::models::{EmailMessage, Lead, Project};
use crate::state::AppState;

const READER: &str = "You read replies to B2B cold emails for Splee, an outbound sales tool, and decide what they mean. \
Judge only from the text. Automatic replies are never hot.";

const REPLIER: &str = "You answer replies to B2B cold emails on behalf of the sender. Be brief, warm and honest. Use only facts \
from the prompt. If you don't know an answer, say you'll find out and suggest a quick call. Never invent prices, features, \
clients or availability. Never use placeholders. Plain text only.";

#[derive(Deserialize)]
struct MessageInput {
    message_id: Uuid,
}

struct Thread {
    inbound: EmailMessage,
    lead: Lead,
    project: Project,
    /// Everything before the inbound message, oldest first.
    history: Vec<EmailMessage>,
}

async fn load_thread(ctx: &Ctx) -> anyhow::Result<Thread> {
    let input: MessageInput = ctx.input()?;
    let inbound = sqlx::query_as::<_, EmailMessage>("select * from email_messages where id = $1 and direction = 'inbound'")
        .bind(input.message_id)
        .fetch_optional(ctx.db())
        .await?
        .context("reply not found")?;
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(inbound.lead_id).fetch_one(ctx.db()).await?;
    let (project, _) = load_project(ctx, inbound.project_id).await?;
    let history = sqlx::query_as::<_, EmailMessage>(
        "select * from (select * from email_messages
                        where lead_id = $1 and id <> $2 and created_at <= $3 and (direction = 'inbound' or status = 'sent')
                        order by created_at desc limit 6) t
         order by created_at",
    )
    .bind(lead.id)
    .bind(inbound.id)
    .bind(inbound.created_at)
    .fetch_all(ctx.db())
    .await?;
    Ok(Thread { inbound, lead, project, history })
}

fn transcript(thread: &Thread) -> String {
    let mut out = String::new();
    for m in thread.history.iter().chain(std::iter::once(&thread.inbound)) {
        let who = if m.direction == "inbound" { "Them" } else { "Us" };
        let body: String = m.body.chars().take(2500).collect();
        out.push_str(&format!("--- {who} ({}) ---\nSubject: {}\n{}\n\n", m.created_at.format("%Y-%m-%d"), m.subject, body.trim()));
    }
    out
}

#[derive(Deserialize)]
struct Verdict {
    classification: String,
    reason: String,
    #[serde(default)]
    return_date: String,
}

/// The reader is Claude, so a reply in any language is read as it is. The examples keep it from taking a German out-of-office
/// or a Spanish "remove me" for "other", which would leave the lead in the sequence.
fn classify_prompt(thread: &Thread) -> String {
    format!(
        "Classify the last message in this email thread (it's their reply to our cold outreach).\n\n{}\n\
         The reply can be in any language. Read it in its own language and sort it the same way, for example: \"bitte abmelden\", \
         \"désinscrivez-moi\", \"darme de baja\", \"me descadastre\" or \"afmelden\" is unsubscribe, \"ich bin abwesend\", \"je suis \
         absent\", \"fuera de la oficina\", \"ausente\" or \"afwezig\" in an automatic reply is ooo, and \"kein Interesse\", \"pas \
         intéressé\", \"no nos interesa\", \"não temos interesse\" or \"geen interesse\" is not_interested.\n\
         Categories:\n\
         - hot: interest that moves a deal forward: wants a call or demo, asks for pricing, samples or a proposal, says yes, or points to the colleague who decides.\n\
         - question: asks something before deciding, without clear interest yet.\n\
         - not_interested: declines, says it's not relevant, or already has a supplier.\n\
         - ooo: an automatic out-of-office or vacation reply.\n\
         - unsubscribe: asks to stop emailing or to be removed.\n\
         - bounce: a delivery failure notice.\n\
         - other: anything else.\n\
         Also give the reason in one short sentence, in English, and return_date (YYYY-MM-DD) when an out-of-office says when they \
         are back, else \"\".",
        transcript(thread)
    )
}

pub async fn classify_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let thread = load_thread(ctx).await?;
    let prompt = classify_prompt(&thread);
    let schema = json!({
        "type": "object",
        "properties": {
            "classification": {"type": "string", "enum": ["hot", "question", "not_interested", "ooo", "unsubscribe", "bounce", "other"]},
            "reason": {"type": "string"},
            "return_date": {"type": "string"}
        },
        "required": ["classification", "reason", "return_date"]
    });
    let verdict: Verdict = ctx.ask(&Call::writing(READER, prompt, schema)).await?;
    sqlx::query("update email_messages set classification = $2, classification_reason = $3 where id = $1")
        .bind(thread.inbound.id)
        .bind(&verdict.classification)
        .bind(verdict.reason.trim())
        .execute(ctx.db())
        .await?;
    apply_verdict(ctx, &thread, &verdict).await?;
    Ok(Outcome {
        message: format!("Classified as {}", verdict.classification),
        output: json!({"classification": verdict.classification, "reason": verdict.reason}),
    })
}

async fn apply_verdict(ctx: &Ctx, t: &Thread, verdict: &Verdict) -> anyhow::Result<()> {
    let db = ctx.db();
    let lead_id = t.lead.id;
    match verdict.classification.as_str() {
        "ooo" => {
            // They'll be back, so keep the sequence going from their return date.
            let back = NaiveDate::parse_from_str(verdict.return_date.trim(), "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(9, 0, 0))
                .map(|d| d.and_utc())
                .filter(|d| *d > Utc::now())
                .unwrap_or_else(|| Utc::now() + chrono::Duration::days(7));
            sqlx::query("update leads set status = 'contacted' where id = $1 and status = 'replied'").bind(lead_id).execute(db).await?;
            sqlx::query(
                "update email_messages set scheduled_for = greatest(coalesce(scheduled_for, now()), $2)
                 where lead_id = $1 and kind = 'follow_up' and status in ('draft', 'approved', 'scheduled')",
            )
            .bind(lead_id)
            .bind(back)
            .execute(db)
            .await?;
            sqlx::query("update linkedin_messages set due_at = greatest(due_at, $2) where lead_id = $1 and status = 'todo' and due_at is not null")
                .bind(lead_id)
                .bind(back)
                .execute(db)
                .await?;
            sqlx::query("update text_messages set due_at = greatest(due_at, $2) where lead_id = $1 and status in ('draft', 'approved')")
                .bind(lead_id)
                .bind(back)
                .execute(db)
                .await?;
            return Ok(());
        }
        "unsubscribe" => {
            mail::suppress_lead(&ctx.state, &t.lead, t.project.org_id, "unsubscribed", "unsubscribe").await?;
            return Ok(());
        }
        "bounce" => {
            mail::suppress_lead(&ctx.state, &t.lead, t.project.org_id, "bounced", "bounce").await?;
            if let Some(mailbox_id) = t.inbound.mailbox_id {
                mail::pause_if_bouncing(db, mailbox_id).await?;
            }
            return Ok(());
        }
        _ => {}
    }

    let status = match verdict.classification.as_str() {
        "hot" => "hot",
        "not_interested" => "not_interested",
        _ => "replied",
    };
    sqlx::query("update leads set status = $2 where id = $1 and status not in ('meeting_booked', 'unsubscribed', 'bounced', 'do_not_contact')")
        .bind(lead_id)
        .bind(status)
        .execute(db)
        .await?;
    // A person answered, so the automated sequence stops here, on LinkedIn too.
    sqlx::query("update email_messages set status = 'cancelled' where lead_id = $1 and kind = 'follow_up' and status in ('draft', 'approved', 'scheduled')")
        .bind(lead_id)
        .execute(db)
        .await?;
    sqlx::query(mail::STOP_LINKEDIN).bind(lead_id).execute(db).await?;
    sqlx::query(crate::twilio::STOP_TEXTS).bind(lead_id).bind("They replied").execute(db).await?;
    sqlx::query(crate::calls::STOP_CALLS).bind(lead_id).bind("They replied").execute(db).await?;

    let reply = |classification: &str| {
        json!({
            "id": t.inbound.id, "subject": t.inbound.subject, "body": t.inbound.body,
            "classification": classification, "reason": verdict.reason,
        })
    };
    hooks::send_lead_event(&ctx.state, t.project.id, "lead_replied", &t.lead, Some(("reply", reply(&verdict.classification)))).await;
    if verdict.classification == "hot" {
        // Before the alerts, so the CRM note says the deal is open.
        crate::deals::hot_reply(&ctx.state, &t.lead).await;
        hooks::send_lead_event(&ctx.state, t.project.id, "hot_lead", &t.lead, Some(("reply", reply("hot")))).await;
        notify_hot(&ctx.state, t, &verdict.reason).await;
    }
    if matches!(verdict.classification.as_str(), "hot" | "question") {
        let mode: Option<String> = sqlx::query_scalar("select auto_reply_mode from project_settings where project_id = $1")
            .bind(t.project.id)
            .fetch_optional(db)
            .await?;
        // Drafts are emails, which would answer a text on the wrong channel.
        if mode.as_deref().is_some_and(|m| m != "off") && t.inbound.channel == "email" {
            super::enqueue(db, kind::DRAFT_REPLY, Some(t.project.id), t.project.org_id, json!({"message_id": t.inbound.id})).await?;
        }
        // In approve mode the Slack message waits for the draft, so it can offer to send it. draft_run posts it then.
        if verdict.classification == "hot" && mode.as_deref() != Some("approve") {
            crate::slack::post_hot_reply(&ctx.state, lead_id).await;
        }
    }
    Ok(())
}

/// Chat, CRM and email alerts for a hot lead. The webhook gets it as a lead event. Failures are logged, they never fail the run.
async fn notify_hot(state: &AppState, t: &Thread, reason: &str) {
    let link = hooks::lead_url(&state.cfg.public_url, t.project.id, t.lead.id);
    let who = format!("{} ({})", t.lead.full_name, t.lead.company_name);
    let snippet: String = t.inbound.body.trim().chars().take(300).collect();
    let summary = format!("{who} replied to {}", t.project.name);
    let alert = hooks::ChatAlert { label: "Hot lead", summary: &summary, quote: Some(&snippet), link: &link, link_text: "Open the inbox" };
    hooks::post_chat(&state.db, t.project.id, &alert).await;
    let note = format!("Hot lead in Splee. They replied:\n\n{}\n\nWhy it's hot: {reason}\n{link}", t.inbound.body.trim());
    crate::crm::sync(state, t.project.id, &t.lead, &note).await;

    let Some(org_id) = t.project.org_id else { return };
    let recipients = crate::ownership::hot_lead_recipients(&state.db, org_id, t.lead.owner_id).await.unwrap_or_default();
    let body = format!("{who} replied to your outreach for {}:\n\n{}\n\nWhy it's hot: {reason}\n\n{link}", t.project.name, t.inbound.body.trim());
    for to in recipients {
        if let Err(e) = mail::send_system_email(state, org_id, &to, &format!("Hot lead: {who}"), &body).await {
            tracing::warn!(error = ?e, "hot lead email failed");
        }
    }
}

#[derive(Deserialize)]
struct ReplyDraft {
    body: String,
}

/// Someone asked for this draft from the inbox, so it is written even when the message already got one, and always waits
/// for approval.
#[derive(Deserialize)]
struct DraftInput {
    #[serde(default)]
    manual: bool,
}

pub async fn draft_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let t = load_thread(ctx).await?;
    let manual = ctx.input::<DraftInput>()?.manual;
    let already: bool = sqlx::query_scalar("select exists (select 1 from email_messages where lead_id = $1 and kind = 'auto_reply' and in_reply_to = $2)")
        .bind(t.lead.id)
        .bind(&t.inbound.message_id)
        .fetch_one(ctx.db())
        .await?;
    if already && !manual {
        return Ok(Outcome::new("A reply is already drafted"));
    }
    let (_, profile) = load_project(ctx, t.project.id).await?;
    let settings = WriterSettings::load(ctx, t.project.id).await?;
    let mode: String = sqlx::query_scalar("select auto_reply_mode from project_settings where project_id = $1")
        .bind(t.project.id)
        .fetch_optional(ctx.db())
        .await?
        .unwrap_or_else(|| "approve".into());
    let faq: Vec<(String, String)> =
        sqlx::query_as("select title, content from materials where project_id = $1 and kind in ('faq', 'case_study', 'doc', 'note') limit 10")
            .bind(t.project.id)
            .fetch_all(ctx.db())
            .await?;
    let faq = faq.iter().map(|(title, content)| format!("- {title}: {}", content.chars().take(600).collect::<String>())).collect::<Vec<_>>().join("\n");

    let next_step = if settings.booking_link.trim().is_empty() {
        "Suggest a short call and ask which day and time suits them.".to_string()
    } else {
        format!("Offer a short call with the booking link {} on its own line.", settings.booking_link.trim())
    };
    // They may answer in another language than we wrote in, and the reply follows them. A one-word answer can't tell.
    let lang = crate::language::reply_language(&t.inbound.body, crate::language::for_lead(ctx.db(), t.lead.id).await?);
    let prompt = format!(
        "Write our reply to the last message in this thread, from {sender}.\n\n{thread}\
         About us:\n{brief}\n{faq_block}\n\
         Answer what they asked in a sentence or two using only the facts above, then move toward a meeting. {next_step} \
         At most 80 words. Start with a greeting and their first name. Tone: {tone}. No sign-off.{extra}\n{language}",
        sender = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) },
        thread = transcript(&t),
        brief = profile.brief(t.project.domain.as_deref()),
        faq_block = if faq.is_empty() { String::new() } else { format!("Facts and FAQ:\n{faq}\n") },
        tone = settings.tone,
        language = crate::language::instruction(lang),
        extra = if settings.auto_reply_instructions.trim().is_empty() {
            String::new()
        } else {
            format!(" Also: {}", settings.auto_reply_instructions.trim())
        },
    );
    let schema = json!({"type": "object", "properties": {"body": {"type": "string"}}, "required": ["body"]});
    let draft: ReplyDraft = ctx.ask(&Call::writing(REPLIER, prompt, schema)).await?;

    let subject = if t.inbound.subject.to_lowercase().starts_with("re:") { t.inbound.subject.clone() } else { format!("Re: {}", t.inbound.subject) };
    let auto = mode == "auto" && !manual;
    // Writing takes a few seconds. If someone answered the lead by hand meanwhile, this draft would be a second answer.
    // The lead lock makes this check and the manual reply's cancelling of drafts take turns.
    let mut tx = ctx.db().begin().await?;
    sqlx::query("select 1 from leads where id = $1 for update").bind(t.lead.id).execute(&mut *tx).await?;
    let answered: bool = sqlx::query_scalar("select exists (select 1 from email_messages where lead_id = $1 and kind = 'manual_reply' and created_at >= $2)")
        .bind(t.lead.id)
        .bind(ctx.run.created_at)
        .fetch_one(&mut *tx)
        .await?;
    if answered {
        return Ok(Outcome::new("Not drafted: the lead was answered by hand meanwhile"));
    }
    sqlx::query(
        "insert into email_messages (project_id, lead_id, segment_id, mailbox_id, direction, kind, subject, body, status, scheduled_for,
                                     in_reply_to, to_email, language)
         values ($1, $2, $3, $4, 'outbound', 'auto_reply', $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(t.project.id)
    .bind(t.lead.id)
    .bind(t.lead.segment_id)
    .bind(t.inbound.mailbox_id)
    .bind(subject)
    .bind(settings.sign(&draft.body))
    .bind(if auto { "scheduled" } else { "draft" })
    .bind(auto.then(Utc::now))
    .bind(&t.inbound.message_id)
    .bind(t.inbound.from_email.as_ref().or(t.lead.email.as_ref()))
    .bind(lang.code)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if !auto && !manual && t.inbound.classification.as_deref() == Some("hot") {
        crate::slack::post_hot_reply(&ctx.state, t.lead.id).await;
    }
    Ok(Outcome::new(if auto { "Reply sent to the outbox" } else { "Reply drafted for approval" }))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use serde_json::{Value, json};
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::{Ctx, classify_run, draft_run};
    use crate::ab_test::tests::{add_leads, seed};
    use crate::agent::fake;
    use crate::models::AgentRun;

    async fn run(db: &PgPool, project: Uuid, kind: &str, message: Uuid, answer: Value) -> Ctx {
        let run = sqlx::query_as::<_, AgentRun>("insert into agent_runs (project_id, kind, input) values ($1, $2, $3) returning *")
            .bind(project)
            .bind(kind)
            .bind(json!({ "message_id": message }))
            .fetch_one(db)
            .await
            .unwrap();
        fake::answer_with(run.id, move |_| answer.clone());
        Ctx { state: crate::autopilot::tests::state(db.clone()), run, step: AtomicI32::new(0) }
    }

    async fn reply(db: &PgPool, project: Uuid, lead: Uuid, body: &str) -> Uuid {
        sqlx::query_scalar(
            "insert into email_messages (project_id, lead_id, direction, kind, status, subject, body, message_id, from_email)
             values ($1, $2, 'inbound', 'inbound', 'received', 'Re: Fieldnote x Brauhaus', $3, gen_random_uuid()::text, 'anna@brauhaus.example')
             returning id",
        )
        .bind(project)
        .bind(lead)
        .bind(body)
        .fetch_one(db)
        .await
        .unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_drafted_reply_answers_in_the_language_they_wrote_in(db: PgPool) {
        let c = seed(&db, 0).await;
        sqlx::query("update segments set language = 'lead' where id = $1").bind(c.segment).execute(&db).await.unwrap();
        let lead = add_leads(&db, &c, 1).await.remove(0);
        sqlx::query("update leads set country_code = 'DE' where id = $1").bind(lead.id).execute(&db).await.unwrap();

        // We wrote in German, but they answered in French, so the reply is French.
        let french = reply(&db, c.project, lead.id, "Bonjour, merci pour votre message. Pouvez-vous m'envoyer vos tarifs et me dire comment se passe la mise en place ?").await;
        let ctx = run(&db, c.project, "draft_reply", french, json!({ "body": "Bonjour Anna" })).await;
        draft_run(&ctx).await.unwrap();
        let prompt = &fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("write in French") && prompt.contains("\"vous\""), "{prompt}");

        // Too short to tell, so it goes by the lead's own language.
        let short = reply(&db, c.project, lead.id, "Ja, gerne!").await;
        let ctx = run(&db, c.project, "draft_reply", short, json!({ "body": "Guten Tag Anna Weber" })).await;
        draft_run(&ctx).await.unwrap();
        let prompt = &fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("write in German") && prompt.contains("\"Sie\""), "{prompt}");

        let drafts: Vec<String> =
            sqlx::query_scalar("select language from email_messages where kind = 'auto_reply' order by created_at").fetch_all(&db).await.unwrap();
        assert_eq!(drafts, ["fr", "de"]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_reader_sorts_replies_in_any_language(db: PgPool) {
        let c = seed(&db, 0).await;
        let lead = add_leads(&db, &c, 1).await.remove(0);
        let message = reply(&db, c.project, lead.id, "Bitte melden Sie mich von Ihrer Liste ab.").await;
        let verdict = json!({ "classification": "unsubscribe", "reason": "They asked to be removed.", "return_date": "" });
        let ctx = run(&db, c.project, "classify_reply", message, verdict).await;
        classify_run(&ctx).await.unwrap();
        let prompt = &fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("Bitte melden Sie mich von Ihrer Liste ab.") && prompt.contains("The reply can be in any language"), "{prompt}");
        for phrase in ["bitte abmelden", "darme de baja", "désinscrivez-moi", "ich bin abwesend", "fuera de la oficina", "kein Interesse", "no nos interesa"] {
            assert!(prompt.contains(phrase), "{phrase} missing from {prompt}");
        }
        let status: String = sqlx::query_scalar("select status from leads where id = $1").bind(lead.id).fetch_one(&db).await.unwrap();
        assert_eq!(status, "unsubscribed");
    }
}
