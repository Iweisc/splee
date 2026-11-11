//! Emails to the team itself (not to leads), driven by notification_prefs, the weekly report in each project's chat and the
//! Slack app's list of emails waiting for approval.
//! Hot-lead emails live in agent::replies next to the webhook and Slack alerts.

use chrono::{Datelike, Timelike, Utc, Weekday};
use uuid::Uuid;

use crate::hooks::{self, ChatAlert};
use crate::models::MailboxStats;
use crate::state::AppState;

const DIGEST_HOUR_UTC: u32 = 8;

/// Hourly. Daily digests go out at 08:00 UTC, weekly reports on Monday at the same hour.
pub async fn digests(state: &AppState) -> anyhow::Result<()> {
    let now = Utc::now();
    if now.hour() != DIGEST_HOUR_UTC {
        return Ok(());
    }
    // Each step runs whatever the one before did, so one organization's broken mailbox doesn't stop the chat reports.
    let mut results = vec![
        ("daily digests", send_digests(state, "daily_digest", 1).await),
        ("Slack approval digests", crate::slack::post_digests(state).await),
    ];
    if now.weekday() == Weekday::Mon {
        results.push(("weekly reports", send_digests(state, "weekly_report", 7).await));
        results.push(("weekly chat reports", post_weekly_reports(state).await));
    }
    for (what, result) in results {
        if let Err(e) = result {
            tracing::error!(error = ?e, "{what} failed");
        }
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct ProjectActivity {
    name: String,
    sent: i64,
    replies: i64,
    hot: i64,
    spend_micros: i64,
}

impl ProjectActivity {
    fn summary(&self) -> String {
        format!("{}: {} emails sent, {} replies, {} hot, {} spent", self.name, self.sent, self.replies, self.hot, usd(self.spend_micros))
    }
}

/// Each project's numbers for the last `days` days. `filter` picks the projects by $1 and is one of our own strings.
async fn activity(db: &sqlx::PgPool, filter: &str, id: Uuid, days: i32) -> sqlx::Result<Vec<ProjectActivity>> {
    sqlx::query_as::<_, ProjectActivity>(&format!(
        "select p.name,
                (select count(*) from email_messages e where e.project_id = p.id and e.direction = 'outbound'
                   and e.status = 'sent' and e.sent_at > now() - make_interval(days => $2)) as sent,
                (select count(*) from email_messages e where e.project_id = p.id and e.direction = 'inbound'
                   and e.classification is distinct from 'bounce' and e.created_at > now() - make_interval(days => $2)) as replies,
                (select count(*) from email_messages e where e.project_id = p.id and e.direction = 'inbound'
                   and e.classification = 'hot' and e.created_at > now() - make_interval(days => $2)) as hot,
                (select coalesce(sum(e.cost_micros), 0)::bigint from email_messages e where e.project_id = p.id
                   and e.status = 'sent' and e.sent_at > now() - make_interval(days => $2))
                + (select (coalesce(sum(r.cost_usd), 0) * 1000000)::bigint from agent_runs r where r.project_id = p.id
                   and r.created_at > now() - make_interval(days => $2)) as spend_micros
         from projects p where {filter} order by p.created_at"
    ))
    .bind(id)
    .bind(days)
    .fetch_all(db)
    .await
}

async fn send_digests(state: &AppState, kind: &str, days: i32) -> anyhow::Result<()> {
    let orgs: Vec<Uuid> = sqlx::query_scalar("select id from organizations").fetch_all(&state.db).await?;
    for org_id in orgs {
        let activity = activity(&state.db, "p.org_id = $1", org_id, days).await?;
        let stopped: Vec<(String, String, bool)> = sqlx::query_as(
            "select email::text, last_error, recheck_at is not null from mailboxes where org_id = $1 and status = 'error' order by email",
        )
        .bind(org_id)
        .fetch_all(&state.db)
        .await?;
        let bouncing: Vec<_> = crate::routes::mailboxes::org_stats(&state.db, org_id).await?.into_iter().filter(MailboxStats::bouncing).collect();
        // A stopped or bouncing mailbox gets a digest even on a quiet day: it may be why the day was quiet.
        let active: Vec<_> = activity.into_iter().filter(|a| a.sent > 0 || a.replies > 0).collect();
        if active.is_empty() && stopped.is_empty() && bouncing.is_empty() {
            continue;
        }
        let waiting = crate::routes::inbox::needs_reply_count(&state.db, org_id, None).await?;
        let body = digest_body(days, &active, waiting, &stopped, &bouncing, &state.cfg.public_url);
        let subject = if days == 1 { "Your daily Splee digest" } else { "Your weekly Splee report" };

        for (user_id, to) in recipients(state, org_id, kind).await? {
            if claim(&state.db, user_id, org_id, kind).await? && !super::send_system_email(state, org_id, &to, subject, &body).await? {
                // No active mailbox to send from. Nothing else in the org will send either.
                break;
            }
        }
    }
    Ok(())
}

/// Marks today's weekly chat report as sent for every project that wants one and has a chat channel, and returns them.
/// Claiming first keeps a second check in the digest hour from posting it twice.
async fn claim_weekly_reports(db: &sqlx::PgPool) -> sqlx::Result<Vec<(Uuid, Uuid)>> {
    sqlx::query_as(
        "update project_settings s set chat_report_on = current_date from projects p
         where p.id = s.project_id and p.org_id is not null and s.chat_weekly_report
           and exists (select 1 from chat_channels c where c.project_id = s.project_id)
           and s.chat_report_on is distinct from current_date
         returning s.project_id, p.org_id",
    )
    .fetch_all(db)
    .await
}

/// Posts the last 7 days' numbers to each project's chat that asked for them, quiet weeks too: a week of zeros is news.
async fn post_weekly_reports(state: &AppState) -> anyhow::Result<()> {
    for (project_id, org_id) in claim_weekly_reports(&state.db).await? {
        let Some(week) = activity(&state.db, "p.id = $1", project_id, 7).await?.pop() else { continue };
        let waiting = crate::routes::inbox::needs_reply_count(&state.db, org_id, Some(project_id)).await?;
        let summary = week.summary();
        let quote = match waiting {
            0 => None,
            1 => Some("1 reply is waiting for an answer.".to_string()),
            n => Some(format!("{n} replies are waiting for an answer.")),
        };
        let link = format!("{}/app/p/{project_id}", state.cfg.public_url);
        let alert = ChatAlert { label: "Last 7 days", summary: &summary, quote: quote.as_deref(), link: &link, link_text: "Open the project" };
        hooks::post_chat(&state.db, project_id, &alert).await;
    }
    Ok(())
}

/// News that can't wait for the digest: posts the alert to the project's chats and emails it to the org members who get the
/// daily digest.
pub async fn notify_team(state: &AppState, project_id: Uuid, org_id: Uuid, alert: &ChatAlert<'_>) {
    hooks::post_chat(&state.db, project_id, alert).await;
    let recipients = match recipients(state, org_id, "daily_digest").await {
        Ok(r) => r,
        Err(e) => return tracing::warn!(error = %e, "couldn't load who to email"),
    };
    let body = format!("{}\n\n{}: {}", alert.summary, alert.link_text, alert.link);
    for (_, to) in recipients {
        match super::send_system_email(state, org_id, &to, alert.label, &body).await {
            Ok(true) => {}
            // No active mailbox to send from.
            Ok(false) => break,
            Err(e) => tracing::warn!(error = ?e, "team email failed"),
        }
    }
}

/// `stopped` is each stopped mailbox's address, error, and whether the sender tests it again by itself.
/// `bouncing` is the mailboxes whose last 30 days bounced too often.
fn digest_body(
    days: i32,
    active: &[ProjectActivity],
    waiting: i64,
    stopped: &[(String, String, bool)],
    bouncing: &[MailboxStats],
    public_url: &str,
) -> String {
    let period = if days == 1 { "the last 24 hours" } else { "the last 7 days" };
    let mut body = String::new();
    if !active.is_empty() {
        body += &format!("Here's what your Splee agent did in {period}:\n\n");
        for a in active {
            body += &format!("{}\n", a.summary());
        }
    }
    if !stopped.is_empty() {
        body += if body.is_empty() { "" } else { "\n" };
        for (email, error, retrying) in stopped {
            let next = if *retrying { ", and Splee tests it again on its own" } else { "" };
            body += &format!("{email} has stopped sending{next}: {error}\n");
        }
        if stopped.iter().any(|(_, _, retrying)| !retrying) {
            body += &format!("Fix it and press Test on {public_url}/app/domains\n");
        }
    }
    if !bouncing.is_empty() {
        body += if body.is_empty() { "" } else { "\n" };
        for s in bouncing {
            body += &format!("{} bounced {}% of the {} leads it emailed in the last 30 days.\n", s.email, s.bounce_percent(), s.emailed);
        }
        body += "Past 5%, providers start sending a mailbox's mail to spam, and past 10% Splee pauses it. ";
        body += "Check the addresses in your lead lists before you send more.\n";
    }
    if waiting > 0 {
        let replies = if waiting == 1 { "1 reply is" } else { &format!("{waiting} replies are") };
        body += &format!("\n{replies} waiting for an answer.");
    }
    body += &format!("\nOpen your inbox: {public_url}/app/inbox");
    body
}

/// Records today's digest of this kind for this user and organization. False when it already went out, which keeps the
/// checks every 15 minutes in the digest hour from sending it twice.
async fn claim(db: &sqlx::PgPool, user_id: Uuid, org_id: Uuid, kind: &str) -> sqlx::Result<bool> {
    let inserted = sqlx::query("insert into notification_log (user_id, org_id, kind, sent_on) values ($1, $2, $3, current_date) on conflict do nothing")
        .bind(user_id)
        .bind(org_id)
        .bind(kind)
        .execute(db)
        .await?;
    Ok(inserted.rows_affected() == 1)
}

fn usd(micros: i64) -> String {
    format!("${:.2}", micros as f64 / 1_000_000.0)
}

/// Org members who turned the given notification_prefs column on. `pref` is one of our own column names, never user input.
async fn recipients(state: &AppState, org_id: Uuid, pref: &str) -> sqlx::Result<Vec<(Uuid, String)>> {
    sqlx::query_as(&format!(
        "select u.id, u.email::text from memberships m join users u on u.id = m.user_id
         join notification_prefs n on n.user_id = u.id
         where m.org_id = $1 and n.{pref}"
    ))
    .bind(org_id)
    .fetch_all(&state.db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[test]
    fn a_stopped_mailbox_is_in_the_digest_even_on_a_quiet_day() {
        let stopped = [("lewis@acme.test".to_string(), "SMTP: authentication failed".to_string(), false)];
        let quiet = digest_body(1, &[], 0, &stopped, &[], "https://app.test");
        assert!(quiet.starts_with("lewis@acme.test has stopped sending: SMTP: authentication failed\n"), "{quiet}");
        assert!(quiet.contains("press Test on https://app.test/app/domains"), "{quiet}");

        let retrying = [("maya@acme.test".to_string(), "SMTP: 5.4.5 Daily user sending limit exceeded".to_string(), true)];
        let waits = digest_body(1, &[], 0, &retrying, &[], "https://app.test");
        assert!(waits.starts_with("maya@acme.test has stopped sending, and Splee tests it again on its own: SMTP: 5.4.5"), "{waits}");
        assert!(!waits.contains("press Test"), "{waits}");

        let busy = [ProjectActivity { name: "Northwind".into(), sent: 12, replies: 2, hot: 1, spend_micros: 1_500_000 }];
        let normal = digest_body(1, &busy, 2, &[], &[], "https://app.test");
        assert!(normal.contains("Northwind: 12 emails sent, 2 replies, 1 hot, $1.50 spent\n"), "{normal}");
        assert!(!normal.contains("stopped sending"), "{normal}");
        assert!(normal.contains("2 replies are waiting for an answer."), "{normal}");
    }

    #[test]
    fn a_bouncing_mailbox_is_in_the_digest() {
        let stats = |emailed, bounced| MailboxStats { mailbox_id: Uuid::nil(), email: "lewis@acme.test".into(), emailed, bounced, replied: 0 };
        assert!(!stats(19, 5).bouncing(), "too few leads to tell");
        assert!(!stats(40, 2).bouncing(), "exactly 5% is fine");
        assert!(stats(40, 3).bouncing());

        let body = digest_body(1, &[], 0, &[], &[stats(48, 7)], "https://app.test");
        assert!(body.starts_with("lewis@acme.test bounced 15% of the 48 leads it emailed in the last 30 days.\n"), "{body}");
        assert!(body.contains("Check the addresses in your lead lists"), "{body}");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_weekly_chat_report_goes_out_once_to_projects_that_want_it(db: sqlx::PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project = |chat: &'static str, weekly: bool| {
            let db = db.clone();
            async move {
                let id: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'P') returning id").bind(org).fetch_one(&db).await.unwrap();
                sqlx::query("insert into project_settings (project_id, chat_weekly_report) values ($1, $2)").bind(id).bind(weekly).execute(&db).await.unwrap();
                if !chat.is_empty() {
                    sqlx::query("insert into chat_channels (project_id, url) values ($1, $2)").bind(id).bind(chat).execute(&db).await.unwrap();
                }
                id
            }
        };
        let wants = project("https://ntfy.sh/acme", true).await;
        project("", true).await;
        project("https://ntfy.sh/acme", false).await;

        assert_eq!(claim_weekly_reports(&db).await.unwrap(), [(wants, org)]);
        assert!(claim_weekly_reports(&db).await.unwrap().is_empty());
        let week = activity(&db, "p.id = $1", wants, 7).await.unwrap();
        assert_eq!(week[0].summary(), "P: 0 emails sent, 0 replies, 0 hot, $0.00 spent");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_user_in_two_orgs_gets_each_orgs_digest_once(db: sqlx::PgPool) {
        let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('ana@example.com', 'x') returning id")
            .fetch_one(&db)
            .await
            .unwrap();
        let org = || sqlx::query_scalar::<_, Uuid>("insert into organizations (name) values ('Test') returning id").fetch_one(&db);
        let (first, second) = (org().await.unwrap(), org().await.unwrap());

        assert!(claim(&db, user, first, "daily_digest").await.unwrap());
        assert!(claim(&db, user, second, "daily_digest").await.unwrap());
        assert!(!claim(&db, user, first, "daily_digest").await.unwrap());
        assert!(claim(&db, user, first, "weekly_report").await.unwrap());
    }
}
