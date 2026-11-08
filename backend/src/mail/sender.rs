//! The sender loop, follow-up scheduling and the campaign optimizer.

use std::collections::HashMap;

use chrono::{Datelike, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::json;
use uuid::Uuid;

use super::smtp;
use crate::agent::{self, kind};
use crate::models::{EmailMessage, Lead, Mailbox, Ts};
use crate::state::AppState;

/// Replies people are waiting for. They skip the send window, the pause switch and the daily email limit.
const REPLY_KINDS: [&str; 2] = ["auto_reply", "manual_reply"];

pub fn new_message_id(sender_email: &str) -> String {
    let domain = sender_email.rsplit('@').next().filter(|d| !d.is_empty()).unwrap_or("splee.local");
    format!("{}@{domain}", Uuid::new_v4())
}

#[derive(sqlx::FromRow)]
struct SendingProject {
    id: Uuid,
    org_id: Uuid,
    daily_email_limit: i32,
    timezone: String,
    send_days: Vec<i32>,
    send_start_hour: i32,
    send_end_hour: i32,
    paused: bool,
}

impl SendingProject {
    fn tz(&self) -> Tz {
        self.timezone.parse().unwrap_or(Tz::UTC)
    }

    fn in_window(&self, now: Ts) -> bool {
        let local = now.with_timezone(&self.tz());
        let day = local.weekday().number_from_monday() as i32;
        let hour = local.hour() as i32;
        self.send_days.contains(&day) && hour >= self.send_start_hour && hour < self.send_end_hour
    }

    fn day_start(&self, now: Ts) -> Ts {
        let tz = self.tz();
        let midnight = now.with_timezone(&tz).date_naive().and_hms_opt(0, 0, 0).expect("midnight exists");
        tz.from_local_datetime(&midnight).earliest().map(|d| d.with_timezone(&Utc)).unwrap_or(now)
    }
}

pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    schedule_follow_ups(state).await?;
    let projects = sqlx::query_as::<_, SendingProject>(
        "select p.id, p.org_id, s.daily_email_limit, s.timezone, s.send_days, s.send_start_hour, s.send_end_hour, s.paused
         from projects p join project_settings s on s.project_id = p.id
         where p.org_id is not null and exists (
             select 1 from email_messages e where e.project_id = p.id and e.direction = 'outbound'
             and e.status in ('approved', 'scheduled') and (e.scheduled_for is null or e.scheduled_for <= now()))",
    )
    .fetch_all(&state.db)
    .await?;
    for project in projects {
        if let Err(e) = send_project(state, &project).await {
            tracing::error!(project_id = %project.id, error = ?e, "sending failed");
        }
    }
    Ok(())
}

async fn send_project(state: &AppState, p: &SendingProject) -> anyhow::Result<()> {
    let now = Utc::now();
    let cold_slots = if !p.paused && p.in_window(now) {
        // Replies count toward the limit too, so a busy inbox slows cold sending rather than adding to it.
        let sent_today: i64 = sqlx::query_scalar(
            "select count(*) from email_messages where project_id = $1 and direction = 'outbound' and status = 'sent' and sent_at >= $2",
        )
        .bind(p.id)
        .bind(p.day_start(now))
        .fetch_one(&state.db)
        .await?;
        (i64::from(p.daily_email_limit) - sent_today).max(0)
    } else {
        0
    };

    // Replies first, then campaigns the optimizer marked as scaling.
    let due = sqlx::query_as::<_, EmailMessage>(
        "(select e.* from email_messages e
          where e.project_id = $1 and e.direction = 'outbound' and e.status in ('approved', 'scheduled')
            and (e.scheduled_for is null or e.scheduled_for <= now()) and e.kind = any($2)
          order by e.created_at limit 20)
         union all
         (select e.* from email_messages e left join segments s on s.id = e.segment_id
          where e.project_id = $1 and e.direction = 'outbound' and e.status in ('approved', 'scheduled')
            and (e.scheduled_for is null or e.scheduled_for <= now()) and e.kind in ('initial', 'follow_up')
            and s.status is distinct from 'paused'
            -- Hold the lead while the agent rewrites its emails, so the old text can't go out mid-run.
            and not exists (select 1 from agent_runs r where r.project_id = e.project_id and r.kind = 'write_emails'
                            and r.status in ('queued', 'running') and r.input -> 'lead_ids' ? e.lead_id::text)
          order by (s.status = 'scaling') desc nulls last, e.kind = 'follow_up' desc, e.scheduled_for nulls first, e.created_at
          limit $3)",
    )
    .bind(p.id)
    .bind(&REPLY_KINDS[..])
    .bind(cold_slots)
    .fetch_all(&state.db)
    .await?;

    // Replies sent in this tick also use up the day's room, so cold emails stop at the limit rather than one past it.
    let mut room = cold_slots;
    for email in due {
        if room <= 0 && !REPLY_KINDS.contains(&email.kind.as_str()) {
            continue;
        }
        deliver(state, p.org_id, email).await?;
        room -= 1;
    }
    Ok(())
}

enum Skip {
    Cancel(&'static str),
    /// Try again on a later tick.
    Wait,
}

async fn check_recipient(state: &AppState, org_id: Uuid, email: &EmailMessage, lead: &Lead) -> anyhow::Result<Option<Skip>> {
    let Some(address) = lead.email.as_deref().filter(|e| e.contains('@')) else {
        return Ok(Some(Skip::Cancel("Lead has no email address")));
    };
    if matches!(lead.status.as_str(), "unsubscribed" | "bounced" | "do_not_contact") {
        return Ok(Some(Skip::Cancel("Lead opted out or bounced")));
    }
    if email.kind == "follow_up" && lead.status != "contacted" {
        return Ok(Some(Skip::Cancel("Lead already replied")));
    }
    let domain = address.rsplit('@').next().unwrap_or_default();
    let suppressed: bool = sqlx::query_scalar("select exists (select 1 from suppressions where org_id = $1 and value in ($2, $3))")
        .bind(org_id)
        .bind(address.to_lowercase())
        .bind(domain.to_lowercase())
        .fetch_one(&state.db)
        .await?;
    if suppressed {
        return Ok(Some(Skip::Cancel("Recipient is on the do-not-contact list")));
    }
    Ok(None)
}

/// Mailboxes of the org that may still send today, least used first.
async fn mailboxes_with_room(state: &AppState, org_id: Uuid) -> anyhow::Result<Vec<Mailbox>> {
    Ok(sqlx::query_as::<_, Mailbox>(
        "select m.* from mailboxes m
         left join lateral (select count(*) as sent, max(sent_at) as last_sent from email_messages e
                            where e.mailbox_id = m.id and e.status = 'sent' and e.sent_at >= date_trunc('day', now())) t on true
         where m.org_id = $1 and m.status = 'active' and coalesce(t.sent, 0) < m.daily_limit
         order by coalesce(t.sent, 0), t.last_sent nulls first",
    )
    .bind(org_id)
    .fetch_all(&state.db)
    .await?)
}

/// Follow-ups and replies go out from the mailbox the lead already talks to, so the thread stays together.
async fn pick_mailbox(state: &AppState, org_id: Uuid, email: &EmailMessage) -> anyhow::Result<Option<Mailbox>> {
    let available = mailboxes_with_room(state, org_id).await?;
    if email.kind == "initial" {
        return Ok(available.into_iter().next());
    }
    let thread_mailbox: Option<Uuid> = match email.mailbox_id {
        Some(id) => Some(id),
        None => sqlx::query_scalar(
            "select mailbox_id from email_messages where lead_id = $1 and mailbox_id is not null and (direction = 'inbound' or status = 'sent')
             order by created_at desc limit 1",
        )
        .bind(email.lead_id)
        .fetch_optional(&state.db)
        .await?
        .flatten(),
    };
    let Some(wanted) = thread_mailbox else { return Ok(available.into_iter().next()) };
    if let Some(mb) = available.iter().find(|m| m.id == wanted) {
        return Ok(Some(mb.clone()));
    }
    let still_usable: bool = sqlx::query_scalar("select exists (select 1 from mailboxes where id = $1 and status = 'active')")
        .bind(wanted)
        .fetch_one(&state.db)
        .await?;
    // The thread's mailbox is only out of quota for today: wait for it rather than switch senders mid-thread.
    if still_usable { Ok(None) } else { Ok(available.into_iter().next()) }
}

async fn deliver(state: &AppState, org_id: Uuid, email: EmailMessage) -> anyhow::Result<()> {
    let claimed = sqlx::query("update email_messages set status = 'sending' where id = $1 and status in ('approved', 'scheduled')")
        .bind(email.id)
        .execute(&state.db)
        .await?;
    if claimed.rows_affected() == 0 {
        return Ok(());
    }
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(email.lead_id).fetch_one(&state.db).await?;

    if let Some(skip) = check_recipient(state, org_id, &email, &lead).await? {
        return release(state, &email, skip).await;
    }
    let Some(mb) = pick_mailbox(state, org_id, &email).await? else {
        return release(state, &email, Skip::Wait).await;
    };
    let to = lead.email.clone().unwrap_or_default();

    let message_id = new_message_id(&mb.email);
    let references: Vec<String> = sqlx::query_scalar(
        "select message_id from email_messages where lead_id = $1 and message_id is not null and (direction = 'inbound' or status = 'sent')
         order by created_at",
    )
    .bind(lead.id)
    .fetch_all(&state.db)
    .await?;
    // Unthreaded follow-ups (written before the parent had an id) still attach to the latest message.
    let in_reply_to = email.in_reply_to.clone().or_else(|| if email.kind == "initial" { None } else { references.last().cloned() });
    let unsubscribe_page = format!("{}/unsubscribe/{}", state.cfg.public_url, lead.unsubscribe_token);
    let one_click = format!("{}/api/unsubscribe/{}", state.cfg.public_url, lead.unsubscribe_token);
    let body = if email.kind == "initial" || email.kind == "follow_up" {
        format!("{}\n\n--\nNot relevant? Unsubscribe: {unsubscribe_page}", email.body.trim_end())
    } else {
        email.body.trim_end().to_string()
    };

    let built = smtp::build(
        &mb,
        &smtp::Outgoing {
            to_name: &lead.full_name,
            to_email: &to,
            subject: &email.subject,
            body: &body,
            message_id: &message_id,
            in_reply_to: in_reply_to.as_deref(),
            references: if in_reply_to.is_some() { &references } else { &[] },
            unsubscribe_url: Some(&one_click),
        },
    );
    let message = match built {
        Ok(m) => m,
        Err(e) => {
            sqlx::query("update email_messages set status = 'failed', error = $2 where id = $1").bind(email.id).bind(e.to_string()).execute(&state.db).await?;
            return Ok(());
        }
    };
    let transport = match smtp::transport(state, &mb) {
        Ok(t) => t,
        Err(e) => return mailbox_failed(state, &mb, &email, &e.to_string()).await,
    };

    match smtp::send(&transport, message).await {
        Ok(()) => record_sent(state, &email, &lead, &mb, &message_id, in_reply_to.as_deref(), &to).await,
        Err(e) => {
            let code = e.status().map(|c| c.to_string()).unwrap_or_default();
            // 550/551/553 reject this recipient. Anything else is a problem with the mailbox or the connection.
            if matches!(code.as_str(), "550" | "551" | "553") {
                sqlx::query("update email_messages set status = 'failed', error = $2 where id = $1")
                    .bind(email.id)
                    .bind(format!("Rejected by the recipient's server: {e}"))
                    .execute(&state.db)
                    .await?;
                super::suppress_lead(&state.db, &lead, Some(org_id), "bounced", "bounce").await?;
                Ok(())
            } else {
                mailbox_failed(state, &mb, &email, &format!("SMTP: {e}")).await
            }
        }
    }
}

async fn release(state: &AppState, email: &EmailMessage, skip: Skip) -> anyhow::Result<()> {
    match skip {
        Skip::Cancel(reason) => {
            sqlx::query("update email_messages set status = 'cancelled', error = $2 where id = $1").bind(email.id).bind(reason).execute(&state.db).await?;
        }
        Skip::Wait => {
            sqlx::query("update email_messages set status = 'approved' where id = $1").bind(email.id).execute(&state.db).await?;
        }
    }
    Ok(())
}

async fn mailbox_failed(state: &AppState, mb: &Mailbox, email: &EmailMessage, error: &str) -> anyhow::Result<()> {
    tracing::warn!(mailbox = %mb.email, %error, "mailbox failed, pausing it");
    sqlx::query("update mailboxes set status = 'error', last_error = $2 where id = $1").bind(mb.id).bind(error).execute(&state.db).await?;
    sqlx::query("update email_messages set status = 'approved', error = $2 where id = $1").bind(email.id).bind(error).execute(&state.db).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn record_sent(
    state: &AppState,
    email: &EmailMessage,
    lead: &Lead,
    mb: &Mailbox,
    message_id: &str,
    in_reply_to: Option<&str>,
    to: &str,
) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "update email_messages set status = 'sent', sent_at = now(), message_id = $2, in_reply_to = $3, mailbox_id = $4, from_email = $5,
                                   to_email = $6, cost_micros = $7, error = ''
         where id = $1",
    )
    .bind(email.id)
    .bind(message_id)
    .bind(in_reply_to)
    .bind(mb.id)
    .bind(&mb.email)
    .bind(to)
    .bind(mb.cost_per_email_micros)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "update leads set last_contacted_at = now(), status = case when status in ('new', 'queued') then 'contacted' else status end where id = $1",
    )
    .bind(lead.id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Leads whose next follow-up is due within 12 hours get one written by the agent.
async fn schedule_follow_ups(state: &AppState) -> anyhow::Result<()> {
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "select l.project_id, p.org_id, l.id
         from leads l
         join projects p on p.id = l.project_id
         join project_settings ps on ps.project_id = l.project_id
         left join segments s on s.id = l.segment_id
         join lateral (select e.step, e.sent_at from email_messages e
                       where e.lead_id = l.id and e.direction = 'outbound' and e.kind in ('initial', 'follow_up') and e.status = 'sent'
                       order by e.step desc limit 1) last on true
         where p.org_id is not null and not ps.paused and l.status = 'contacted'
           and coalesce(s.status, 'working') <> 'paused'
           and jsonb_typeof(ps.follow_ups) = 'array' and jsonb_array_length(ps.follow_ups) > last.step
           and coalesce((ps.follow_ups -> last.step ->> 'enabled')::boolean, false)
           and last.sent_at + make_interval(days => greatest(1, coalesce((ps.follow_ups -> last.step ->> 'delay_days')::int, 3)))
               <= now() + interval '12 hours'
           and not exists (select 1 from email_messages f where f.lead_id = l.id and f.kind = 'follow_up' and f.step = last.step + 1)
           and not exists (select 1 from email_messages i where i.lead_id = l.id and i.direction = 'inbound')
           and not exists (select 1 from agent_runs r where r.kind = 'write_emails' and r.project_id = l.project_id
                           and (r.status in ('queued', 'running') or r.created_at > now() - interval '6 hours')
                           and r.input -> 'lead_ids' ? l.id::text)
         limit 200",
    )
    .fetch_all(&state.db)
    .await?;

    let mut by_project: HashMap<(Uuid, Uuid), Vec<Uuid>> = HashMap::new();
    for (project_id, org_id, lead_id) in due {
        by_project.entry((project_id, org_id)).or_default().push(lead_id);
    }
    for ((project_id, org_id), leads) in by_project {
        for batch in leads.chunks(20) {
            agent::enqueue(&state.db, kind::WRITE_EMAILS, Some(project_id), Some(org_id), json!({"lead_ids": batch, "follow_up": true})).await?;
        }
    }
    Ok(())
}

/// Hourly: campaigns with enough data move between scaling, working and paused by emails sent per hot lead.
pub async fn optimize(state: &AppState) -> anyhow::Result<()> {
    let rows: Vec<(Uuid, String, i64, i64)> = sqlx::query_as(
        "select s.id, s.status,
                (select count(*) from email_messages e where e.segment_id = s.id and e.status = 'sent'),
                (select count(*) from leads l where l.segment_id = s.id and l.status in ('hot', 'meeting_booked'))
         from segments s join project_settings ps on ps.project_id = s.project_id
         where s.status in ('working', 'scaling') and not ps.paused",
    )
    .fetch_all(&state.db)
    .await?;
    for (id, status, sent, hot) in rows {
        let Some(next) = campaign_status(sent, hot) else { continue };
        if next != status {
            tracing::info!(segment_id = %id, from = %status, to = next, "optimizer moved campaign");
            sqlx::query("update segments set status = $2 where id = $1").bind(id).bind(next).execute(&state.db).await?;
        }
    }
    Ok(())
}

/// Where the optimizer puts a campaign, or None while it has too little data to judge.
fn campaign_status(sent: i64, hot: i64) -> Option<&'static str> {
    if sent < 40 {
        return None;
    }
    // These were $10 and $25 per hot lead when every email cost $0.04.
    // No hot lead yet counts as all the emails sent so far for one, the most optimistic reading.
    Some(if hot > 0 && sent < 250 * hot {
        "scaling"
    } else if sent > 625 * hot.max(1) {
        "paused"
    } else {
        "working"
    })
}

#[cfg(test)]
mod tests {
    use super::campaign_status;

    #[test]
    fn waits_for_enough_sends() {
        assert_eq!(campaign_status(39, 5), None);
    }

    #[test]
    fn scales_cheap_campaigns_and_pauses_expensive_ones() {
        assert_eq!(campaign_status(400, 2), Some("scaling"));
        assert_eq!(campaign_status(800, 2), Some("working"));
        // 625.5 emails per hot lead is over the line, which integer division used to miss.
        assert_eq!(campaign_status(1251, 2), Some("paused"));
    }

    #[test]
    fn no_hot_lead_pauses_only_past_the_threshold() {
        assert_eq!(campaign_status(625, 0), Some("working"));
        assert_eq!(campaign_status(626, 0), Some("paused"));
    }
}
