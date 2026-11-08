//! Email delivery: SMTP sending, IMAP reply polling, follow-ups, DNS checks.

pub mod dns;
mod imap;
mod inbound;
mod notify;
mod sender;
mod smtp;

use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{Lead, Mailbox};
use crate::state::AppState;

pub fn spawn_workers(state: AppState) {
    let s = state.clone();
    if !s.cfg.sending_enabled {
        tracing::warn!("SENDING_ENABLED=false: approved emails, follow-ups and auto-replies wait");
    } else {
        tokio::spawn(async move {
            // A send that was interrupted by a restart never got its SMTP answer. Try it again.
            if let Err(e) = sqlx::query("update email_messages set status = 'approved' where status = 'sending'").execute(&s.db).await {
                tracing::error!(error = ?e, "could not reset interrupted sends");
            }
            every(Duration::from_secs(30), || sender::tick(&s), "sender").await;
        });
    }
    let s = state.clone();
    tokio::spawn(async move { every(Duration::from_secs(60), || inbound::tick(&s), "imap poller").await });
    let s = state.clone();
    tokio::spawn(async move { every(Duration::from_secs(3600), || sender::optimize(&s), "optimizer").await });
    // Checked every 15 minutes so the 08:00 UTC window is never skipped. notification_log prevents doubles.
    tokio::spawn(async move { every(Duration::from_secs(900), || notify::digests(&state), "digests").await });
}

pub(crate) async fn every<F, Fut>(period: Duration, mut job: F, name: &str)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        if let Err(e) = job().await {
            tracing::error!(error = ?e, "{name} failed");
        }
    }
}

/// Connects to SMTP (and IMAP when configured) with the mailbox credentials.
pub async fn test_mailbox(state: &AppState, mailbox: &Mailbox) -> anyhow::Result<()> {
    smtp::test(state, mailbox).await?;
    if !mailbox.imap_host.trim().is_empty() {
        imap::test(state, mailbox).await?;
    }
    Ok(())
}

/// Looks up MX, SPF and DMARC for the mailbox domain. Returns the JSON stored in mailboxes.dns.
pub async fn check_dns(domain: &str) -> anyhow::Result<serde_json::Value> {
    dns::check_dns(domain).await
}

/// Sends a one-off email (e.g. a team invite) from the org's first active mailbox.
/// Returns Ok(false) when the org has no active mailbox.
pub async fn send_system_email(state: &AppState, org_id: Uuid, to: &str, subject: &str, body: &str) -> anyhow::Result<bool> {
    let mailbox = sqlx::query_as::<_, Mailbox>("select * from mailboxes where org_id = $1 and status = 'active' order by created_at limit 1")
        .bind(org_id)
        .fetch_optional(&state.db)
        .await?;
    let Some(mb) = mailbox else { return Ok(false) };
    let message_id = sender::new_message_id(&mb.email);
    let message = smtp::build(
        &mb,
        &smtp::Outgoing {
            to_name: "",
            to_email: to,
            subject,
            body,
            message_id: &message_id,
            in_reply_to: None,
            references: &[],
            unsubscribe_url: None,
        },
    )?;
    smtp::send(&smtp::transport(state, &mb)?, message).await?;
    Ok(true)
}

/// An email as its lead would get it, sent to `to` from the org's first active mailbox. Returns false without one.
/// The footer's unsubscribe link goes nowhere, so clicking it in the test can't unsubscribe the real lead.
pub async fn send_test(state: &AppState, org_id: Uuid, to: &str, kind: &str, subject: &str, body: &str) -> anyhow::Result<bool> {
    let body = sender::with_footer(kind, body, &format!("{}/unsubscribe/test", state.cfg.public_url));
    send_system_email(state, org_id, to, subject, &body).await
}

/// Stops all outreach to a lead: sets its status, suppresses the address for the org and cancels unsent emails.
pub async fn suppress_lead(db: &PgPool, lead: &Lead, org_id: Option<Uuid>, status: &str, reason: &str) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("update leads set status = $2 where id = $1").bind(lead.id).bind(status).execute(&mut *tx).await?;
    if let (Some(org_id), Some(email)) = (org_id, &lead.email) {
        sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, $2, 'email', $3) on conflict (org_id, value) do nothing")
            .bind(org_id)
            .bind(email.to_lowercase())
            .bind(reason)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "update email_messages set status = 'cancelled', error = $2
         where lead_id = $1 and direction = 'outbound' and status in ('draft', 'approved', 'scheduled')",
    )
    .bind(lead.id)
    .bind(format!("Lead {status}"))
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}
