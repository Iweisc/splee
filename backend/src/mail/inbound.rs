//! IMAP poller: stores replies to our emails and hands them to the classifier.

use mail_parser::{HeaderValue, MessageParser, MimeHeaders};
use serde_json::json;
use uuid::Uuid;

use super::imap;
use crate::agent::{self, kind};
use crate::models::{EmailMessage, Lead, Mailbox};
use crate::state::AppState;

pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    let mailboxes = sqlx::query_as::<_, Mailbox>("select * from mailboxes where status in ('active', 'paused') and imap_host <> ''")
        .fetch_all(&state.db)
        .await?;
    for mb in mailboxes {
        if let Err(e) = poll(state, &mb).await {
            // Reading replies failing doesn't stop sending, so only record it.
            tracing::warn!(mailbox = %mb.email, error = ?e, "imap poll failed");
            sqlx::query("update mailboxes set last_error = $2 where id = $1").bind(mb.id).bind(format!("{e:#}")).execute(&state.db).await?;
        }
    }
    Ok(())
}

async fn poll(state: &AppState, mb: &Mailbox) -> anyhow::Result<()> {
    let after = u32::try_from(mb.imap_last_uid).unwrap_or(0);
    let messages = imap::fetch_new(state, mb, after).await?;
    for (uid, raw) in messages {
        if let Err(e) = store(state, mb, &raw).await {
            tracing::warn!(mailbox = %mb.email, uid, error = ?e, "could not store inbound message");
        }
        sqlx::query("update mailboxes set imap_last_uid = greatest(imap_last_uid, $2) where id = $1")
            .bind(mb.id)
            .bind(i64::from(uid))
            .execute(&state.db)
            .await?;
    }
    if !mb.last_error.is_empty() && mb.status == "active" {
        sqlx::query("update mailboxes set last_error = '' where id = $1").bind(mb.id).execute(&state.db).await?;
    }
    Ok(())
}

fn strip_brackets(id: &str) -> String {
    id.trim().trim_matches(['<', '>']).to_string()
}

fn header_ids(value: &HeaderValue) -> Vec<String> {
    value.as_text_list().map(|ids| ids.iter().map(|id| strip_brackets(id)).collect()).unwrap_or_default()
}

/// Every `<...@...>` token in the raw message. Bounce reports quote the original Message-ID somewhere in the body.
fn bracketed_ids(raw: &str) -> Vec<String> {
    raw.split('<')
        .skip(1)
        .filter_map(|rest| rest.split_once('>').map(|(id, _)| id))
        .filter(|id| id.contains('@') && !id.contains(char::is_whitespace) && id.len() < 200)
        .map(String::from)
        .collect()
}

/// The reply without the quoted thread under it.
fn strip_quoted(body: &str) -> String {
    let mut kept = vec![];
    for line in body.lines() {
        let t = line.trim();
        let quote_intro = (t.starts_with("On ") && t.ends_with("wrote:")) || t.starts_with("-----Original Message-----");
        if quote_intro || t.starts_with('>') {
            break;
        }
        kept.push(line);
    }
    // Gmail wraps a long "On <date> <name> <address> wrote:" line, so only its last piece ends with "wrote:".
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }
    if kept.last().is_some_and(|l| l.trim().ends_with("wrote:")) {
        let from = kept.len().saturating_sub(3);
        if let Some(start) = (from..kept.len()).rev().find(|&i| kept[i].trim().starts_with("On ")) {
            kept.truncate(start);
        }
    }
    let text = kept.join("\n").trim().to_string();
    if text.is_empty() { body.trim().to_string() } else { text }
}

async fn store(state: &AppState, mb: &Mailbox, raw: &[u8]) -> anyhow::Result<()> {
    let Some(msg) = MessageParser::default().parse(raw) else { return Ok(()) };
    let from = msg.from().and_then(|a| a.first()).and_then(|a| a.address()).map(str::to_lowercase).unwrap_or_default();
    let subject = msg.subject().unwrap_or_default().to_string();
    let message_id = msg.message_id().map(strip_brackets);
    let in_reply_to = header_ids(msg.in_reply_to());
    let mut thread_ids = in_reply_to.clone();
    thread_ids.extend(header_ids(msg.references()));
    let body = msg.body_text(0).map(|b| b.into_owned()).unwrap_or_default();

    let local = from.split('@').next().unwrap_or_default();
    let is_bounce = matches!(local, "mailer-daemon" | "postmaster") || msg.is_content_type("multipart", "report");
    if is_bounce {
        thread_ids.extend(bracketed_ids(&String::from_utf8_lossy(raw)));
    }

    if let Some(id) = &message_id {
        let seen: bool = sqlx::query_scalar("select exists (select 1 from email_messages where message_id = $1 and direction = 'inbound')")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
        if seen {
            return Ok(());
        }
    }

    let Some((lead, parent)) = match_lead(state, mb, &thread_ids, if is_bounce { None } else { Some(&from) }).await? else {
        return Ok(());
    };
    let project_org: Option<Uuid> = sqlx::query_scalar("select org_id from projects where id = $1").bind(lead.project_id).fetch_one(&state.db).await?;

    let text = if is_bounce { body.trim().chars().take(4000).collect() } else { strip_quoted(&body) };
    let inbound_id: Uuid = sqlx::query_scalar(
        "insert into email_messages (project_id, lead_id, segment_id, mailbox_id, direction, kind, step, subject, body, status, message_id,
                                     in_reply_to, from_email, to_email, classification, classification_reason)
         values ($1, $2, $3, $4, 'inbound', 'inbound', 0, $5, $6, 'received', $7, $8, $9, $10, $11, $12)
         returning id",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(lead.segment_id)
    .bind(mb.id)
    .bind(&subject)
    .bind(&text)
    .bind(&message_id)
    .bind(in_reply_to.first().cloned().or(parent.and_then(|p| p.message_id)))
    .bind(&from)
    .bind(&mb.email)
    .bind(is_bounce.then_some("bounce"))
    .bind(if is_bounce { "Delivery failed" } else { "" })
    .fetch_one(&state.db)
    .await?;

    if is_bounce {
        super::suppress_lead(&state.db, &lead, project_org, "bounced", "bounce").await?;
        return Ok(());
    }
    // Replied for now, so no follow-up goes out while the classifier works. It sets the final status.
    sqlx::query("update leads set status = 'replied' where id = $1 and status in ('new', 'queued', 'contacted')")
        .bind(lead.id)
        .execute(&state.db)
        .await?;
    agent::enqueue(&state.db, kind::CLASSIFY_REPLY, Some(lead.project_id), project_org, json!({"message_id": inbound_id})).await?;
    tracing::info!(lead = %lead.id, from = %from, "stored reply");
    Ok(())
}

/// Finds the lead by the thread headers first, then by sender address among leads we have emailed.
async fn match_lead(state: &AppState, mb: &Mailbox, ids: &[String], from: Option<&String>) -> anyhow::Result<Option<(Lead, Option<EmailMessage>)>> {
    if !ids.is_empty() {
        let parent = sqlx::query_as::<_, EmailMessage>(
            "select e.* from email_messages e join projects p on p.id = e.project_id
             where p.org_id = $1 and e.direction = 'outbound' and e.message_id = any($2)
             order by e.created_at desc limit 1",
        )
        .bind(mb.org_id)
        .bind(ids)
        .fetch_optional(&state.db)
        .await?;
        if let Some(parent) = parent {
            let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(parent.lead_id).fetch_one(&state.db).await?;
            return Ok(Some((lead, Some(parent))));
        }
    }
    let Some(from) = from.filter(|f| f.contains('@')) else { return Ok(None) };
    let lead = sqlx::query_as::<_, Lead>(
        "select l.* from leads l join projects p on p.id = l.project_id
         where p.org_id = $1 and l.email = $2
           and exists (select 1 from email_messages e where e.lead_id = l.id and e.direction = 'outbound' and e.status = 'sent')
         order by l.last_contacted_at desc nulls last limit 1",
    )
    .bind(mb.org_id)
    .bind(from)
    .fetch_optional(&state.db)
    .await?;
    Ok(lead.map(|l| (l, None)))
}

#[cfg(test)]
mod tests {
    use super::strip_quoted;

    #[test]
    fn drops_the_quoted_thread() {
        let body = "Sounds good, Tuesday works.\n\nOn Mon, Nov 9, 2026 at 10:00 AM Sarah Chen <sarah@northwind.example> wrote:\n> Hi Sarah,\n> quick question";
        assert_eq!(strip_quoted(body), "Sounds good, Tuesday works.");
    }

    #[test]
    fn drops_an_attribution_line_gmail_wrapped() {
        let body = "Sounds good, Tuesday works.\n\nOn Mon, Nov 9, 2026 at 10:00 AM Sarah Chen <\nsarah@northwind.example> wrote:\n\n> Hi Sarah,";
        assert_eq!(strip_quoted(body), "Sounds good, Tuesday works.");
    }

    #[test]
    fn drops_an_outlook_quote() {
        let body = "Not interested, thanks.\n\n-----Original Message-----\nFrom: Sam\nSubject: Hello";
        assert_eq!(strip_quoted(body), "Not interested, thanks.");
    }

    #[test]
    fn keeps_a_reply_that_is_only_quoted() {
        assert_eq!(strip_quoted("> only a quote"), "> only a quote");
    }
}
