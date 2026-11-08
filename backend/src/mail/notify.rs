//! Emails to the team itself (not to leads), driven by notification_prefs.
//! Hot-lead emails live in agent::replies next to the webhook and Slack alerts.

use chrono::{Datelike, Timelike, Utc, Weekday};
use uuid::Uuid;

use crate::state::AppState;

const DIGEST_HOUR_UTC: u32 = 8;

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
        let waiting = crate::routes::inbox::needs_reply_count(&state.db, org_id).await?;
        if waiting > 0 {
            let replies = if waiting == 1 { "1 reply is" } else { &format!("{waiting} replies are") };
            body += &format!("\n{replies} waiting for an answer.");
        }
        body += &format!("\nOpen your inbox: {}/app/inbox", state.cfg.public_url);
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
