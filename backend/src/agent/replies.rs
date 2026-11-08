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

pub async fn classify_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let thread = load_thread(ctx).await?;
    let prompt = format!(
        "Classify the last message in this email thread (it's their reply to our cold outreach).\n\n{}\n\
         Categories:\n\
         - hot: interest that moves a deal forward: wants a call or demo, asks for pricing, samples or a proposal, says yes, or points to the colleague who decides.\n\
         - question: asks something before deciding, without clear interest yet.\n\
         - not_interested: declines, says it's not relevant, or already has a supplier.\n\
         - ooo: an automatic out-of-office or vacation reply.\n\
         - unsubscribe: asks to stop emailing or to be removed.\n\
         - bounce: a delivery failure notice.\n\
         - other: anything else.\n\
         Also give the reason in one short sentence, and return_date (YYYY-MM-DD) when an out-of-office says when they are back, else \"\".",
        transcript(&thread)
    );
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
            return Ok(());
        }
        "unsubscribe" => {
            mail::suppress_lead(db, &t.lead, t.project.org_id, "unsubscribed", "unsubscribe").await?;
            return Ok(());
        }
        "bounce" => {
            mail::suppress_lead(db, &t.lead, t.project.org_id, "bounced", "bounce").await?;
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
    // A person answered, so the automated sequence stops here.
    sqlx::query("update email_messages set status = 'cancelled' where lead_id = $1 and kind = 'follow_up' and status in ('draft', 'approved', 'scheduled')")
        .bind(lead_id)
        .execute(db)
        .await?;

    if verdict.classification == "hot" {
        notify_hot(&ctx.state, t, &verdict.reason).await;
    }
    if matches!(verdict.classification.as_str(), "hot" | "question") {
        let mode: Option<String> = sqlx::query_scalar("select auto_reply_mode from project_settings where project_id = $1")
            .bind(t.project.id)
            .fetch_optional(db)
            .await?;
        if mode.as_deref().is_some_and(|m| m != "off") {
            super::enqueue(db, kind::DRAFT_REPLY, Some(t.project.id), t.project.org_id, json!({"message_id": t.inbound.id})).await?;
        }
    }
    Ok(())
}

/// Webhook, Slack and email alerts for a hot lead. Failures are logged, they never fail the run.
async fn notify_hot(state: &AppState, t: &Thread, reason: &str) {
    let hooks: Option<(String, String, String)> =
        sqlx::query_as("select webhook_url, slack_webhook_url, webhook_secret from project_settings where project_id = $1")
        .bind(t.project.id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let (webhook, slack, secret) = hooks.unwrap_or_default();
    let link = format!("{}/app/p/{}/inbox", state.cfg.public_url, t.project.id);
    let who = format!("{} ({})", t.lead.full_name, t.lead.company_name);

    if !webhook.trim().is_empty() {
        let payload = json!({
            "event": "hot_lead",
            "project": {"id": t.project.id, "name": t.project.name},
            "lead": {
                "id": t.lead.id, "full_name": t.lead.full_name, "email": t.lead.email, "title": t.lead.title,
                "company_name": t.lead.company_name, "company_domain": t.lead.company_domain,
                "linkedin_url": t.lead.linkedin_url, "x_url": t.lead.x_url,
            },
            "reply": {"id": t.inbound.id, "subject": t.inbound.subject, "body": t.inbound.body, "reason": reason},
            "url": link,
        });
        let result = hooks::post_json_retrying(&webhook, &payload, Some(&secret)).await;
        if let Err(e) = &result {
            tracing::warn!(error = %e, "hot lead webhook failed");
        }
        hooks::record(&state.db, t.project.id, hooks::Alert::Webhook, &webhook, &result).await;
    }
    if !slack.trim().is_empty() {
        let snippet: String = t.inbound.body.trim().chars().take(300).collect();
        let text = format!("*Hot lead:* {who} replied to {}\n> {}\n<{link}|Open the inbox>", t.project.name, snippet.replace('\n', "\n> "));
        let result = hooks::post_json_retrying(&slack, &json!({"text": text}), None).await;
        if let Err(e) = &result {
            tracing::warn!(error = %e, "slack notification failed");
        }
        hooks::record(&state.db, t.project.id, hooks::Alert::Slack, &slack, &result).await;
    }

    let Some(org_id) = t.project.org_id else { return };
    let recipients: Vec<String> = sqlx::query_scalar(
        "select u.email::text from memberships m join users u on u.id = m.user_id
         left join notification_prefs n on n.user_id = u.id
         where m.org_id = $1 and coalesce(n.hot_lead_email, true)",
    )
    .bind(org_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
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

pub async fn draft_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let t = load_thread(ctx).await?;
    let already: bool = sqlx::query_scalar("select exists (select 1 from email_messages where lead_id = $1 and kind = 'auto_reply' and in_reply_to = $2)")
        .bind(t.lead.id)
        .bind(&t.inbound.message_id)
        .fetch_one(ctx.db())
        .await?;
    if already {
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
    let prompt = format!(
        "Write our reply to the last message in this thread, from {sender}.\n\n{thread}\
         About us:\n{brief}\n{faq_block}\n\
         Answer what they asked in a sentence or two using only the facts above, then move toward a meeting. {next_step} \
         At most 80 words. Reply in the language of their message, starting with a greeting and their first name. Tone: {tone}. No sign-off.{extra}",
        sender = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) },
        thread = transcript(&t),
        brief = profile.brief(t.project.domain.as_deref()),
        faq_block = if faq.is_empty() { String::new() } else { format!("Facts and FAQ:\n{faq}\n") },
        tone = settings.tone,
        extra = if settings.auto_reply_instructions.trim().is_empty() {
            String::new()
        } else {
            format!(" Also: {}", settings.auto_reply_instructions.trim())
        },
    );
    let schema = json!({"type": "object", "properties": {"body": {"type": "string"}}, "required": ["body"]});
    let draft: ReplyDraft = ctx.ask(&Call::writing(REPLIER, prompt, schema)).await?;

    let subject = if t.inbound.subject.to_lowercase().starts_with("re:") { t.inbound.subject.clone() } else { format!("Re: {}", t.inbound.subject) };
    let auto = mode == "auto";
    sqlx::query(
        "insert into email_messages (project_id, lead_id, segment_id, mailbox_id, direction, kind, subject, body, status, scheduled_for,
                                     in_reply_to, to_email)
         values ($1, $2, $3, $4, 'outbound', 'auto_reply', $5, $6, $7, $8, $9, $10)",
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
    .execute(ctx.db())
    .await?;
    Ok(Outcome::new(if auto { "Reply sent to the outbox" } else { "Reply drafted for approval" }))
}
