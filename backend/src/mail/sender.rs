//! The sender loop, follow-up scheduling and the campaign optimizer.

use std::collections::HashMap;

use chrono::{Datelike, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::json;
use uuid::Uuid;

use super::warmup::{HOLD_DAYS, LANDING_IN_SPAM, WARMUP_SENT_TODAY};
use super::{dns, smtp};
use crate::agent::{self, kind};
use crate::hooks::{self, Alert};
use crate::models::{EmailMessage, Lead, Mailbox, Ts};
use crate::state::AppState;
use crate::verify::{self, Lookup, Verdict};

/// Replies people are waiting for. They skip the send window, the pause switch and the daily email limit.
const REPLY_KINDS: [&str; 2] = ["auto_reply", "manual_reply"];
/// The kinds Splee writes, ending with the signature WriterSettings::sign added. A reply someone typed is sent as they
/// wrote it.
const WRITTEN_KINDS: [&str; 3] = ["initial", "follow_up", "auto_reply"];

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
    autopilot: bool,
    autopilot_lead_cap: i32,
}

impl SendingProject {
    fn tz(&self) -> Tz {
        self.timezone.parse().unwrap_or(Tz::UTC)
    }

    fn in_window(&self, now: Ts) -> bool {
        in_window(self.tz(), &self.send_days, self.send_start_hour, self.send_end_hour, now)
    }

    fn day_start(&self, now: Ts) -> Ts {
        let tz = self.tz();
        let midnight = now.with_timezone(&tz).date_naive().and_hms_opt(0, 0, 0).expect("midnight exists");
        tz.from_local_datetime(&midnight).earliest().map(|d| d.with_timezone(&Utc)).unwrap_or(now)
    }
}

/// Whether a project's send days and hours, in its timezone, include now. LinkedIn sends keep to the same window.
pub fn in_window(tz: Tz, days: &[i32], start_hour: i32, end_hour: i32, now: Ts) -> bool {
    let local = now.with_timezone(&tz);
    let day = local.weekday().number_from_monday() as i32;
    let hour = local.hour() as i32;
    days.contains(&day) && hour >= start_hour && hour < end_hour
}

pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    schedule_follow_ups(state).await?;
    let projects = sqlx::query_as::<_, SendingProject>(
        "select p.id, p.org_id, s.daily_email_limit, s.timezone, s.send_days, s.send_start_hour, s.send_end_hour, s.paused,
                s.autopilot, s.autopilot_lead_cap
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

    // On autopilot, first emails also stop at the month's cap on new leads. Follow-ups go to leads already contacted.
    let mut new_leads = if p.autopilot && due.iter().any(|e| e.kind == "initial") {
        i64::from(p.autopilot_lead_cap) - crate::autopilot::contacted_since(&state.db, p.id, crate::autopilot::month_start(now)).await?
    } else {
        i64::MAX
    };
    // Replies sent in this tick also use up the day's room, so cold emails stop at the limit rather than one past it.
    let mut room = cold_slots;
    for email in due {
        if room <= 0 && !REPLY_KINDS.contains(&email.kind.as_str()) {
            continue;
        }
        if email.kind == "initial" {
            if new_leads <= 0 {
                continue;
            }
            new_leads -= 1;
        }
        deliver(state, p.org_id, email).await?;
        room -= 1;
    }
    Ok(())
}

enum Skip {
    /// Not sent on purpose: the lead opted out, replied or is on the do-not-contact list.
    Cancel(&'static str),
    /// Can never be delivered as it stands. Failed emails show in the Failed tab with the reason.
    Fail(&'static str),
    /// Try again on a later tick.
    Wait,
}

async fn check_recipient(state: &AppState, org_id: Uuid, email: &EmailMessage, lead: &Lead) -> anyhow::Result<Option<Skip>> {
    let Some(address) = lead.email.as_deref().filter(|e| e.contains('@')) else {
        return Ok(Some(Skip::Fail("Lead has no email address")));
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
    // A sure bounce hurts the mailbox's reputation, so don't send what DNS says can't arrive.
    if dns::undeliverable(&dns::resolver()?, domain).await {
        return Ok(Some(Skip::Fail("The recipient's domain can't receive email")));
    }
    Ok(None)
}

/// Checks a first email's address with the project's verifier. A follow-up's address was already sent to, and a reply's
/// came from the lead.
async fn verify_first(state: &AppState, email: &EmailMessage, lead: &Lead, to: &str) -> anyhow::Result<Option<Skip>> {
    const INVALID: Skip = Skip::Fail("The email verifier says this address doesn't exist");
    if email.kind != "initial" || lead.email_status == "verified" {
        return Ok(None);
    }
    if lead.email_status == "invalid" {
        return Ok(Some(INVALID));
    }
    // A found address beats a guessed one. The email waits a tick so the new address gets the do-not-contact and domain
    // checks too. Skipped when another lead in the project already has it, so nobody gets two first emails.
    let lookup = if lead.email_status == "guessed" { verify::find(state, email.project_id, lead, to).await } else { Lookup::Nothing };
    let finder_failed = matches!(lookup, Lookup::Failed);
    if let Lookup::Found(found) = lookup {
        if found.email != to.to_lowercase() {
            if replace_guessed(&state.db, lead, &found).await? {
                return Ok(Some(Skip::Wait));
            }
        } else if found.verified {
            sqlx::query("update leads set email_status = 'verified' where id = $1").bind(lead.id).execute(&state.db).await?;
            return Ok(None);
        }
    }
    let status = match verify::verify(state, email.project_id, to, finder_failed).await {
        Some(Verdict::Valid) => "verified",
        Some(Verdict::Invalid) => "invalid",
        Some(Verdict::Unsure) | None => return Ok(None),
    };
    sqlx::query("update leads set email_status = $2 where id = $1").bind(lead.id).bind(status).execute(&state.db).await?;
    Ok((status == "invalid").then_some(INVALID))
}

/// Points the lead at the found address, unless another lead in the project already has it. True when it moved.
async fn replace_guessed(db: &sqlx::PgPool, lead: &Lead, found: &verify::Found) -> sqlx::Result<bool> {
    let moved = sqlx::query(
        "update leads set email = $2, email_status = $3 where id = $1
           and not exists (select 1 from leads o where o.project_id = $4 and o.id <> $1 and lower(o.email::text) = $2)",
    )
    .bind(lead.id)
    .bind(&found.email)
    .bind(if found.verified { "verified" } else { "found" })
    .bind(lead.project_id)
    .execute(db)
    .await?;
    Ok(moved.rows_affected() == 1)
}

/// How many emails mailbox `m` may send today. A ramping mailbox starts at 10 on its first day and adds 4 a day until it
/// reaches its daily limit. Keep in sync with todayLimit in frontend/src/lib/mailbox.ts.
const TODAY_LIMIT: &str = "case when m.ramp_up then least(m.daily_limit, 10 + 4 * (current_date - coalesce(m.first_sent_on, current_date)))
                           else m.daily_limit end";

/// Mailboxes of the org that may still send campaigns today, least used first. A warming mailbox also stays under its
/// total with today's warmup emails. One held until warm sends none in its first 14 days of warmup, or while its warmup
/// emails land in spam.
async fn mailboxes_with_room(db: &sqlx::PgPool, org_id: Uuid) -> anyhow::Result<Vec<Mailbox>> {
    Ok(sqlx::query_as::<_, Mailbox>(&format!(
        "select m.* from mailboxes m
         left join lateral (select count(*) as sent, max(sent_at) as last_sent from email_messages e
                            where e.mailbox_id = m.id and e.status = 'sent' and e.sent_at >= date_trunc('day', now())) t on true
         where m.org_id = $1 and m.status = 'active' and coalesce(t.sent, 0) < {TODAY_LIMIT}
           and (not m.warmup or coalesce(t.sent, 0) + ({WARMUP_SENT_TODAY}) < m.total_daily_limit)
           and not (m.warmup and m.hold_until_warm and (coalesce(current_date - m.warmup_started_on, 0) < {HOLD_DAYS} or {LANDING_IN_SPAM}))
         order by coalesce(t.sent, 0), t.last_sent nulls first",
    ))
    .bind(org_id)
    .fetch_all(db)
    .await?)
}

/// Follow-ups and replies go out from the mailbox the lead already talks to, so the thread stays together.
async fn pick_mailbox(db: &sqlx::PgPool, org_id: Uuid, email: &EmailMessage) -> anyhow::Result<Option<Mailbox>> {
    let available = mailboxes_with_room(db, org_id).await?;
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
        .fetch_optional(db)
        .await?
        .flatten(),
    };
    let Some(wanted) = thread_mailbox else { return Ok(available.into_iter().next()) };
    if let Some(mb) = available.iter().find(|m| m.id == wanted) {
        return Ok(Some(mb.clone()));
    }
    // A mailbox Deliverability held keeps its threads waiting for the user to resume it, rather than moving them to another sender.
    let held: bool = sqlx::query_scalar("select exists (select 1 from mailboxes where id = $1 and status = 'paused' and health_held_at is not null)")
        .bind(wanted)
        .fetch_one(db)
        .await?;
    if held {
        return Ok(None);
    }
    let thread_mailbox = sqlx::query_as::<_, Mailbox>("select * from mailboxes where id = $1 and status = 'active'")
        .bind(wanted)
        .fetch_optional(db)
        .await?;
    match thread_mailbox {
        // The daily limit paces cold email. A lead waiting for an answer gets it today.
        Some(mb) if REPLY_KINDS.contains(&email.kind.as_str()) => Ok(Some(mb)),
        // A follow-up waits for the thread's mailbox to have room again rather than switch senders mid-thread.
        Some(_) => Ok(None),
        None => Ok(available.into_iter().next()),
    }
}

/// Writes the email as the sending mailbox: its signature and booking link, when it has them, in place of the project's,
/// so a team sending from several people's mailboxes signs each email as the person it comes from and books the call on
/// their calendar. The mailbox is only known now, after the email was written and approved.
async fn as_mailbox(db: &sqlx::PgPool, email: &EmailMessage, mb: &Mailbox) -> anyhow::Result<String> {
    let (signature, link) = (mb.signature.trim(), mb.booking_link.trim());
    if (signature.is_empty() && link.is_empty()) || !WRITTEN_KINDS.contains(&email.kind.as_str()) {
        return Ok(email.body.clone());
    }
    let (project_signature, sender_name, project_link): (String, String, String) =
        sqlx::query_as("select signature, sender_name, booking_link from project_settings where project_id = $1")
            .bind(email.project_id)
            .fetch_optional(db)
            .await?
            .unwrap_or_default();
    Ok(rewrite(&email.body, (agent::signature(&project_signature, &sender_name), signature), (project_link.trim(), link)))
}

/// Swaps each (old, new) pair whose new value is set. The signature goes first, since it can hold the booking link and
/// then wouldn't match the project's anymore.
fn rewrite(body: &str, (old_signature, signature): (&str, &str), (old_link, link): (&str, &str)) -> String {
    let body = if signature.is_empty() { body.to_string() } else { swap_signature(body, old_signature, signature) };
    if link.is_empty() { body } else { swap_link(&body, old_link, link) }
}

/// Replaces each `old` link in `body` with `new`. A longer link that starts with it, like another page on the same
/// calendar, is left alone.
fn swap_link(body: &str, old: &str, new: &str) -> String {
    if old.is_empty() {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find(old) {
        let after = &rest[at + old.len()..];
        // The link ends at a space, the end, closing punctuation, or a full stop ending the sentence.
        let ends = after.chars().next().is_none_or(|c| c.is_whitespace() || ",;:!)>]\"'".contains(c))
            || after.strip_prefix('.').is_some_and(|a| a.chars().next().is_none_or(char::is_whitespace));
        out.push_str(&rest[..at]);
        out.push_str(if ends { new } else { old });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Replaces the `old` signature at the end of `body` with `new`. A body that no longer ends with it was edited by hand,
/// so it's left alone rather than signed twice.
fn swap_signature(body: &str, old: &str, new: &str) -> String {
    let body = body.trim_end();
    let unsigned = if old.is_empty() { Some(body) } else { body.strip_suffix(old).and_then(|rest| rest.strip_suffix("\n\n")) };
    match unsigned {
        Some(rest) => format!("{}\n\n{new}", rest.trim_end()),
        None => body.to_string(),
    }
}

/// Cold emails carry an unsubscribe footer. Replies go without one, like any other message in a conversation.
pub(super) fn with_footer(kind: &str, body: &str, unsubscribe_page: &str) -> String {
    if kind == "initial" || kind == "follow_up" {
        format!("{}\n\n--\nNot relevant? Unsubscribe: {unsubscribe_page}", body.trim_end())
    } else {
        body.trim_end().to_string()
    }
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
    let Some(mb) = pick_mailbox(&state.db, org_id, &email).await? else {
        return release(state, &email, Skip::Wait).await;
    };
    let to = lead.email.clone().unwrap_or_default();
    // After a mailbox is free, so an email waiting for room isn't verified, and paid for, on every tick.
    if let Some(skip) = verify_first(state, &email, &lead, &to).await? {
        return release(state, &email, skip).await;
    }

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
    let unsubscribe_page = format!("{}/unsubscribe/{}", state.cfg.unsubscribe_url, lead.unsubscribe_token);
    let one_click = format!("{}/api/unsubscribe/{}", state.cfg.unsubscribe_url, lead.unsubscribe_token);
    // Saved as sent, so the app shows the signature the lead got.
    let email = EmailMessage { body: as_mailbox(&state.db, &email, &mb).await?, ..email };
    let body = with_footer(&email.kind, &email.body, &unsubscribe_page);

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
            warmup: None,
        },
    );
    let message = match built {
        Ok(m) => m,
        Err(e) => {
            sqlx::query("update email_messages set status = 'failed', error = $2 where id = $1").bind(email.id).bind(e.to_string()).execute(&state.db).await?;
            return Ok(());
        }
    };
    let transport = match smtp::transport(state, &mb).await {
        Ok(t) => t,
        Err(e) => return mailbox_failed(state, &mb, &email, &e.to_string(), recheck_after_setup(&e)).await,
    };
    let raw = message.formatted();

    match smtp::send(&transport, message).await {
        Ok(()) => {
            record_sent(state, &email, &lead, &mb, &message_id, in_reply_to.as_deref(), &to).await?;
            copy_to_bcc(state, &transport, &mb, email.project_id, &raw).await;
            Ok(())
        }
        Err(e) => {
            // A 4xx asks us to try again later, often a server greylisting a sender it hasn't seen before, so the mailbox is
            // fine. Anything else that isn't about the recipient is a problem with the mailbox or the connection.
            if smtp::recipient_rejected(&e) {
                sqlx::query("update email_messages set status = 'failed', error = $2 where id = $1")
                    .bind(email.id)
                    .bind(format!("Rejected by the recipient's server: {e}"))
                    .execute(&state.db)
                    .await?;
                super::suppress_lead(state, &lead, Some(org_id), "bounced", "bounce").await?;
                Ok(())
            } else if e.is_transient() {
                defer(&state.db, &email, &e.to_string()).await
            } else {
                mailbox_failed(state, &mb, &email, &format!("SMTP: {e}"), recheck_after(&e)).await
            }
        }
    }
}

/// Sends a copy of a sent email to the project's BCC address, like a CRM's logging address. It goes in an SMTP
/// transaction of its own, so a refused copy can't fail or bounce the email the lead got. How it went shows on the
/// integrations page.
async fn copy_to_bcc(state: &AppState, transport: &smtp::Transport, mb: &Mailbox, project_id: Uuid, raw: &[u8]) {
    let bcc: Option<String> = sqlx::query_scalar("select bcc_email from project_settings where project_id = $1 and bcc_email <> ''")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(bcc) = bcc else { return };
    let result = async {
        let envelope = lettre::address::Envelope::new(Some(mb.email.parse()?), vec![bcc.parse()?])?;
        // lettre's message already holds the server's reply, which its source would repeat.
        lettre::AsyncTransport::send_raw(transport, &envelope, raw).await.map_err(|e| anyhow::anyhow!("SMTP: {e}"))?;
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = &result {
        tracing::warn!(error = %e, "bcc copy failed");
    }
    hooks::record(&state.db, project_id, Alert::Bcc, &bcc, &result).await;
}

async fn release(state: &AppState, email: &EmailMessage, skip: Skip) -> anyhow::Result<()> {
    match skip {
        Skip::Cancel(reason) | Skip::Fail(reason) => {
            let status = if matches!(skip, Skip::Fail(_)) { "failed" } else { "cancelled" };
            sqlx::query("update email_messages set status = $3, error = $2 where id = $1")
                .bind(email.id)
                .bind(reason)
                .bind(status)
                .execute(&state.db)
                .await?;
        }
        Skip::Wait => {
            sqlx::query("update email_messages set status = 'approved' where id = $1").bind(email.id).execute(&state.db).await?;
        }
    }
    Ok(())
}

/// Tries a deferred email again later, waiting longer each time: 30 minutes, then an hour, and so on, about 18 hours in
/// all. Then it gives up, since the server is unlikely to change its mind.
const MAX_DEFERRALS: i32 = 8;

async fn defer(db: &sqlx::PgPool, email: &EmailMessage, error: &str) -> anyhow::Result<()> {
    if email.deferrals + 1 >= MAX_DEFERRALS {
        sqlx::query("update email_messages set status = 'failed', deferrals = deferrals + 1, error = $2 where id = $1")
            .bind(email.id)
            .bind(format!("The recipient's server kept asking to try again later: {error}"))
            .execute(db)
            .await?;
        return Ok(());
    }
    sqlx::query(
        "update email_messages set status = 'approved', deferrals = deferrals + 1,
           scheduled_for = now() + make_interval(mins => 30 * (deferrals + 1)), error = $2
         where id = $1",
    )
    .bind(email.id)
    .bind(format!("The recipient's server asked to try again later: {error}"))
    .execute(db)
    .await?;
    Ok(())
}

/// How long a mailbox whose server couldn't be reached waits before the sender tests it again.
const RECHECK_MINUTES: i32 = 15;
/// Gmail doesn't say when its daily limit resets, so a mailbox over quota tries once an hour. Each try costs one rejected
/// send, and the email goes back in the queue.
const QUOTA_RECHECK_MINUTES: i32 = 60;

/// Stops a mailbox. An unreachable server is often a short outage and a sending quota resets, so those are tested again
/// after `recheck` minutes. Anything else, like a rejected login, waits for someone to fix it and press Test, since
/// retrying a bad password can lock the account.
async fn mailbox_failed(state: &AppState, mb: &Mailbox, email: &EmailMessage, error: &str, recheck: Option<i32>) -> anyhow::Result<()> {
    stop_mailbox(&state.db, mb, error, recheck).await?;
    sqlx::query("update email_messages set status = 'approved', error = $2 where id = $1").bind(email.id).bind(error).execute(&state.db).await?;
    Ok(())
}

/// Sets the mailbox to error, which stops its campaign and warmup sends until it works again.
pub(super) async fn stop_mailbox(db: &sqlx::PgPool, mb: &Mailbox, error: &str, recheck: Option<i32>) -> sqlx::Result<()> {
    tracing::warn!(mailbox = %mb.email, %error, "mailbox failed, pausing it");
    sqlx::query("update mailboxes set status = 'error', last_error = $2, recheck_at = now() + make_interval(mins => $3) where id = $1")
        .bind(mb.id)
        .bind(error)
        .bind(recheck)
        .execute(db)
        .await?;
    Ok(())
}

/// When the sender tests a mailbox stopped by this SMTP error again on its own. None waits for someone to press Test.
pub(super) fn recheck_after(e: &lettre::transport::smtp::Error) -> Option<i32> {
    if smtp::unreachable(e) {
        Some(RECHECK_MINUTES)
    } else if smtp::over_quota(e) {
        Some(QUOTA_RECHECK_MINUTES)
    } else {
        None
    }
}

/// When the sender tests a mailbox again whose transport couldn't be set up. Only a sign-in provider that was down or busy
/// is worth it. A bad setting or a revoked sign-in waits for someone.
pub(super) fn recheck_after_setup(e: &anyhow::Error) -> Option<i32> {
    super::oauth::retry_later(e).then_some(RECHECK_MINUTES)
}

/// Tests the mailboxes stopped by an unreachable server or a sending quota, and puts each one back to work once its server
/// answers again. A quota that hasn't reset yet stops it again on its next send.
pub async fn recheck_mailboxes(state: &AppState) -> anyhow::Result<()> {
    let due = sqlx::query_as::<_, Mailbox>("select * from mailboxes where status = 'error' and recheck_at <= now()").fetch_all(&state.db).await?;
    for mb in due {
        let (status, error, recheck) = match smtp::transport(state, &mb).await {
            Err(e) => ("error", e.to_string(), recheck_after_setup(&e).is_some()),
            Ok(t) => match t.test_connection().await {
                Ok(true) => ("active", String::new(), false),
                // Lettre hides why NOOP failed, but the login had already worked, so testing again can't lock the account.
                Ok(false) => ("error", "SMTP: the server did not answer NOOP".to_string(), true),
                Err(e) => ("error", format!("SMTP: {e}"), smtp::unreachable(&e)),
            },
        };
        tracing::info!(mailbox = %mb.email, %status, "rechecked a stopped mailbox");
        // The status check leaves alone a mailbox someone fixed or paused while the test ran.
        sqlx::query(
            "update mailboxes set status = $2, last_error = $3, recheck_at = case when $4 then now() + make_interval(mins => $5) end
             where id = $1 and status = 'error'",
        )
        .bind(mb.id)
        .bind(status)
        .bind(&error)
        .bind(recheck)
        .bind(RECHECK_MINUTES)
        .execute(&state.db)
        .await?;
    }
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
                                   to_email = $6, cost_micros = $7, error = '', body = $8
         where id = $1",
    )
    .bind(email.id)
    .bind(message_id)
    .bind(in_reply_to)
    .bind(mb.id)
    .bind(&mb.email)
    .bind(to)
    .bind(mb.cost_per_email_micros)
    .bind(&email.body)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "update leads set last_contacted_at = now(), status = case when status in ('new', 'queued') then 'contacted' else status end where id = $1",
    )
    .bind(lead.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("update mailboxes set first_sent_on = current_date where id = $1 and first_sent_on is null").bind(mb.id).execute(&mut *tx).await?;
    if email.kind == "initial" {
        queue_linkedin(&mut *tx, lead.id).await?;
    }

    tx.commit().await?;
    // The body as written, without the unsubscribe footer. Keep in sync with the email_sent note on the integrations page.
    let sent = json!({
        "id": email.id, "kind": email.kind, "step": email.step, "subject": email.subject, "body": email.body,
        "from": mb.email, "to": to, "message_id": message_id,
    });
    crate::hooks::send_lead_event(state, lead.project_id, "email_sent", lead, Some(("email", sent))).await;
    Ok(())
}

/// With LinkedIn on, a lead's first email also gets their LinkedIn sequence written, so the connect note comes due the same day.
/// Leads without a profile, or with a sequence written or being written, are left alone.
async fn queue_linkedin<'e, E: sqlx::PgExecutor<'e>>(db: E, lead: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "insert into agent_runs (kind, project_id, org_id, input)
         select $2, p.id, p.org_id, jsonb_build_object('lead_id', l.id)
         from leads l
         join projects p on p.id = l.project_id
         join project_settings ps on ps.project_id = l.project_id
         where l.id = $1 and l.linkedin_url <> '' and p.org_id is not null and coalesce((ps.channels ->> 'linkedin')::boolean, false)
           and not exists (select 1 from linkedin_messages m where m.lead_id = l.id)
           and not exists (select 1 from agent_runs r where r.kind = $2 and r.status in ('queued', 'running') and r.input ->> 'lead_id' = l.id::text)",
    )
    .bind(lead)
    .bind(kind::LINKEDIN_SEQUENCE)
    .execute(db)
    .await?;
    Ok(())
}

/// Leads whose next follow-up is due within 12 hours get one written by the agent. A campaign brought over from another tool
/// follows its own steps and their delays and stops after its last one. Other campaigns follow the project's follow-ups.
pub(crate) async fn schedule_follow_ups(state: &AppState) -> anyhow::Result<()> {
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "select l.project_id, p.org_id, l.id
         from leads l
         join projects p on p.id = l.project_id
         join project_settings ps on ps.project_id = l.project_id
         left join segments s on s.id = l.segment_id
         join lateral (select e.step, e.sent_at from email_messages e
                       where e.lead_id = l.id and e.direction = 'outbound' and e.kind in ('initial', 'follow_up') and e.status = 'sent'
                       order by e.step desc limit 1) last on true
         left join segment_steps own on own.segment_id = l.segment_id and own.position = last.step + 1
         where p.org_id is not null and not ps.paused and l.status = 'contacted'
           and coalesce(s.status, 'working') <> 'paused'
           and (own.segment_id is not null
                or (not exists (select 1 from segment_steps x where x.segment_id = l.segment_id)
                    and jsonb_typeof(ps.follow_ups) = 'array' and jsonb_array_length(ps.follow_ups) > last.step
                    and coalesce((ps.follow_ups -> last.step ->> 'enabled')::boolean, false)))
           and last.sent_at + make_interval(days => greatest(1, coalesce(own.delay_days, (ps.follow_ups -> last.step ->> 'delay_days')::int, 3)))
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

/// Hourly: campaigns with enough data move between scaling, working and paused by emails sent per hot lead, and A/B tests
/// with auto-pick on keep their winner.
pub async fn optimize(db: &sqlx::PgPool) -> anyhow::Result<()> {
    let rows: Vec<(Uuid, String, bool, i64, i64)> = sqlx::query_as(
        "select s.id, s.status, s.auto_paused_at is not null,
                (select count(*) from email_messages e where e.segment_id = s.id and e.status = 'sent'),
                (select count(*) from leads l where l.segment_id = s.id and l.status in ('hot', 'meeting_booked'))
         from segments s join project_settings ps on ps.project_id = s.project_id
         where s.status in ('working', 'scaling') and not ps.paused",
    )
    .fetch_all(db)
    .await?;
    for (id, status, paused_before, sent, hot) in rows {
        let Some(mut next) = campaign_status(sent, hot) else { continue };
        // You resumed it after we paused it, so it's yours to stop now.
        if next == "paused" && paused_before {
            next = "working";
        }
        if next != status {
            tracing::info!(segment_id = %id, from = %status, to = next, "optimizer moved campaign");
            sqlx::query("update segments set status = $2, auto_paused_at = case when $2 = 'paused' then now() else auto_paused_at end where id = $1")
                .bind(id)
                .bind(next)
                .execute(db)
                .await?;
        }
    }
    crate::ab_test::auto_pick(db).await
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
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::{MAX_DEFERRALS, campaign_status, defer, mailboxes_with_room, optimize, pick_mailbox, queue_linkedin, replace_guessed, rewrite, swap_link, swap_signature};
    use crate::models::{EmailMessage, Lead};
    use crate::verify::Found;

    #[test]
    fn the_sending_mailbox_links_its_own_calendar() {
        let body = "Worth a chat?\nhttps://cal.com/acme\n\nOr see https://cal.com/acme. Prices: https://cal.com/acme/pricing";
        let want = "Worth a chat?\nhttps://cal.com/jo\n\nOr see https://cal.com/jo. Prices: https://cal.com/acme/pricing";
        assert_eq!(swap_link(body, "https://cal.com/acme", "https://cal.com/jo"), want);
        assert_eq!(swap_link("Worth a chat?", "", "https://cal.com/jo"), "Worth a chat?");
    }

    #[test]
    fn a_project_signature_holding_the_booking_link_is_still_replaced() {
        let body = "Worth a chat?\n\nAlex\nhttps://cal.com/acme";
        let got = rewrite(body, ("Alex\nhttps://cal.com/acme", "Jo\nhttps://cal.com/jo"), ("https://cal.com/acme", "https://cal.com/jo"));
        assert_eq!(got, "Worth a chat?\n\nJo\nhttps://cal.com/jo");
    }

    #[test]
    fn the_sending_mailbox_signs_in_place_of_the_project() {
        assert_eq!(swap_signature("Hi Sam,\n\nWorth a chat?\n\nAlex\nAcme\n", "Alex\nAcme", "Jo Lee\nAcme"), "Hi Sam,\n\nWorth a chat?\n\nJo Lee\nAcme");
        // A project with no signature or sender name signs nothing, so the mailbox's goes under the body.
        assert_eq!(swap_signature("Worth a chat?", "", "Jo"), "Worth a chat?\n\nJo");
        // Edited by hand to end some other way, so it's sent as it is.
        assert_eq!(swap_signature("Worth a chat?\n\nCheers, Alex B", "Alex", "Jo"), "Worth a chat?\n\nCheers, Alex B");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn first_emails_wait_once_autopilot_reaches_its_lead_cap(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        sqlx::query(
            "insert into project_settings (project_id, paused, send_days, send_start_hour, send_end_hour, autopilot, autopilot_lead_cap)
             values ($1, false, '{1,2,3,4,5,6,7}', 0, 24, true, 1)",
        )
        .bind(project)
        .execute(&db)
        .await
        .unwrap();
        // One lead emailed this month already. The next is on the do-not-contact list, so a send attempt cancels it
        // without SMTP, and an email the cap holds stays approved.
        sqlx::query(
            "with l as (insert into leads (project_id, email, status) values ($1, 'ana@sent.example', 'contacted') returning id)
             insert into email_messages (project_id, lead_id, direction, kind, status, sent_at) select $1, id, 'outbound', 'initial', 'sent', now() from l",
        )
        .bind(project)
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, 'bo@held.example', 'email', 'manual')")
            .bind(org)
            .execute(&db)
            .await
            .unwrap();
        let email: Uuid = sqlx::query_scalar(
            "with l as (insert into leads (project_id, email, status) values ($1, 'bo@held.example', 'queued') returning id)
             insert into email_messages (project_id, lead_id, direction, kind, status) select $1, id, 'outbound', 'initial', 'approved' from l
             returning id",
        )
        .bind(project)
        .fetch_one(&db)
        .await
        .unwrap();
        let state = crate::autopilot::tests::state(db.clone());
        let status = || sqlx::query_scalar::<_, String>("select status from email_messages where id = $1").bind(email).fetch_one(&db);

        super::tick(&state).await.unwrap();
        assert_eq!(status().await.unwrap(), "approved");

        sqlx::query("update project_settings set autopilot = false").execute(&db).await.unwrap();
        super::tick(&state).await.unwrap();
        assert_eq!(status().await.unwrap(), "cancelled");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_ramping_mailbox_sends_10_then_4_more_a_day(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id) values ($1) returning id").bind(project).fetch_one(&db).await.unwrap();
        let mailbox: Uuid = sqlx::query_scalar(
            "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, daily_limit, ramp_up)
             values ($1, 'new@example.test', 'smtp.example.test', 'new', 'x', 40, true) returning id",
        )
        .bind(org)
        .fetch_one(&db)
        .await
        .unwrap();
        let send = |n: i32| {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status, sent_at)
                 select $1, $2, $3, 'outbound', 'initial', 'sent', now() from generate_series(1, $4)",
            )
            .bind(project)
            .bind(lead)
            .bind(mailbox)
            .bind(n)
            .execute(&db)
        };
        let has_room = || async { !mailboxes_with_room(&db, org).await.unwrap().is_empty() };

        // Its first day: 10.
        send(9).await.unwrap();
        assert!(has_room().await);
        send(1).await.unwrap();
        assert!(!has_room().await);

        // Three days after its first send: 10 + 3 * 4.
        sqlx::query("update mailboxes set first_sent_on = current_date - 3 where id = $1").bind(mailbox).execute(&db).await.unwrap();
        send(11).await.unwrap();
        assert!(has_room().await);
        send(1).await.unwrap();
        assert!(!has_room().await);

        // Once the ramp passes the daily limit, the limit applies.
        sqlx::query("update mailboxes set first_sent_on = current_date - 30 where id = $1").bind(mailbox).execute(&db).await.unwrap();
        send(17).await.unwrap();
        assert!(has_room().await);
        send(1).await.unwrap();
        assert!(!has_room().await);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_warming_mailbox_keeps_campaign_and_warmup_emails_under_its_total(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id) values ($1) returning id").bind(project).fetch_one(&db).await.unwrap();
        let mailbox = |email: &'static str| {
            sqlx::query_scalar::<_, Uuid>(
                "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, daily_limit, warmup, warmup_started_on,
                                        total_daily_limit)
                 values ($1, $2, 'smtp.example.test', 'x', 'x', 50, true, current_date - 20, 60) returning id",
            )
            .bind(org)
            .bind(email)
            .fetch_one(&db)
        };
        let (warming, peer) = (mailbox("warm@alpha.test").await.unwrap(), mailbox("peer@beta.test").await.unwrap());
        sqlx::query("update mailboxes set status = 'paused' where id = $1").bind(peer).execute(&db).await.unwrap();
        let campaign = |n: i32| {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status, sent_at)
                 select $1, $2, $3, 'outbound', 'initial', 'sent', now() from generate_series(1, $4)",
            )
            .bind(project)
            .bind(lead)
            .bind(warming)
            .bind(n)
            .execute(&db)
        };
        let warmup = |n: i32, landed: &'static str, days_ago: i32| {
            sqlx::query(
                "insert into warmup_emails (token, from_mailbox_id, to_mailbox_id, subject, message_id, status, sent_at, landed)
                 select gen_random_uuid()::text, $1, $2, 'Hi', gen_random_uuid()::text, 'sent', now() - make_interval(days => $3), nullif($4, '')
                 from generate_series(1, $5)",
            )
            .bind(warming)
            .bind(peer)
            .bind(days_ago)
            .bind(landed)
            .bind(n)
            .execute(&db)
        };
        let set = |sql: &'static str| sqlx::query(sql).bind(warming).execute(&db);
        let has_room = || async { !mailboxes_with_room(&db, org).await.unwrap().is_empty() };

        // 30 warmup and 29 campaign emails today, under the total of 60 and the daily limit of 50.
        warmup(30, "", 0).await.unwrap();
        campaign(29).await.unwrap();
        assert!(has_room().await);
        campaign(1).await.unwrap();
        assert!(!has_room().await, "60 in all");
        // Without warmup, only the daily limit counts, like before warmup existed.
        set("update mailboxes set warmup = false where id = $1").await.unwrap();
        assert!(has_room().await);

        // Held until warm: on day 6 of warmup no campaign goes out.
        set("update mailboxes set warmup = true, total_daily_limit = 1000, hold_until_warm = true, warmup_started_on = current_date - 5 where id = $1")
            .await
            .unwrap();
        assert!(!has_room().await);
        set("update mailboxes set warmup_started_on = current_date - 14 where id = $1").await.unwrap();
        assert!(has_room().await, "day 15");
        // Past day 14 but landing in spam: 3 of the last 10 the peers saw.
        warmup(7, "inbox", 2).await.unwrap();
        warmup(3, "spam", 2).await.unwrap();
        assert!(!has_room().await);
        set("update mailboxes set hold_until_warm = false where id = $1").await.unwrap();
        assert!(has_room().await);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_reply_goes_out_after_the_mailbox_hits_its_daily_limit(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id) values ($1) returning id").bind(project).fetch_one(&db).await.unwrap();
        let mailbox: Uuid = sqlx::query_scalar(
            "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, daily_limit)
             values ($1, 'full@example.test', 'smtp.example.test', 'full', 'x', 3) returning id",
        )
        .bind(org)
        .fetch_one(&db)
        .await
        .unwrap();
        // The lead's thread started from this mailbox, which has used up today's limit.
        sqlx::query(
            "insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status, sent_at)
             select $1, $2, $3, 'outbound', 'initial', 'sent', now() from generate_series(1, 3)",
        )
        .bind(project)
        .bind(lead)
        .bind(mailbox)
        .execute(&db)
        .await
        .unwrap();
        let queued = |kind: &'static str| {
            sqlx::query_as::<_, EmailMessage>(
                "insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'outbound', $3, 'approved') returning *",
            )
            .bind(project)
            .bind(lead)
            .bind(kind)
            .fetch_one(&db)
        };

        for kind in ["manual_reply", "auto_reply"] {
            let reply = queued(kind).await.unwrap();
            assert_eq!(pick_mailbox(&db, org, &reply).await.unwrap().map(|m| m.id), Some(mailbox), "{kind}");
        }
        let follow_up = queued("follow_up").await.unwrap();
        assert!(pick_mailbox(&db, org, &follow_up).await.unwrap().is_none());

        // Held by Deliverability, the thread waits for it even with another mailbox free, replies included.
        sqlx::query("update mailboxes set status = 'paused', health_held_at = now() where id = $1").bind(mailbox).execute(&db).await.unwrap();
        sqlx::query("insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, 'free@example.test', 'smtp.example.test', 'free', 'x')")
            .bind(org)
            .execute(&db)
            .await
            .unwrap();
        for kind in ["follow_up", "manual_reply"] {
            assert!(pick_mailbox(&db, org, &queued(kind).await.unwrap()).await.unwrap().is_none(), "{kind}");
        }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_deferred_email_waits_longer_each_time_then_fails(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id) values ($1) returning id").bind(project).fetch_one(&db).await.unwrap();
        let id: Uuid = sqlx::query_scalar(
            "insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'outbound', 'initial', 'sending') returning id",
        )
        .bind(project)
        .bind(lead)
        .fetch_one(&db)
        .await
        .unwrap();
        let load = || sqlx::query_as::<_, EmailMessage>("select * from email_messages where id = $1").bind(id).fetch_one(&db);

        for attempt in 1..MAX_DEFERRALS {
            defer(&db, &load().await.unwrap(), "451 4.7.1 Greylisted").await.unwrap();
            let email = load().await.unwrap();
            assert_eq!((email.status.as_str(), email.deferrals), ("approved", attempt));
            let wait = (email.scheduled_for.unwrap() - chrono::Utc::now()).num_minutes();
            assert!((30 * attempt as i64 - 1..=30 * attempt as i64).contains(&wait), "attempt {attempt} waits {wait} minutes");
        }
        defer(&db, &load().await.unwrap(), "451 4.7.1 Greylisted").await.unwrap();
        let email = load().await.unwrap();
        assert_eq!((email.status.as_str(), email.deferrals), ("failed", MAX_DEFERRALS));
        assert!(email.error.contains("kept asking"));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_resumed_campaign_is_not_paused_again(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        sqlx::query("insert into project_settings (project_id, paused) values ($1, false)").bind(project).execute(&db).await.unwrap();
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name, status) values ($1, 'Costly', 'working') returning id")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, segment_id, status) values ($1, $2, 'contacted') returning id")
            .bind(project)
            .bind(segment)
            .fetch_one(&db)
            .await
            .unwrap();
        // 700 sends and no hot lead is over the 625-per-hot-lead line.
        sqlx::query(
            "insert into email_messages (project_id, lead_id, segment_id, direction, kind, status, sent_at)
             select $1, $2, $3, 'outbound', 'initial', 'sent', now() from generate_series(1, 700)",
        )
        .bind(project)
        .bind(lead)
        .bind(segment)
        .execute(&db)
        .await
        .unwrap();
        let status = || sqlx::query_scalar::<_, String>("select status from segments where id = $1").bind(segment).fetch_one(&db);

        optimize(&db).await.unwrap();
        assert_eq!(status().await.unwrap(), "paused");

        sqlx::query("update segments set status = 'working' where id = $1").bind(segment).execute(&db).await.unwrap();
        optimize(&db).await.unwrap();
        assert_eq!(status().await.unwrap(), "working");
    }

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

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_found_address_replaces_a_guess_unless_another_lead_has_it(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(&db).await.unwrap();
        let lead = |email: &'static str| {
            sqlx::query_as::<_, Lead>("insert into leads (project_id, full_name, email, email_status) values ($1, 'Sam Lee', $2, 'guessed') returning *")
                .bind(project)
                .bind(email)
                .fetch_one(&db)
        };
        let guessed = lead("s.lee@acme.example").await.unwrap();
        lead("Sam@acme.example").await.unwrap();

        let taken = Found { email: "sam@acme.example".into(), verified: true };
        assert!(!replace_guessed(&db, &guessed, &taken).await.unwrap());
        let fresh = Found { email: "samuel@acme.example".into(), verified: false };
        assert!(replace_guessed(&db, &guessed, &fresh).await.unwrap());
        let (email, status): (String, String) =
            sqlx::query_as("select email::text, email_status from leads where id = $1").bind(guessed.id).fetch_one(&db).await.unwrap();
        assert_eq!((email.as_str(), status.as_str()), ("samuel@acme.example", "found"));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_first_email_queues_one_linkedin_sequence_when_linkedin_is_on(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(&db).await.unwrap();
        let lead = |url: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, linkedin_url) values ($1, $2) returning id")
                .bind(project)
                .bind(url)
                .fetch_one(&db)
        };
        let (with_profile, without) = (lead("https://www.linkedin.com/in/sam").await.unwrap(), lead("").await.unwrap());
        let runs = || async {
            sqlx::query_scalar::<_, i64>("select count(*) from agent_runs where kind = 'linkedin_sequence'").fetch_one(&db).await.unwrap()
        };

        // LinkedIn is off by default.
        queue_linkedin(&db, with_profile).await.unwrap();
        assert_eq!(runs().await, 0);

        sqlx::query("update project_settings set channels = '{\"email\": true, \"linkedin\": true}' where project_id = $1")
            .bind(project)
            .execute(&db)
            .await
            .unwrap();
        for lead in [with_profile, with_profile, without] {
            queue_linkedin(&db, lead).await.unwrap();
        }
        let input: serde_json::Value = sqlx::query_scalar("select input from agent_runs").fetch_one(&db).await.unwrap();
        assert_eq!((runs().await, input), (1, serde_json::json!({ "lead_id": with_profile })));
    }
}
