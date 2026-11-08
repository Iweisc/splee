//! Emails to the team itself (not to leads), driven by notification_prefs.
//! Hot-lead emails live in agent::replies next to the webhook and Slack alerts.

use chrono::{Datelike, Timelike, Utc, Weekday};
use uuid::Uuid;

use crate::billing::{self, EMAIL_COST_MICROS};
use crate::state::AppState;

pub const LOW_BALANCE_MICROS: i64 = 2_000_000;
const DIGEST_HOUR_UTC: u32 = 8;

/// Called after every sent email. Fires only on the email that takes the balance below the threshold.
pub async fn low_balance(state: &AppState, org_id: Uuid) -> anyhow::Result<()> {
    if !state.cfg.payments_enabled {
        return Ok(());
    }
    let balance = billing::balance(&state.db, org_id).await?;
    if !(balance < LOW_BALANCE_MICROS && balance + EMAIL_COST_MICROS >= LOW_BALANCE_MICROS) {
        return Ok(());
    }
    let link = format!("{}/app/billing", state.cfg.public_url);
    let body = format!(
        "Your Splee balance is down to {}. Sending stops when it reaches $0.00.\n\nAdd funds: {link}",
        usd(balance)
    );
    for (_, to) in recipients(state, org_id, "low_balance").await? {
        super::send_system_email(state, org_id, &to, "Your Splee balance is running low", &body).await?;
    }
    Ok(())
}

/// Hourly. Daily digests go out at 08:00 UTC, weekly reports on Monday at the same hour.
pub async fn digests(state: &AppState) -> anyhow::Result<()> {
    let now = Utc::now();
    if now.hour() != DIGEST_HOUR_UTC {
        return Ok(());
    }
    send_digests(state, "daily_digest", 1).await?;
    if now.weekday() == Weekday::Mon {
        send_digests(state, "weekly_report", 7).await?;
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

async fn send_digests(state: &AppState, kind: &str, days: i32) -> anyhow::Result<()> {
    let orgs: Vec<Uuid> = sqlx::query_scalar("select id from organizations").fetch_all(&state.db).await?;
    for org_id in orgs {
        let activity = sqlx::query_as::<_, ProjectActivity>(
            "select p.name,
                    (select count(*) from email_messages e where e.project_id = p.id and e.direction = 'outbound'
                       and e.status = 'sent' and e.sent_at > now() - make_interval(days => $2)) as sent,
                    (select count(*) from email_messages e where e.project_id = p.id and e.direction = 'inbound'
                       and e.created_at > now() - make_interval(days => $2)) as replies,
                    (select count(*) from email_messages e where e.project_id = p.id and e.direction = 'inbound'
                       and e.classification = 'hot' and e.created_at > now() - make_interval(days => $2)) as hot,
                    (select coalesce(sum(e.cost_micros), 0)::bigint from email_messages e where e.project_id = p.id
                       and e.status = 'sent' and e.sent_at > now() - make_interval(days => $2))
                    + (select (coalesce(sum(r.cost_usd), 0) * 1000000)::bigint from agent_runs r where r.project_id = p.id
                       and r.created_at > now() - make_interval(days => $2)) as spend_micros
             from projects p where p.org_id = $1 order by p.created_at",
        )
        .bind(org_id)
        .bind(days)
        .fetch_all(&state.db)
        .await?;
        let active: Vec<_> = activity.iter().filter(|a| a.sent > 0 || a.replies > 0).collect();
        if active.is_empty() {
            continue;
        }

        let period = if days == 1 { "the last 24 hours" } else { "the last 7 days" };
        let mut body = format!("Here's what your Splee agent did in {period}:\n\n");
        for a in &active {
            body += &format!(
                "{}: {} emails sent, {} replies, {} hot, {} spent\n",
                a.name, a.sent, a.replies, a.hot, usd(a.spend_micros)
            );
        }
        body += &format!("\nOpen your inbox: {}/app/inbox", state.cfg.public_url);
        let subject = if days == 1 { "Your daily Splee digest" } else { "Your weekly Splee report" };

        for (user_id, to) in recipients(state, org_id, kind).await? {
            let claimed = sqlx::query("insert into notification_log (user_id, kind, sent_on) values ($1, $2, current_date) on conflict do nothing")
                .bind(user_id)
                .bind(kind)
                .execute(&state.db)
                .await?
                .rows_affected()
                == 1;
            if claimed && !super::send_system_email(state, org_id, &to, subject, &body).await? {
                // No active mailbox to send from. Nothing else in the org will send either.
                break;
            }
        }
    }
    Ok(())
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
