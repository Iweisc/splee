//! Warmup: the mailboxes with it on email each other, across every org in this Splee, so mailbox providers see a new
//! address get mail that is read, answered and taken out of spam before it sends campaigns. Only this module sees which
//! mailboxes those are. The routes give each org counts for its own mailboxes alone.
//!
//! The warmup worker sends. The IMAP poller receives: a warmup email in INBOX is marked read, sometimes flagged and
//! answered, then archived, and one in the spam folder is moved to INBOX first, so the next poll handles it the same way.
//! It is archived rather than deleted because a deleted email reads to Gmail and Outlook as unwanted, and the archive
//! keeps it out of the user's way: the folder marked \Archive (Outlook, iCloud, Fastmail), else \All (Gmail, where moving
//! out of INBOX removes the Inbox label), else an "Archive" folder Splee creates. A server that can neither MOVE nor
//! COPY with UIDPLUS keeps it in INBOX, read.

use std::time::Duration;

use anyhow::anyhow;
use chrono::{TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use futures::TryStreamExt;
use mail_parser::MessageParser;
use uuid::Uuid;

use super::imap::{self, ImapSession};
use super::{sender, smtp};
use crate::models::{Mailbox, Ts, WarmupHealth};
use crate::state::AppState;

pub const HEADER: &str = "X-Splee-Warmup";
/// Starts the code in a warmup email's last line, for a receiving server that drops unknown headers.
const CODE_PREFIX: &str = "SW-";
const TOKEN_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const TOKEN_LEN: usize = 8;

/// Campaigns from a mailbox held until warm start after this many days of warmup.
pub const HOLD_DAYS: i32 = 14;
const RAMP_START: i64 = 2;
const RAMP_STEP: i64 = 2;
/// Out of 100: the received warmup emails that get a reply, and those flagged as important.
const REPLY_SHARE: usize = 35;
const FLAG_SHARE: usize = 20;

/// The mailboxes that send and receive warmup emails. Reading the replies and spam folder needs IMAP, and a paused or
/// stopped mailbox sends nothing, warmup included.
const POOL: &str = "m.warmup and m.status = 'active' and m.imap_host <> ''";

/// Warmup emails mailbox `m` sent today, as a subquery. A warming mailbox's campaign and warmup emails together stay
/// under its total_daily_limit.
pub const WARMUP_SENT_TODAY: &str =
    "select count(*) from warmup_emails x where x.from_mailbox_id = m.id and x.status = 'sent' and x.sent_at >= date_trunc('day', now())";

/// Whether the receiving mailboxes found more than 1 in 5 of mailbox `m`'s warmup emails from the last 14 days in spam,
/// out of at least 10 they saw. Keep in sync with landingInSpam in frontend/src/lib/mailbox.ts.
pub const LANDING_IN_SPAM: &str = "(select count(x.landed) >= 10 and count(*) filter (where x.landed = 'spam') * 5 > count(x.landed)
                                    from warmup_emails x where x.from_mailbox_id = m.id and x.sent_at > now() - interval '14 days')";

/// Warmup emails a mailbox sends on day `days` of warmup, counting from 0: 2, then 2 more a day up to `cap`.
pub fn daily_target(days: i64, cap: i32) -> i32 {
    (RAMP_START + RAMP_STEP * days.max(0)).min(i64::from(cap)) as i32
}

/// Room for one more warmup email today: under the ramp, and under the total with today's campaign emails.
fn has_room(campaign_today: i64, warmup_today: i64, target: i32, total_limit: i32) -> bool {
    warmup_today < i64::from(target) && campaign_today + warmup_today < i64::from(total_limit)
}

/// Seconds until the next warmup email: what's left of the window, less a minute held back per email for the once-a-minute
/// tick, split over one more than the emails left, times 0.5 to 1.5 (`jitter` is 0 to 100) so they don't go out like
/// clockwork. Held-back minutes and the extra slot keep every email of the day inside the window even at the longest gaps.
fn gap_secs(window_left_secs: i64, emails_left: i64, jitter: usize) -> i64 {
    let emails = emails_left.max(1);
    let spare = (window_left_secs - 60 * (emails + 1)).max(0);
    (spare / (emails + 1) * (50 + jitter as i64) / 100).max(60)
}

/// A random number below `n`. Warmup only needs variety, and a v4 UUID's random bits give it without another crate.
fn roll(n: usize) -> usize {
    (Uuid::new_v4().as_u128() % n.max(1) as u128) as usize
}

fn new_token(pick: &mut impl FnMut(usize) -> usize) -> String {
    (0..TOKEN_LEN).map(|_| TOKEN_ALPHABET[pick(TOKEN_ALPHABET.len())] as char).collect()
}

fn is_token(t: &str) -> bool {
    t.len() == TOKEN_LEN && t.bytes().all(|b| TOKEN_ALPHABET.contains(&b))
}

/// The warmup token from the X-Splee-Warmup header, or from the code in the body when a server dropped the header.
pub fn find_token(header: Option<&str>, body: &str) -> Option<String> {
    if let Some(token) = header.map(str::trim).filter(|t| is_token(t)) {
        return Some(token.to_string());
    }
    body.match_indices(CODE_PREFIX).find_map(|(at, _)| {
        let start = at + CODE_PREFIX.len();
        let token = body.get(start..start + TOKEN_LEN)?;
        let alone = body[..at].chars().next_back().is_none_or(|c| !c.is_ascii_alphanumeric())
            && body[start + TOKEN_LEN..].chars().next().is_none_or(|c| !c.is_ascii_alphanumeric());
        (alone && is_token(token)).then(|| token.to_string())
    })
}

fn token_in(raw: &[u8]) -> Option<String> {
    let msg = MessageParser::default().parse(raw)?;
    let body = msg.body_text(0).unwrap_or_default();
    find_token(msg.header_raw(HEADER), &body)
}

// Templates. {name} is the recipient's first name, {me} the sender's. Each email also gets a greeting, a sign-off and
// the slots below, so two emails rarely read the same.

const FRESH: [(&str, &str); 32] = [
    ("Quick question about {thing}", "Do you have the latest version of {thing}? I want to make sure I'm working from the right one before {day}."),
    ("{thing}", "I went through {thing} this morning and left a few notes. Nothing urgent, just a couple of wording changes."),
    ("Catching up {day}?", "Are you free {day} around {time}? I'd like to run a few ideas past you, it shouldn't take more than 20 minutes."),
    ("Following up", "Just following up on what we talked about last week. Let me know if anything changed on your side."),
    ("Notes from today", "Thanks for the time today. I'll tidy up my notes and send them over {day}."),
    ("Re-sending {thing}", "Re-sending {thing} in case it got buried. Happy to walk through it whenever suits you."),
    ("Small change to the plan", "We moved things around a little, so the review is now {day} at {time}. Does that still work for you?"),
    ("Thoughts on {thing}?", "When you get a minute, could you look over {thing}? I'd value a second pair of eyes before it goes out."),
    ("Thanks again", "Thanks again for the help earlier. It saved me a good hour."),
    ("Meeting {day}", "Confirming we're still on for {day} at {time}. I'll send a link beforehand."),
    ("A quick update", "Quick update from my side: {thing} is nearly done. I should have something to share {day}."),
    ("Did this reach you?", "I sent something over yesterday and wanted to check it reached you. Our mail has been a bit slow."),
    ("Question on timing", "Is {day} still realistic for {thing}, or should we push it a little? Either is fine on my end."),
    ("Coffee {day}?", "I'll be near your office {day}. Fancy a coffee around {time}?"),
    ("Draft for review", "Here's where I got to with {thing}. Rough around the edges, but the structure is there."),
    ("One more thing", "One more thing I forgot to mention on the call: we should loop in finance before {day}."),
    ("Re: our call", "Good talking earlier. I'll put the next steps in writing so we're on the same page."),
    ("Checking in", "Just checking in to see how things are going on your end. Anything I can help with this week?"),
    ("{thing} looks good", "Had a look at {thing} and it reads well. Only one small question about the second part."),
    ("Shared folder", "I've moved {thing} into the shared folder so it's easier to find. Shout if you can't open it."),
    ("Rescheduling", "Something came up on my side. Could we move our chat to {day} at {time} instead?"),
    ("Intro", "I mentioned you to a colleague who's working on something similar. Would you be open to a short intro?"),
    ("Before {day}", "Before {day}, could you confirm the numbers in {thing}? I want to double-check one figure."),
    ("Agenda", "Here's a rough agenda for {day}: a quick recap, {thing}, then open questions. Add anything I missed."),
    ("Got a minute?", "Got a minute later today? Nothing urgent, I just want your take on {thing}."),
    ("Weekly summary", "Short summary of the week: most of {thing} is done and the rest should land {day}."),
    ("Next steps", "As promised, the next steps: I'll finish {thing}, you check the dates, and we regroup {day}."),
    ("Small favour", "Small favour to ask: could you forward me {thing} when you get a chance? I can't seem to find it."),
    ("Good news", "Good news, the team signed off on {thing}. Thanks for pushing it along."),
    ("Calendar", "I've put a placeholder in your calendar for {day} at {time}. Move it if it clashes with anything."),
    ("Re: last week", "Picking up the thread from last week. Is {thing} still the priority, or did that change?"),
    ("Heads up", "Heads up that I'll be out {day}. If anything comes up with {thing}, just email and I'll reply when I'm back."),
];

const REPLIES: [&str; 16] = [
    "Thanks, that works for me.",
    "Got it, I'll take a look {day}.",
    "Sounds good. {day} at {time} is fine.",
    "Thanks for sending this over. I'll get back to you shortly.",
    "Perfect, thank you.",
    "Appreciate the update. Let's keep it as planned.",
    "Received, thanks. I'll read it properly this afternoon.",
    "Good timing, I was about to ask. Thanks!",
    "Makes sense to me. Let's talk {day}.",
    "Thanks for the heads up. Noted.",
    "Yes, that still works. See you {day}.",
    "Thanks, I'll forward it to the team.",
    "Great, nothing to add from me.",
    "Thanks! Can we go over it {day} around {time}?",
    "All good on my side, thanks for checking.",
    "Will do. I'll send it over by {day}.",
];

const GREETINGS: [&str; 6] = ["Hi {name},", "Hey {name},", "Hello {name},", "{name},", "Hi {name}!", "Morning {name},"];
const SIGNOFFS: [&str; 8] = ["Thanks,", "Best,", "Cheers,", "Speak soon,", "Thanks again,", "All the best,", "Kind regards,", "Talk soon,"];
const DAYS: [&str; 9] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "tomorrow", "next week", "later this week", "early next week"];
const TIMES: [&str; 8] = ["10am", "11:30", "noon", "2pm", "3:30", "4pm", "9:30", "after lunch"];
const THINGS: [&str; 16] = [
    "the onboarding doc",
    "the Q3 numbers",
    "the pricing sheet",
    "the vendor list",
    "the launch checklist",
    "the board deck",
    "the hiring plan",
    "the partner brief",
    "the renewal list",
    "the roadmap draft",
    "the budget sheet",
    "the case study",
    "the training notes",
    "the contract draft",
    "the event plan",
    "the project timeline",
];

#[derive(Debug, PartialEq)]
pub struct Draft {
    pub subject: String,
    pub body: String,
}

fn fill(template: &str, pick: &mut impl FnMut(usize) -> usize, name: &str, me: &str) -> String {
    let (day, time, thing) = (DAYS[pick(DAYS.len())], TIMES[pick(TIMES.len())], THINGS[pick(THINGS.len())]);
    template.replace("{name}", name).replace("{me}", me).replace("{day}", day).replace("{time}", time).replace("{thing}", thing)
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn letter(pick: &mut impl FnMut(usize) -> usize, body: &str, to: &str, me: &str, token: &str) -> String {
    let greeting = GREETINGS[pick(GREETINGS.len())].replace("{name}", to);
    format!("{greeting}\n\n{body}\n\n{}\n{me}\n\n{CODE_PREFIX}{token}", SIGNOFFS[pick(SIGNOFFS.len())])
}

/// A new warmup email from `me` to `to`, by first name.
pub fn compose(pick: &mut impl FnMut(usize) -> usize, to: &str, me: &str, token: &str) -> Draft {
    let (subject, body) = FRESH[pick(FRESH.len())];
    let subject = capitalize(&fill(subject, pick, to, me));
    let body = fill(body, pick, to, me);
    Draft { subject, body: letter(pick, &body, to, me, token) }
}

pub fn compose_reply(pick: &mut impl FnMut(usize) -> usize, subject: &str, to: &str, me: &str, token: &str) -> Draft {
    let body = fill(REPLIES[pick(REPLIES.len())], pick, to, me);
    Draft { subject: reply_subject(subject), body: letter(pick, &body, to, me, token) }
}

fn reply_subject(subject: &str) -> String {
    if subject.to_lowercase().starts_with("re:") { subject.to_string() } else { format!("Re: {subject}") }
}

/// The name a person would greet the mailbox's owner by: the first word of its from name, else of its address.
fn first_name(mb: &Mailbox) -> String {
    named(&mb.from_name, &mb.email)
}

fn named(from_name: &str, email: &str) -> String {
    if let Some(name) = from_name.split_whitespace().next() {
        return name.to_string();
    }
    capitalize(email.split('@').next().unwrap_or_default().split(['.', '_', '-', '+']).next().unwrap_or_default())
}

#[derive(sqlx::FromRow)]
struct Warming {
    #[sqlx(flatten)]
    mb: Mailbox,
    timezone: String,
    start_hour: i32,
    end_hour: i32,
}

/// Every minute, at most one warmup email per mailbox: an answer that's due, else a new email when its time has come.
/// They go out within the send hours of the org's first project, every day.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    let warming = sqlx::query_as::<_, Warming>(&format!(
        "select m.*, coalesce(w.timezone, 'UTC') as timezone, coalesce(w.send_start_hour, 9) as start_hour, coalesce(w.send_end_hour, 17) as end_hour
         from mailboxes m
         left join lateral (select s.timezone, s.send_start_hour, s.send_end_hour from projects p join project_settings s on s.project_id = p.id
                            where p.org_id = m.org_id order by p.created_at limit 1) w on true
         where {POOL}",
    ))
    .fetch_all(&state.db)
    .await?;
    for w in warming {
        if let Err(e) = warm(state, &w).await {
            tracing::error!(mailbox = %w.mb.email, error = ?e, "warmup failed");
        }
    }
    Ok(())
}

/// Today's send window in `tz`, when `now` is inside it.
fn window(tz: Tz, start_hour: i32, end_hour: i32, now: Ts) -> Option<(Ts, Ts)> {
    let local = now.with_timezone(&tz);
    let hour = local.hour() as i32;
    if hour < start_hour || hour >= end_hour {
        return None;
    }
    let midnight = local.date_naive().and_hms_opt(0, 0, 0)?;
    let at = |h: i32| tz.from_local_datetime(&(midnight + chrono::Duration::hours(i64::from(h)))).earliest().map(|t| t.with_timezone(&Utc));
    Some((at(start_hour)?, at(end_hour)?))
}

async fn warm(state: &AppState, w: &Warming) -> anyhow::Result<()> {
    let now = Utc::now();
    let Some((open, close)) = window(w.timezone.parse().unwrap_or(Tz::UTC), w.start_hour, w.end_hour, now) else { return Ok(()) };
    let days = w.mb.warmup_started_on.map_or(0, |d| (now.date_naive() - d).num_days());
    let target = daily_target(days, w.mb.warmup_daily_cap);
    // Counted now, not when the tick started, since the campaign sender and earlier mailboxes' sends move them.
    let (campaign_today, warmup_today): (i64, i64) = sqlx::query_as(&format!(
        "select (select count(*) from email_messages e where e.mailbox_id = m.id and e.status = 'sent' and e.sent_at >= date_trunc('day', now())),
                ({WARMUP_SENT_TODAY})
         from mailboxes m where m.id = $1"
    ))
    .bind(w.mb.id)
    .fetch_one(&state.db)
    .await?;
    if !has_room(campaign_today, warmup_today, target, w.mb.total_daily_limit) {
        return Ok(());
    }
    // Someone is waiting on an answer in the thread, so it goes before a new email.
    if send_reply(state, &w.mb).await? {
        return Ok(());
    }
    let left = i64::from(target) - warmup_today;
    let next = |sent: i64| now + chrono::Duration::seconds(gap_secs((close - now).num_seconds(), left - sent, roll(101)));
    match w.mb.warmup_next_at {
        Some(at) if at >= open && at > now => return Ok(()),
        Some(at) if at >= open => {}
        // Today's first chance. Pick a time rather than send the moment the window opens.
        _ => return schedule(&state.db, w.mb.id, next(0)).await,
    }
    let Some(peer) = sqlx::query_as::<_, Mailbox>(&format!(
        "select m.* from mailboxes m where {POOL} and m.id <> $1 and lower(m.email::text) <> lower($2)
         order by lower(split_part(m.email::text, '@', 2)) = lower(split_part($2, '@', 2)), random() limit 1"
    ))
    .bind(w.mb.id)
    .bind(&w.mb.email)
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(());
    };
    send_fresh(state, &w.mb, &peer).await?;
    schedule(&state.db, w.mb.id, next(1)).await
}

async fn schedule(db: &sqlx::PgPool, mailbox_id: Uuid, at: Ts) -> anyhow::Result<()> {
    sqlx::query("update mailboxes set warmup_next_at = $2 where id = $1").bind(mailbox_id).bind(at).execute(db).await?;
    Ok(())
}

async fn send_fresh(state: &AppState, from: &Mailbox, to: &Mailbox) -> anyhow::Result<()> {
    let token = new_token(&mut roll);
    let draft = compose(&mut roll, &first_name(to), &first_name(from), &token);
    let message_id = sender::new_message_id(&from.email);
    let id: Uuid = sqlx::query_scalar(
        "insert into warmup_emails (token, from_mailbox_id, to_mailbox_id, subject, message_id, status) values ($1, $2, $3, $4, $5, 'sending') returning id",
    )
    .bind(&token)
    .bind(from.id)
    .bind(to.id)
    .bind(&draft.subject)
    .bind(&message_id)
    .fetch_one(&state.db)
    .await?;
    deliver(state, from, to, id, &token, &draft, &message_id, None).await
}

#[derive(sqlx::FromRow)]
struct DueReply {
    id: Uuid,
    token: String,
    to_mailbox_id: Uuid,
    subject: String,
    message_id: String,
    parent_message_id: String,
}

/// Sends the mailbox's longest-due reply, if one is due. True when it took this tick's send.
async fn send_reply(state: &AppState, from: &Mailbox) -> anyhow::Result<bool> {
    let due = sqlx::query_as::<_, DueReply>(
        "update warmup_emails r set status = 'sending'
         from warmup_emails p
         where r.id = (select id from warmup_emails where from_mailbox_id = $1 and status = 'queued' and send_after <= now() order by send_after limit 1)
           and p.id = r.parent_id and r.status = 'queued'
         returning r.id, r.token, r.to_mailbox_id, r.subject, r.message_id, p.message_id as parent_message_id",
    )
    .bind(from.id)
    .fetch_optional(&state.db)
    .await?;
    let Some(due) = due else { return Ok(false) };
    let to = sqlx::query_as::<_, Mailbox>(&format!("select m.* from mailboxes m where m.id = $1 and {POOL}"))
        .bind(due.to_mailbox_id)
        .fetch_optional(&state.db)
        .await?;
    let Some(to) = to else {
        failed(&state.db, due.id, "The other mailbox stopped warming up").await?;
        return Ok(false);
    };
    let draft = compose_reply(&mut roll, &due.subject, &first_name(&to), &first_name(from), &due.token);
    deliver(state, from, &to, due.id, &due.token, &draft, &due.message_id, Some(&due.parent_message_id)).await?;
    Ok(true)
}

async fn failed(db: &sqlx::PgPool, id: Uuid, error: &str) -> sqlx::Result<()> {
    sqlx::query("update warmup_emails set status = 'failed', error = $2 where id = $1").bind(id).bind(error).execute(db).await?;
    Ok(())
}

/// Sends like the campaign sender does, and a mailbox that fails stops the same way, with the error on the Mailboxes
/// page. A refused recipient or a server asking to try later only fails this email: the next one goes to another peer.
#[allow(clippy::too_many_arguments)]
async fn deliver(
    state: &AppState,
    from: &Mailbox,
    to: &Mailbox,
    id: Uuid,
    token: &str,
    draft: &Draft,
    message_id: &str,
    in_reply_to: Option<&str>,
) -> anyhow::Result<()> {
    let references: Vec<String> = in_reply_to.map(String::from).into_iter().collect();
    let built = smtp::build(
        from,
        &smtp::Outgoing {
            to_name: &to.from_name,
            to_email: &to.email,
            subject: &draft.subject,
            body: &draft.body,
            message_id,
            in_reply_to,
            references: &references,
            unsubscribe_url: None,
            warmup: Some(token),
        },
    );
    let message = match built {
        Ok(m) => m,
        Err(e) => return Ok(failed(&state.db, id, &e.to_string()).await?),
    };
    let transport = match smtp::transport(state, from).await {
        Ok(t) => t,
        Err(e) => {
            failed(&state.db, id, &e.to_string()).await?;
            return Ok(sender::stop_mailbox(&state.db, from, &e.to_string(), sender::recheck_after_setup(&e)).await?);
        }
    };
    match smtp::send(&transport, message).await {
        Ok(()) => {
            sqlx::query("update warmup_emails set status = 'sent', sent_at = now() where id = $1").bind(id).execute(&state.db).await?;
            sqlx::query("update mailboxes set warmup_started_on = current_date where id = $1 and warmup_started_on is null")
                .bind(from.id)
                .execute(&state.db)
                .await?;
        }
        Err(e) if smtp::recipient_rejected(&e) || e.is_transient() => failed(&state.db, id, &format!("SMTP: {e}")).await?,
        Err(e) => {
            failed(&state.db, id, &format!("SMTP: {e}")).await?;
            sender::stop_mailbox(&state.db, from, &format!("SMTP: {e}"), sender::recheck_after(&e)).await?;
        }
    }
    Ok(())
}

/// A warmup email the poller found in a mailbox's INBOX.
#[derive(Debug, sqlx::FromRow)]
pub struct Received {
    id: Uuid,
    from_mailbox_id: Uuid,
    parent_id: Option<Uuid>,
    subject: String,
}

/// The warmup email `raw` is, when its token is one this Splee sent to `mb`, and records that it reached the inbox unless
/// the spam pass already found it in spam. Anything else, a lookalike code included, is ordinary mail.
pub async fn received(db: &sqlx::PgPool, mb: &Mailbox, raw: &[u8]) -> sqlx::Result<Option<Received>> {
    let Some(token) = token_in(raw) else { return Ok(None) };
    sqlx::query_as::<_, Received>(
        "update warmup_emails set landed = coalesce(landed, 'inbox'), landed_at = coalesce(landed_at, now())
         where token = $1 and to_mailbox_id = $2
         returning id, from_mailbox_id, parent_id, subject",
    )
    .bind(token)
    .bind(mb.id)
    .fetch_optional(db)
    .await
}

/// Answers some of the new warmup emails in `found` (UID, email), then marks them all read, flags some and archives them.
pub async fn file_away(state: &AppState, mb: &Mailbox, found: &[(u32, Received)]) -> anyhow::Result<()> {
    if mb.warmup && mb.status == "active" {
        for (_, r) in found.iter().filter(|(_, r)| r.parent_id.is_none() && roll(100) < REPLY_SHARE) {
            queue_reply(&state.db, mb, r).await?;
        }
    }
    let uids: Vec<u32> = found.iter().map(|(uid, _)| *uid).collect();
    let flagged: Vec<u32> = uids.iter().copied().filter(|_| roll(100) < FLAG_SHARE).collect();
    let mut session = imap::connect(state, mb).await?;
    let result = tokio::time::timeout(IMAP_LIMIT, archive(&mut session, &uids, &flagged)).await;
    finish(session, result).await
}

async fn archive(session: &mut ImapSession, uids: &[u32], flagged: &[u32]) -> anyhow::Result<()> {
    session.select("INBOX").await?;
    add_flag(session, uids, "\\Seen").await?;
    add_flag(session, flagged, "\\Flagged").await?;
    let archive = match archive_folder(&folders(session).await?) {
        Some(name) => name,
        None => {
            session.create("Archive").await?;
            "Archive".to_string()
        }
    };
    move_uids(session, uids, &archive).await
}

async fn queue_reply(db: &sqlx::PgPool, mb: &Mailbox, r: &Received) -> sqlx::Result<()> {
    // Between 10 minutes and 2 hours later, like a person getting to their inbox.
    let delay = 10 + roll(110) as i32;
    sqlx::query(
        "insert into warmup_emails (token, from_mailbox_id, to_mailbox_id, parent_id, subject, message_id, send_after)
         values ($1, $2, $3, $4, $5, $6, now() + make_interval(mins => $7))
         on conflict (parent_id) where parent_id is not null do nothing",
    )
    .bind(new_token(&mut roll))
    .bind(mb.id)
    .bind(r.from_mailbox_id)
    .bind(r.id)
    .bind(reply_subject(&r.subject))
    .bind(sender::new_message_id(&mb.email))
    .bind(delay)
    .execute(db)
    .await?;
    Ok(())
}

/// Moves the warmup emails that arrived in the spam folder since the last pass to INBOX, marked read, after recording
/// that they landed in spam. The next poll picks them up from INBOX like the others.
pub async fn rescue_spam(state: &AppState, mb: &Mailbox) -> anyhow::Result<()> {
    let mut session = imap::connect(state, mb).await?;
    let result = tokio::time::timeout(IMAP_LIMIT, rescue(&mut session, &state.db, mb)).await;
    finish(session, result).await
}

async fn rescue(session: &mut ImapSession, db: &sqlx::PgPool, mb: &Mailbox) -> anyhow::Result<()> {
    let Some(junk) = junk_folder(&folders(session).await?) else { return Ok(()) };
    let selected = session.select(&junk).await?;
    let mut after = u32::try_from(mb.warmup_junk_uid).unwrap_or(0);
    // A folder renumbered since the last pass starts over. SINCE keeps that, and the first pass, to two days.
    if selected.uid_next.is_some_and(|next| next <= after) {
        after = 0;
    }
    let since = (Utc::now() - chrono::Duration::days(2)).format("%-d-%b-%Y");
    let mut uids: Vec<u32> = session.uid_search(format!("UID {}:* SINCE {since}", after + 1)).await?.into_iter().filter(|u| *u > after).collect();
    uids.sort_unstable();
    // A spam folder can take thousands a day. The newest 200 a minute is far more than warmup sends.
    let uids = &uids[uids.len().saturating_sub(200)..];
    let Some(&newest) = uids.last() else { return Ok(()) };
    let fetches: Vec<_> = session.uid_fetch(uid_set(uids), "(UID BODY.PEEK[])").await?.try_collect().await?;
    let mut warm = vec![];
    for f in &fetches {
        let (Some(uid), Some(token)) = (f.uid, f.body().and_then(token_in)) else { continue };
        let ours: bool = sqlx::query_scalar(
            "with hit as (update warmup_emails set landed = 'spam', landed_at = now() where token = $1 and to_mailbox_id = $2 and landed is null)
             select exists (select 1 from warmup_emails where token = $1 and to_mailbox_id = $2)",
        )
        .bind(token)
        .bind(mb.id)
        .fetch_one(db)
        .await?;
        if ours {
            warm.push(uid);
        }
    }
    add_flag(session, &warm, "\\Seen").await?;
    move_uids(session, &warm, "INBOX").await?;
    sqlx::query("update mailboxes set warmup_junk_uid = $2 where id = $1").bind(mb.id).bind(i64::from(newest)).execute(db).await?;
    if !warm.is_empty() {
        tracing::info!(mailbox = %mb.email, moved = warm.len(), "moved warmup emails out of spam");
    }
    Ok(())
}

const IMAP_LIMIT: Duration = Duration::from_secs(120);

/// Logs out whether the warmup pass worked, timed out or failed.
async fn finish(mut session: ImapSession, result: Result<anyhow::Result<()>, tokio::time::error::Elapsed>) -> anyhow::Result<()> {
    let _ = session.logout().await;
    result.unwrap_or_else(|_| Err(anyhow!("IMAP: warmup timed out")))
}

fn uid_set(uids: &[u32]) -> String {
    uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

async fn add_flag(session: &mut ImapSession, uids: &[u32], flag: &str) -> anyhow::Result<()> {
    if !uids.is_empty() {
        let _: Vec<_> = session.uid_store(uid_set(uids), format!("+FLAGS.SILENT ({flag})")).await?.try_collect().await?;
    }
    Ok(())
}

/// MOVE where the server has it. Otherwise COPY, then delete and expunge just those UIDs, which needs UIDPLUS: a plain
/// EXPUNGE would also remove whatever else the user marked deleted. Without either, they stay where they are.
async fn move_uids(session: &mut ImapSession, uids: &[u32], to: &str) -> anyhow::Result<()> {
    if uids.is_empty() {
        return Ok(());
    }
    let caps = session.capabilities().await?;
    let set = uid_set(uids);
    if caps.has_str("MOVE") {
        session.uid_mv(&set, to).await?;
    } else if caps.has_str("UIDPLUS") {
        session.uid_copy(&set, to).await?;
        add_flag(session, uids, "\\Deleted").await?;
        let _: Vec<_> = session.uid_expunge(&set).await?.try_collect().await?;
    }
    Ok(())
}

#[derive(Debug)]
struct Folder {
    name: String,
    junk: bool,
    archive: bool,
    all: bool,
}

async fn folders(session: &mut ImapSession) -> anyhow::Result<Vec<Folder>> {
    use async_imap::types::NameAttribute;
    let names: Vec<_> = session.list(Some(""), Some("*")).await?.try_collect().await?;
    Ok(names
        .iter()
        .map(|n| {
            let has = |a: NameAttribute| n.attributes().contains(&a);
            Folder { name: n.name().to_string(), junk: has(NameAttribute::Junk), archive: has(NameAttribute::Archive), all: has(NameAttribute::All) }
        })
        .collect())
}

/// SPECIAL-USE \Junk, else a folder with one of the usual names, alone or under INBOX like Courier's "INBOX.Spam".
fn junk_folder(folders: &[Folder]) -> Option<String> {
    const NAMES: [&str; 6] = ["spam", "junk", "[gmail]/spam", "[google mail]/spam", "junk email", "junk e-mail"];
    let named = |f: &&Folder| {
        let lower = f.name.to_lowercase();
        let last = lower.strip_prefix("inbox.").or_else(|| lower.strip_prefix("inbox/")).unwrap_or(&lower);
        NAMES.contains(&last)
    };
    folders.iter().find(|f| f.junk).or_else(|| folders.iter().find(named)).map(|f| f.name.clone())
}

fn archive_folder(folders: &[Folder]) -> Option<String> {
    let named = |f: &&Folder| f.name.eq_ignore_ascii_case("archive");
    folders.iter().find(|f| f.archive).or_else(|| folders.iter().find(|f| f.all)).or_else(|| folders.iter().find(named)).map(|f| f.name.clone())
}

/// The org's warming mailboxes over the last 14 days. See WarmupHealth.
pub async fn health(db: &sqlx::PgPool, org_id: Uuid) -> sqlx::Result<Vec<WarmupHealth>> {
    let mut rows = sqlx::query_as::<_, WarmupHealth>(&format!(
        "select m.id as mailbox_id, current_date - m.warmup_started_on + 1 as day, m.warmup_daily_cap as cap,
                ({WARMUP_SENT_TODAY}) as sent_today,
                (select count(*) from warmup_emails x where x.from_mailbox_id = m.id and x.status = 'sent' and x.sent_at > now() - interval '14 days') as sent,
                (select count(*) from warmup_emails x where x.to_mailbox_id = m.id and x.landed_at > now() - interval '14 days') as received,
                (select count(*) from warmup_emails x where x.from_mailbox_id = m.id and x.landed = 'inbox' and x.sent_at > now() - interval '14 days') as inbox,
                (select count(*) from warmup_emails x where x.from_mailbox_id = m.id and x.landed = 'spam' and x.sent_at > now() - interval '14 days') as spam,
                exists (select 1 from mailboxes o where o.id <> m.id and {}) as has_peers
         from mailboxes m where m.org_id = $1 and m.warmup order by m.created_at",
        POOL.replace("m.", "o."),
    ))
    .bind(org_id)
    .fetch_all(db)
    .await?;
    for h in &mut rows {
        h.target_today = daily_target(h.day.map_or(0, |d| i64::from(d) - 1), h.cap);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn ramps_from_2_by_2_a_day_up_to_the_cap() {
        let ramp: Vec<i32> = (0..25).map(|d| daily_target(d, 40)).collect();
        assert_eq!(&ramp[..4], &[2, 4, 6, 8]);
        assert_eq!(ramp[18], 38);
        assert!(ramp[19..].iter().all(|&t| t == 40), "{ramp:?}");
        assert_eq!(daily_target(3, 5), 5);
        assert_eq!(daily_target(-1, 40), 2, "a clock running behind doesn't drop below the start");
    }

    #[test]
    fn spreads_the_day_with_jitter() {
        // 8 hours left for 4 emails is 2 hours apart, give or take half.
        assert_eq!(gap_secs(10 * 3600 + 300, 4, 50), 2 * 3600);
        assert_eq!(gap_secs(10 * 3600 + 300, 4, 0), 3600);
        assert_eq!(gap_secs(10 * 3600 + 300, 4, 100), 3 * 3600);
        // Every email of the day fits in an 8-hour window even when every gap is as long as it can be and each send waits
        // for the next minute's tick: from day 1's 2 up to the 40 a day cap.
        for target in [2, 12, 40] {
            let window = 8 * 3600;
            let mut at = 0;
            for left in (1..=target).rev() {
                at += gap_secs(window - at, left, 100);
                at = (at + 59) / 60 * 60;
                assert!(at < window, "email {} of {target} at {at}s", target - left + 1);
            }
        }
        assert_eq!(gap_secs(30, 0, 50), 60, "never faster than a minute");
    }

    #[test]
    fn the_total_counts_campaign_and_warmup_emails() {
        assert!(has_room(0, 5, 6, 80));
        assert!(!has_room(0, 6, 6, 80), "the ramp is full");
        assert!(!has_room(75, 5, 40, 80), "campaigns used the rest of the total");
        assert!(has_room(74, 5, 40, 80));
    }

    #[test]
    fn finds_the_token_in_the_header_or_the_body() {
        assert_eq!(find_token(Some(" K4M9QX2D "), ""), Some("K4M9QX2D".into()));
        // The header was stripped, so the code in the last line counts.
        assert_eq!(find_token(None, "Hi Sam,\n\nThanks,\nJo\n\nSW-K4M9QX2D"), Some("K4M9QX2D".into()));
        assert_eq!(find_token(Some("garbage"), "Thanks\n\nSW-K4M9QX2D\n"), Some("K4M9QX2D".into()));
        // Lookalikes: a longer code, one glued to a word, letters outside the alphabet (O, 1) and too short.
        for body in ["SW-K4M9QX2DZ", "NEWSW-K4M9QX2D", "SW-K4M9QX2O", "SW-K4M91X2D", "SW-K4M9", "Order no. 8812"] {
            assert_eq!(find_token(None, body), None, "{body}");
        }
    }

    #[test]
    fn reads_the_token_from_a_raw_email() {
        let with_header = "From: a@alpha.test\r\nX-Splee-Warmup: K4M9QX2D\r\nSubject: Hi\r\n\r\nHello";
        let header_stripped = "From: a@alpha.test\r\nSubject: Hi\r\n\r\nHello\r\n\r\nSW-ZZ23AB45\r\n";
        let plain = "From: a@alpha.test\r\nSubject: Hi\r\n\r\nInterested, call me.";
        assert_eq!(token_in(with_header.as_bytes()), Some("K4M9QX2D".into()));
        assert_eq!(token_in(header_stripped.as_bytes()), Some("ZZ23AB45".into()));
        assert_eq!(token_in(plain.as_bytes()), None);
    }

    #[test]
    fn templates_vary_and_fill_every_slot() {
        let mut pick = roll;
        let drafts: Vec<Draft> = (0..60).map(|_| compose(&mut pick, "Sam", "Jo", &new_token(&mut roll))).collect();
        let subjects: HashSet<&str> = drafts.iter().map(|d| d.subject.as_str()).collect();
        let bodies: HashSet<&str> = drafts.iter().map(|d| d.body.as_str()).collect();
        assert!(subjects.len() >= 20, "only {} different subjects", subjects.len());
        assert_eq!(bodies.len(), 60);
        for d in &drafts {
            assert!(!d.subject.contains('{') && !d.body.contains('{'), "{d:?}");
            assert!(d.body.contains("Sam") && d.body.contains("\nJo\n"), "{d:?}");
            assert!(d.subject.chars().next().unwrap().is_uppercase(), "{d:?}");
            assert!(find_token(None, &d.body).is_some(), "{d:?}");
        }
        // Every template, greeting and reply gets used and filled.
        let mut i = 0;
        let mut each = |n: usize| {
            i += 1;
            i % n
        };
        for _ in 0..FRESH.len() * 3 {
            let reply = compose_reply(&mut each, "Re: Agenda", "Sam", "Jo", "K4M9QX2D");
            assert_eq!(reply.subject, "Re: Agenda");
            assert!(!reply.body.contains('{'), "{reply:?}");
        }
        assert_eq!(compose_reply(&mut each, "Agenda", "Sam", "Jo", "K4M9QX2D").subject, "Re: Agenda");
    }

    #[test]
    fn finds_the_spam_and_archive_folders() {
        let f = |name: &str, junk, archive, all| Folder { name: name.into(), junk, archive, all };
        let gmail = [f("INBOX", false, false, false), f("[Gmail]/All Mail", false, false, true), f("[Gmail]/Spam", true, false, false)];
        assert_eq!(junk_folder(&gmail).as_deref(), Some("[Gmail]/Spam"));
        assert_eq!(archive_folder(&gmail).as_deref(), Some("[Gmail]/All Mail"));
        // No SPECIAL-USE: the usual names, alone or under INBOX.
        let plain = [f("INBOX", false, false, false), f("Junk E-mail", false, false, false), f("archive", false, false, false)];
        assert_eq!(junk_folder(&plain).as_deref(), Some("Junk E-mail"));
        assert_eq!(archive_folder(&plain).as_deref(), Some("archive"));
        assert_eq!(junk_folder(&[f("INBOX.Spam", false, false, false)]).as_deref(), Some("INBOX.Spam"));
        assert_eq!(junk_folder(&[f("INBOX", false, false, false), f("Spam reports", false, false, false)]), None);
        assert_eq!(archive_folder(&[f("INBOX", false, false, false)]), None);
    }

    #[test]
    fn names_people_like_a_colleague_would() {
        assert_eq!(named("Alex Morgan", "alex@try-acme.com"), "Alex");
        assert_eq!(named("", "sam.lee@try-acme.com"), "Sam");
    }
}
