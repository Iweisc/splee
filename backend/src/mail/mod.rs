//! Email delivery: SMTP sending, IMAP reply polling, follow-ups, DNS checks.

pub mod dns;
pub(crate) mod imap;
pub(crate) mod inbound;
mod notify;
pub mod oauth;
pub mod providers;
pub(crate) mod sender;
pub(crate) mod smtp;
pub mod warmup;

pub use notify::notify_team;
pub use sender::in_window;

use std::time::Duration;

use uuid::Uuid;

use crate::models::{Lead, Mailbox};
use crate::state::AppState;

pub fn spawn_workers(state: AppState) {
    let s = state.clone();
    if !s.cfg.sending_enabled {
        tracing::warn!("SENDING_ENABLED=false: approved emails, follow-ups, auto-replies, warmup emails, LinkedIn steps, texts, call steps, social posts and client reports wait");
    } else {
        tokio::spawn(async move {
            // A send that was interrupted by a restart never got its SMTP answer. Try it again.
            if let Err(e) = sqlx::query("update email_messages set status = 'approved' where status = 'sending'").execute(&s.db).await {
                tracing::error!(error = ?e, "could not reset interrupted sends");
            }
            every(Duration::from_secs(30), || sender::tick(&s), "sender").await;
        });
        // Its own loop, because testing a server that never answers takes 30 seconds and sending shouldn't wait on it.
        let s = state.clone();
        tokio::spawn(async move { every(Duration::from_secs(60), || sender::recheck_mailboxes(&s), "mailbox recheck").await });
        let s = state.clone();
        tokio::spawn(async move { every(Duration::from_secs(60), || warmup::tick(&s), "warmup").await });
        // Every 10 minutes, one step per project, which keeps the profile lookups under LinkedIn's daily limit too.
        let s = state.clone();
        tokio::spawn(async move { every(Duration::from_secs(600), || crate::unipile::tick(&s), "linkedin sender").await });
        let s = state.clone();
        tokio::spawn(async move {
            // Unlike an email, a text cut off by a restart may have reached Twilio, so it isn't sent again.
            let why = "Splee restarted while sending this, so it may or may not have gone out";
            if let Err(e) = sqlx::query("update text_messages set status = 'failed', error = $1 where status = 'sending'").bind(why).execute(&s.db).await {
                tracing::error!(error = ?e, "could not settle interrupted texts");
            }
            every(Duration::from_secs(60), || crate::twilio::tick(&s), "text sender").await;
        });
        let s = state.clone();
        tokio::spawn(async move { every(Duration::from_secs(60), || crate::calls::tick(&s.db), "call steps").await });
        // One post per account a tick, so 15 approved posts go out over a couple of hours.
        let s = state.clone();
        tokio::spawn(async move { every(Duration::from_secs(600), || crate::social::tick(&s), "social poster").await });
        let s = state.clone();
        tokio::spawn(async move { every(Duration::from_secs(600), || crate::social_channels::poll(&s), "discord and telegram reader").await });
        // Client reports go to people outside the team, so they wait with the rest of the sending.
        let s = state.clone();
        tokio::spawn(async move { every(Duration::from_secs(900), || crate::reports::email::tick(&s), "client reports").await });
    }
    let s = state.clone();
    tokio::spawn(async move { every(Duration::from_secs(60), || inbound::tick(&s), "imap poller").await });
    let s = state.clone();
    tokio::spawn(async move { every(Duration::from_secs(3600), || sender::optimize(&s.db), "optimizer").await });
    let s = state.clone();
    tokio::spawn(async move { every(Duration::from_secs(600), || crate::autopilot::tick(&s), "autopilot").await });
    let s = state.clone();
    tokio::spawn(async move { every(Duration::from_secs(60), || crate::hooks::announce_due_reminders(&s), "reminder alerts").await });
    let s = state.clone();
    tokio::spawn(async move { every(Duration::from_secs(60), || crate::rest_hooks::post_new_leads(&s), "new lead hooks").await });
    let s = state.clone();
    tokio::spawn(async move { every(Duration::from_secs(600), || crate::tracking::recheck_hosts(&s), "tracking hosts").await });
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
pub async fn check_dns(domain: &str, dkim_selector: Option<&str>) -> anyhow::Result<serde_json::Value> {
    dns::check_dns(domain, dkim_selector).await
}

/// Sends a one-off email (e.g. a team invite) from the org's first active mailbox.
/// Returns Ok(false) when the org has no active mailbox.
pub async fn send_system_email(state: &AppState, org_id: Uuid, to: &str, subject: &str, body: &str) -> anyhow::Result<bool> {
    send_system(state, org_id, to, subject, body, None, None).await
}

/// send_system_email with an HTML version and a List-Unsubscribe header, for the client reports.
pub async fn send_system_html(state: &AppState, org_id: Uuid, to: &str, subject: &str, text: &str, html: &str, unsubscribe_url: &str) -> anyhow::Result<bool> {
    send_system(state, org_id, to, subject, text, Some(html), Some(unsubscribe_url)).await
}

async fn send_system(
    state: &AppState,
    org_id: Uuid,
    to: &str,
    subject: &str,
    body: &str,
    html: Option<&str>,
    unsubscribe_url: Option<&str>,
) -> anyhow::Result<bool> {
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
            unsubscribe_url,
            warmup: None,
            html,
        },
    )?;
    smtp::send(&smtp::transport(state, &mb).await?, message).await?;
    Ok(true)
}

/// A calendar invite or cancellation from `mb`. See smtp::build_invite.
pub async fn send_invite(state: &AppState, mb: &Mailbox, to: (&str, &str), subject: &str, body: &str, ics: &str, method: &str) -> anyhow::Result<()> {
    let message = smtp::build_invite(mb, to.0, to.1, subject, body, ics, method)?;
    smtp::send(&smtp::transport(state, mb).await?, message).await?;
    Ok(())
}

/// An email as its lead would get it, sent to `to` from the org's first active mailbox. Returns false without one.
/// The footer's unsubscribe link goes nowhere, so clicking it in the test can't unsubscribe the real lead.
pub async fn send_test(state: &AppState, org_id: Uuid, to: &str, kind: &str, subject: &str, body: &str) -> anyhow::Result<bool> {
    let body = sender::with_footer(kind, body, &format!("{}/unsubscribe/test", state.cfg.public_url));
    send_system_email(state, org_id, to, subject, &body).await
}

/// Pauses a mailbox that bounced too much of its last 30 days, after a bounce came back to it. Resuming it is left to
/// someone who has cleaned the lead lists, and the next bounce pauses it again while the rate stays that high.
pub async fn pause_if_bouncing(db: &sqlx::PgPool, mailbox_id: Uuid) -> sqlx::Result<()> {
    let Some(stats) = crate::routes::mailboxes::mailbox_stats(db, mailbox_id).await? else { return Ok(()) };
    if !stats.too_many_bounces() {
        return Ok(());
    }
    let why = format!(
        "Paused after {}% of the {} leads it emailed in the last 30 days bounced. Check the addresses in your lead lists, then resume it.",
        stats.bounce_percent(),
        stats.emailed
    );
    let paused = sqlx::query("update mailboxes set status = 'paused', last_error = $2 where id = $1 and status = 'active'")
        .bind(mailbox_id)
        .bind(&why)
        .execute(db)
        .await?;
    if paused.rows_affected() == 1 {
        tracing::warn!(mailbox = %stats.email, bounced = stats.bounced, emailed = stats.emailed, "paused a mailbox for bounces");
    }
    Ok(())
}

/// Takes a lead's unsent LinkedIn steps off the to-do list, for a reply or a suppression. $1 is the lead.
pub const STOP_LINKEDIN: &str = "update linkedin_messages set status = 'skipped' where lead_id = $1 and status = 'todo'";

/// Stops all outreach to a lead: sets its status, suppresses the address for the org and cancels unsent emails, LinkedIn
/// steps and texts.
pub async fn suppress_lead(state: &AppState, lead: &Lead, org_id: Option<Uuid>, status: &str, reason: &str) -> sqlx::Result<()> {
    let mut tx = state.db.begin().await?;
    // Only a lead this changes gets a webhook event, so an unsubscribe link clicked twice sends one.
    let changed = sqlx::query("update leads set status = $2 where id = $1 and status <> $2").bind(lead.id).bind(status).execute(&mut *tx).await?;
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
    sqlx::query(STOP_LINKEDIN).bind(lead.id).execute(&mut *tx).await?;
    sqlx::query("update text_messages set status = 'cancelled', error = $2 where lead_id = $1 and status in ('draft', 'approved')")
        .bind(lead.id)
        .bind(format!("Lead {status}"))
        .execute(&mut *tx)
        .await?;
    sqlx::query(crate::calls::STOP_CALLS).bind(lead.id).bind(format!("Lead {status}")).execute(&mut *tx).await?;
    tx.commit().await?;
    let event = match status {
        "unsubscribed" => Some("lead_unsubscribed"),
        "bounced" => Some("lead_bounced"),
        _ => None,
    };
    if let Some(event) = event.filter(|_| changed.rows_affected() > 0) {
        crate::hooks::send_lead_event(state, lead.project_id, event, lead, None).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::pause_if_bouncing;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_mailbox_stops_once_over_a_tenth_of_its_leads_bounce(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        let mailbox: Uuid = sqlx::query_scalar(
            "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc)
             values ($1, 'a@example.test', 'smtp.example.test', 'a', 'x') returning id",
        )
        .bind(org)
        .fetch_one(&db)
        .await
        .unwrap();
        // 20 leads emailed, and a bounce back from each of the first `n`.
        let leads: Vec<Uuid> = sqlx::query_scalar("insert into leads (project_id) select $1 from generate_series(1, 20) returning id")
            .bind(project)
            .fetch_all(&db)
            .await
            .unwrap();
        for lead in &leads {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status, sent_at)
                 values ($1, $2, $3, 'outbound', 'initial', 'sent', now())",
            )
                .bind(project)
                .bind(lead)
                .bind(mailbox)
                .execute(&db)
                .await
                .unwrap();
        }
        let bounce = |lead: Uuid| {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status, classification)
                 values ($1, $2, $3, 'inbound', 'inbound', 'received', 'bounce')",
            )
                .bind(project)
                .bind(lead)
                .bind(mailbox)
                .execute(&db)
        };
        let state = || async {
            sqlx::query_as::<_, (String, String)>("select status, last_error from mailboxes where id = $1").bind(mailbox).fetch_one(&db).await.unwrap()
        };

        // 2 of 20 is 10%: past the digest's warning, not yet over the line.
        bounce(leads[0]).await.unwrap();
        bounce(leads[1]).await.unwrap();
        pause_if_bouncing(&db, mailbox).await.unwrap();
        assert_eq!(state().await.0, "active");

        bounce(leads[2]).await.unwrap();
        pause_if_bouncing(&db, mailbox).await.unwrap();
        let (status, why) = state().await;
        assert_eq!(status, "paused");
        assert!(why.starts_with("Paused after 15% of the 20 leads"), "{why}");
    }
}
