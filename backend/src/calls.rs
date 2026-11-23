//! Calls to leads. A follow-up step of kind "call" becomes an open call for the lead's owner when it comes due, and the
//! sequence waits for someone to log what happened or skip it (LAST_STEP). Calls are made from the browser through the
//! organization's Twilio account: Splee mints a short-lived Access Token, the Voice SDK calls the TwiML App, and Twilio asks
//! /api/twilio/voice what to do, which dials the lead only after the checks in `authorize`. Twilio's callbacks then bring
//! the length, the recording and, with transcription on, the transcript the agent summarizes.
//!
//! A lead calling one of the organization's numbers set to the TwiML App reaches /api/twilio/voice too, and rings the browsers
//! of whoever should take it (`incoming`). A call nobody takes asks them for a message and puts a call back at the top of the
//! Calls page (`on_dialed`).

use chrono::Timelike;
use chrono_tz::Tz;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::agent::{self, kind};
use crate::mail::{COMPANY_REPLIED, FOLLOW_UPS};
use crate::models::{Call, Lead, Ts};
use crate::phone;
use crate::state::AppState;

/// Calls go to a lead between 8:00 and 20:00 their time.
pub const FIRST_HOUR: u32 = 8;
pub const LAST_HOUR: u32 = 20;
pub const OUTCOMES: &[&str] = &["connected", "voicemail", "no_answer", "wrong_number", "not_interested", "meeting_booked"];
/// The outcomes where someone picked up and talked, which make the connect rate.
pub const CONNECTED: [&str; 3] = ["connected", "not_interested", "meeting_booked"];
/// How long a browser's token to place calls lasts. The page asks for a new one for each call.
pub const TOKEN_SECS: i64 = 600;
/// How long the team's browsers ring for a lead's call before they're asked for a message.
pub const RING_SECS: u32 = 20;
/// Twilio asks dialed_url once the browsers stop ringing, but not when the caller hung up first, so a call still ringing this
/// long after it came in was given up on, and tick counts it missed.
const ABANDONED_SECS: i32 = 120;
/// Twilio rings at most 10 browsers at once.
const MAX_RINGING: i64 = 10;

/// The lead's last finished step of the email sequence, as `last` (step, at): the last email sent, or a call step done or
/// skipped after it. Joined laterally on a lead aliased `l`, `join {LAST_STEP} on true`.
pub const LAST_STEP: &str = "lateral (select x.step, x.at from (
       select e.step, e.sent_at as at from email_messages e
       where e.lead_id = l.id and e.direction = 'outbound' and e.kind in ('initial', 'follow_up') and e.status = 'sent'
       union all
       select c.step, c.closed_at from calls c where c.lead_id = l.id and c.kind = 'sequence' and c.status in ('done', 'skipped')
     ) x order by x.step desc limit 1) last";

/// SQL that's true when the lead `l` of project `p` is on the organization's do-not-contact list, by number, email or
/// domain, or on one of its lists covering the project.
fn suppressed() -> String {
    let listed = crate::routes::suppression_lists::listed("p.org_id", "p.id", "l.email::text", "l.company_domain::text");
    format!(
        "(exists (select 1 from suppressions x where x.org_id = p.org_id
           and ((x.kind = 'phone' and x.value = l.phone) or x.value = l.email or x.value = split_part(l.email::text, '@', 2))) or {listed})"
    )
}

/// Takes a lead's open calls off. $1 is the lead, $2 the reason shown on them.
pub const STOP_CALLS: &str = "update calls set status = 'cancelled', cancel_reason = $2, closed_at = now() where lead_id = $1 and status = 'open'";
/// STOP_CALLS for the sequence's call steps only, which a reply ends. A call back after the lead called stays.
pub const STOP_SEQUENCE_CALLS: &str =
    "update calls set status = 'cancelled', cancel_reason = $2, closed_at = now() where lead_id = $1 and status = 'open' and kind = 'sequence'";

pub fn voice_url(public_url: &str) -> String {
    format!("{}/api/twilio/voice", public_url.trim_end_matches('/'))
}

pub fn status_url(public_url: &str) -> String {
    format!("{}/api/twilio/calls/status", public_url.trim_end_matches('/'))
}

pub fn recording_url(public_url: &str) -> String {
    format!("{}/api/twilio/calls/recording", public_url.trim_end_matches('/'))
}

pub fn transcription_url(public_url: &str) -> String {
    format!("{}/api/twilio/calls/transcription", public_url.trim_end_matches('/'))
}

/// Twilio's callbacks when a team member's browser answers a lead's call, and when that browser's side of it ends.
pub fn client_url(public_url: &str) -> String {
    format!("{}/api/twilio/calls/client", public_url.trim_end_matches('/'))
}

/// What Twilio asks once the browsers stop ringing for a lead's call, answered or not, unless the lead hung up first.
pub fn dialed_url(public_url: &str) -> String {
    format!("{}/api/twilio/calls/dialed", public_url.trim_end_matches('/'))
}

/// What Twilio asks once a lead nobody answered is done leaving a message.
pub fn message_url(public_url: &str) -> String {
    format!("{}/api/twilio/calls/message", public_url.trim_end_matches('/'))
}

/// The time zone a lead is called in: theirs, from their number or country, else the project's.
pub fn zone(lead_phone: &str, lead_country: &str, project_tz: &str) -> Tz {
    phone::time_zone(lead_phone, lead_country).unwrap_or_else(|| project_tz.parse().unwrap_or(Tz::UTC))
}

/// Why now is a bad time to call someone in `tz`, or None inside their calling hours.
pub fn outside_hours(tz: Tz, now: Ts) -> Option<String> {
    let local = now.with_timezone(&tz);
    if (FIRST_HOUR..LAST_HOUR).contains(&local.hour()) {
        return None;
    }
    Some(format!("It's {} where they are. Call between {FIRST_HOUR}:00 and {LAST_HOUR}:00 their time.", local.format("%H:%M")))
}

/// The Voice SDK's identity for a team member. Twilio allows letters, digits and underscores, so no dashes.
pub fn identity(user_id: Uuid) -> String {
    format!("u{}", user_id.simple())
}

/// The team member a TwiML request's From ("client:u…") names.
pub fn caller(from: &str) -> Option<Uuid> {
    from.strip_prefix("client:")?.strip_prefix('u')?.parse().ok()
}

/// A Twilio Access Token with a Voice grant that lets the browser place calls through the TwiML App, and with `incoming`, ring
/// for calls leads make to the organization's numbers. It's a JWT signed with the API key's secret, as Twilio's helper
/// libraries make it.
pub fn access_token(account_sid: &str, key_sid: &str, key_secret: &str, app_sid: &str, identity: &str, incoming: bool, now: i64) -> String {
    let header = json!({ "alg": "HS256", "typ": "JWT", "cty": "twilio-fpa;v=1" });
    let claims = json!({
        "jti": format!("{key_sid}-{now}"),
        "iss": key_sid,
        "sub": account_sid,
        "iat": now,
        "nbf": now,
        "exp": now + TOKEN_SECS,
        "grants": { "identity": identity, "voice": { "incoming": { "allow": incoming }, "outgoing": { "application_sid": app_sid } } },
    });
    crate::crypto::hs256_jwt(key_secret.as_bytes(), &header, &claims)
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// What Twilio says to the caller instead of dialing, then it hangs up.
pub fn twiml_refuse(reason: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response><Say>{}</Say><Hangup/></Response>", xml(reason))
}

/// Plays the audio at `url`, then hangs up: a recorded voicemail left on the lead's side of a call.
pub fn twiml_play(url: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response><Play>{}</Play><Hangup/></Response>", xml(url))
}

/// Has Twilio transcribe the call, both sides, with who's on the inbound track (the audio coming into Twilio) and who on the
/// outbound one. On a call the team makes the inbound track is the browser, and on one the lead makes it's the lead.
fn twiml_transcribe(public_url: &str, inbound: &str, outbound: &str) -> String {
    format!(
        "<Start><Transcription statusCallbackUrl=\"{}\" track=\"both_tracks\" inboundTrackLabel=\"{inbound}\" outboundTrackLabel=\"{outbound}\"/></Start>",
        xml(&transcription_url(public_url))
    )
}

/// Dials the lead from the organization's number, recording both sides from when they answer, and with transcription on,
/// transcribing it too.
pub fn twiml_dial(public_url: &str, from: &str, to: &str, transcribe: bool) -> String {
    let start = if transcribe { twiml_transcribe(public_url, "Us", "Lead") } else { String::new() };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response>{start}<Dial callerId=\"{}\" answerOnBridge=\"true\" timeLimit=\"3600\" \
         record=\"record-from-answer-dual\" recordingStatusCallback=\"{}\" recordingStatusCallbackEvent=\"completed\">\
         <Number statusCallback=\"{}\" statusCallbackEvent=\"completed\">{}</Number></Dial></Response>",
        xml(from),
        xml(&recording_url(public_url)),
        xml(&status_url(public_url)),
        xml(to),
    )
}

/// Rings the browsers of `members` for a call a lead made to one of the organization's numbers, with `caller` for the browser
/// to show who it is. Whoever answers first takes it, and it's recorded from then. With transcription on the whole call is
/// transcribed, a message they leave included. After RING_SECS, or once it ends, Twilio asks dialed_url what next. Each
/// browser's answered and completed callbacks go to client_url, since a lead hanging up first ends it without dialed_url.
pub fn twiml_ring(public_url: &str, members: &[Uuid], caller: &[(&str, &str)], transcribe: bool) -> String {
    let start = if transcribe { twiml_transcribe(public_url, "Lead", "Us") } else { String::new() };
    let params: String = caller.iter().map(|(name, value)| format!("<Parameter name=\"{}\" value=\"{}\"/>", xml(name), xml(value))).collect();
    let clients: String = members
        .iter()
        .map(|member| {
            format!(
                "<Client statusCallback=\"{}\" statusCallbackEvent=\"answered completed\"><Identity>{}</Identity>{params}</Client>",
                xml(&client_url(public_url)),
                identity(*member)
            )
        })
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response>{start}<Dial timeout=\"{RING_SECS}\" answerOnBridge=\"true\" timeLimit=\"3600\" \
         action=\"{}\" record=\"record-from-answer-dual\" recordingStatusCallback=\"{}\" recordingStatusCallbackEvent=\"completed\">{clients}</Dial></Response>",
        xml(&dialed_url(public_url)),
        xml(&recording_url(public_url)),
    )
}

/// Asks a lead nobody answered for a message of up to two minutes, which the recording callback keeps on their call. It doesn't
/// name the organization, which is often still named after whoever signed up.
pub fn twiml_message(public_url: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response><Say>Sorry we missed your call. Leave a message after the tone and we'll call you \
         back.</Say><Record maxLength=\"120\" timeout=\"5\" playBeep=\"true\" action=\"{}\" recordingStatusCallback=\"{}\" \
         recordingStatusCallbackEvent=\"completed\"/></Response>",
        xml(&message_url(public_url)),
        xml(&recording_url(public_url)),
    )
}

pub const TWIML_HANGUP: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response><Hangup/></Response>";

/// A call the checks let through, with what Twilio needs to place it.
#[derive(Debug)]
pub struct Dial {
    pub call: Call,
    pub lead: Lead,
    pub from: String,
    pub transcribe: bool,
    pub account_sid: String,
    pub api_key_sid: String,
    pub api_secret_enc: String,
    pub twiml_app_sid: String,
}

/// Checks that `user_id` may call the lead of call `call_id` now, from any phone: they're on the lead's organization, the call
/// is open, `to` (when given) is the lead's number, the lead isn't on the do-not-contact list and it's within their calling
/// hours. Err is the reason, in words for the caller.
pub async fn check(db: &PgPool, org_id: Uuid, user_id: Uuid, call_id: Uuid, to: Option<&str>, now: Ts) -> sqlx::Result<Result<(Call, Lead), String>> {
    let call = sqlx::query_as::<_, Call>("select c.* from calls c join projects p on p.id = c.project_id where c.id = $1 and p.org_id = $2")
        .bind(call_id)
        .bind(org_id)
        .fetch_optional(db)
        .await?;
    let Some(call) = call else { return Ok(Err("This call isn't in your organization.".into())) };
    let member: bool = sqlx::query_scalar("select exists (select 1 from memberships where org_id = $1 and user_id = $2)")
        .bind(org_id)
        .bind(user_id)
        .fetch_one(db)
        .await?;
    if !member {
        return Ok(Err("Only someone on the lead's team can call them.".into()));
    }
    if call.status != "open" {
        return Ok(Err("This call was already logged, skipped or cancelled.".into()));
    }
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(call.lead_id).fetch_one(db).await?;
    let blocked = suppressed();
    let (suppressed, project_tz): (bool, String) = sqlx::query_as(&format!(
        "select {blocked} or l.status in ('unsubscribed', 'do_not_contact'), coalesce(ps.timezone, 'UTC')
         from leads l join projects p on p.id = l.project_id left join project_settings ps on ps.project_id = l.project_id where l.id = $1"
    ))
    .bind(lead.id)
    .fetch_one(db)
    .await?;
    if lead.phone.is_empty() {
        return Ok(Err("This lead has no phone number.".into()));
    }
    if to.is_some_and(|to| phone::normalize(to, "").as_deref() != Some(lead.phone.as_str())) {
        return Ok(Err("That number isn't this lead's.".into()));
    }
    if suppressed {
        return Ok(Err("They're on the do-not-contact list.".into()));
    }
    if let Some(why) = outside_hours(zone(&lead.phone, &lead.country_code, &project_tz), now) {
        return Ok(Err(why));
    }
    Ok(Ok((call, lead)))
}

/// What authorize reads of the organization's Twilio account.
#[derive(sqlx::FromRow)]
struct Calling {
    account_sid: String,
    api_key_sid: String,
    api_secret_enc: String,
    twiml_app_sid: String,
    voice_from: String,
    sms_from: String,
    call_transcription: bool,
    local_numbers: Vec<String>,
}

/// The number to call `to` from: with local presence on, the first of the default and the account's `local` numbers that's in
/// their area code, for the US, Canada and the rest of +1, else in their country, else the default.
pub fn caller_id(to: &str, local: &[String], default: &str) -> String {
    let area = |n: &str| n.strip_prefix("+1").and_then(|rest| rest.get(..3)).map(str::to_string);
    let numbers: Vec<&str> = std::iter::once(default).chain(local.iter().map(String::as_str)).collect();
    let same_area = area(to).and_then(|code| numbers.iter().find(|n| area(n).as_ref() == Some(&code)));
    let country = |n: &str| phone::nanp_country(n).or_else(|| phone::country(n));
    let same_country = country(to).and_then(|c| numbers.iter().find(|n| country(n) == Some(c)));
    same_area.or(same_country).copied().unwrap_or(default).to_string()
}

/// `check`, and that the organization's Twilio is set up for calls, with what Twilio needs to place the call.
pub async fn authorize(db: &PgPool, org_id: Uuid, user_id: Uuid, call_id: Uuid, to: Option<&str>, now: Ts) -> sqlx::Result<Result<Dial, String>> {
    let (call, lead) = match check(db, org_id, user_id, call_id, to, now).await? {
        Ok(checked) => checked,
        Err(why) => return Ok(Err(why)),
    };
    let account: Option<Calling> = sqlx::query_as(
        "select account_sid, api_key_sid, api_secret_enc, twiml_app_sid, voice_from, sms_from, call_transcription, local_numbers
         from twilio_accounts where org_id = $1",
    )
    .bind(org_id)
    .fetch_optional(db)
    .await?;
    let Some(a) = account else {
        return Ok(Err("Connect Twilio on the Integrations page to call from Splee.".into()));
    };
    let from = if a.voice_from.is_empty() && a.sms_from.starts_with('+') { a.sms_from } else { a.voice_from };
    if a.twiml_app_sid.is_empty() || from.is_empty() {
        return Ok(Err("Turn on calling in the Twilio card on the Integrations page first.".into()));
    }
    let from = caller_id(&lead.phone, &a.local_numbers, &from);
    let Calling { account_sid, api_key_sid, api_secret_enc, twiml_app_sid, call_transcription: transcribe, .. } = a;
    Ok(Ok(Dial { call, lead, from, transcribe, account_sid, api_key_sid, api_secret_enc, twiml_app_sid }))
}

/// Every minute with the sender: call steps that came due become open calls, open calls of leads who replied, booked or went
/// on the do-not-contact list are cancelled, and calls leads gave up on while they rang are missed.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    schedule(&state.db).await?;
    cancel_stale(&state.db, None).await?;
    let abandoned: Vec<(Uuid, String)> = sqlx::query_as(
        "select p.org_id, c.twilio_sid from calls c join projects p on p.id = c.project_id
         where c.inbound and c.call_status = 'ringing' and c.called_at < now() - make_interval(secs => $1) and p.org_id is not null
         limit 100",
    )
    .bind(ABANDONED_SECS)
    .fetch_all(&state.db)
    .await?;
    for (org_id, sid) in abandoned {
        after_ringing(state, org_id, &sid, "canceled", None).await?;
    }
    Ok(())
}

/// Opens the call step of each lead whose step is due now, in its campaign's follow-ups or else the project's, for the lead's
/// owner. A lead with no number, or whose number is on the do-not-contact list, gets it skipped instead, so their sequence goes
/// on. Campaigns brought over from another tool keep their own steps, which are all emails.
pub async fn schedule(db: &PgPool) -> sqlx::Result<u64> {
    let done = sqlx::query(&format!(
        "with due as (
           select l.id as lead_id, l.project_id, l.segment_id, l.owner_id, l.phone, last.step + 1 as step,
                  coalesce({FOLLOW_UPS} -> last.step ->> 'instructions', '') as instructions,
                  last.at + make_interval(days => greatest(1, coalesce(({FOLLOW_UPS} -> last.step ->> 'delay_days')::int, 3))) as due_at,
                  case when l.phone = '' then 'No phone number'
                       when exists (select 1 from suppressions x where x.org_id = p.org_id and x.kind = 'phone' and x.value = l.phone)
                         then 'Their number is on the do-not-contact list' end as why
           from leads l
           join projects p on p.id = l.project_id
           join project_settings ps on ps.project_id = l.project_id
           left join segments s on s.id = l.segment_id
           join {LAST_STEP} on true
           where p.org_id is not null and not ps.paused and l.status = 'contacted' and coalesce(s.status, 'working') <> 'paused'
             and not exists (select 1 from segment_steps x where x.segment_id = l.segment_id)
             and jsonb_typeof({FOLLOW_UPS}) = 'array' and jsonb_array_length({FOLLOW_UPS}) > last.step
             and {FOLLOW_UPS} -> last.step ->> 'kind' = 'call'
             and coalesce(({FOLLOW_UPS} -> last.step ->> 'enabled')::boolean, false)
             and not exists (select 1 from email_messages i where i.lead_id = l.id and i.direction = 'inbound')
             and not {COMPANY_REPLIED}
         )
         insert into calls (project_id, lead_id, segment_id, kind, step, instructions, owner_id, status, due_at, to_number, closed_at, cancel_reason)
         select project_id, lead_id, segment_id, 'sequence', step, instructions, owner_id, case when why is null then 'open' else 'skipped' end,
                due_at, phone, case when why is not null then now() end, coalesce(why, '')
         from due where due_at <= now()
         on conflict (lead_id, step) where kind = 'sequence' do nothing"
    ))
    .execute(db)
    .await?;
    Ok(done.rows_affected())
}

/// Cancels the open calls of leads who replied, booked or went on the do-not-contact list, in `project` or everywhere. The
/// paths that change a lead do it at once too. This catches the rest, like an address added to the list by hand. A reply
/// only ends the sequence's call steps, since a lead who calls in has often replied before, and their call back stays.
pub async fn cancel_stale(db: &PgPool, project: Option<Uuid>) -> sqlx::Result<u64> {
    let blocked = suppressed();
    let done = sqlx::query(&format!(
        "update calls c set status = 'cancelled', closed_at = now(), cancel_reason = r.why
         from (select o.id, case
                 when {blocked} or l.status in ('unsubscribed', 'do_not_contact') then 'On the do-not-contact list'
                 when l.status = 'meeting_booked' then 'They booked a meeting'
                 when l.status = 'not_interested' then 'Marked not interested'
                 when o.kind = 'sequence' and (l.status in ('replied', 'hot')
                   or exists (select 1 from email_messages i where i.lead_id = l.id and i.direction = 'inbound')) then 'They replied'
               end as why
               from calls o join leads l on l.id = o.lead_id join projects p on p.id = o.project_id
               where o.status = 'open' and ($1::uuid is null or o.project_id = $1)) r
         where c.id = r.id and r.why is not null and c.status = 'open'"
    ))
    .bind(project)
    .execute(db)
    .await?;
    Ok(done.rows_affected())
}

/// Ends the lead's sequence as a reply does, then cancels its other open calls too, like a call back they asked for.
async fn stop_sequence(db: &PgPool, lead_id: Uuid, why: &str) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    crate::mail::end_sequence(&mut tx, lead_id, why).await?;
    sqlx::query(STOP_CALLS).bind(lead_id).bind(why).execute(&mut *tx).await?;
    tx.commit().await
}

/// Logs what happened on an open call and closes it, which lets the sequence go on, then lets the outcome change the lead:
/// a booked meeting goes through the usual booked flow, not interested stops the sequence, and a wrong number is cleared
/// along with the texts and calls still due to it. None when the call isn't open any more.
pub async fn log(state: &AppState, call_id: Uuid, outcome: &str, notes: &str, user: Option<Uuid>) -> anyhow::Result<Option<Call>> {
    let call = sqlx::query_as::<_, Call>(
        "update calls set status = 'done', outcome = $2, notes = $3, closed_at = now(), called_by = coalesce(called_by, $4),
                          called_at = coalesce(called_at, now())
         where id = $1 and status = 'open' returning *",
    )
    .bind(call_id)
    .bind(outcome)
    .bind(notes)
    .bind(user)
    .fetch_optional(&state.db)
    .await?;
    if let Some(call) = &call {
        apply_outcome(state, call.lead_id, outcome).await?;
    }
    Ok(call)
}

/// A call made outside a call step, like one logged from the API for a lead with no open call.
pub async fn log_manual(state: &AppState, lead: &Lead, outcome: &str, notes: &str, user: Option<Uuid>) -> anyhow::Result<Call> {
    let call = sqlx::query_as::<_, Call>(
        "insert into calls (project_id, lead_id, segment_id, kind, owner_id, status, to_number, called_by, called_at, closed_at, outcome, notes)
         values ($1, $2, $3, 'manual', $4, 'done', $5, $6, now(), now(), $7, $8) returning *",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(lead.segment_id)
    .bind(lead.owner_id)
    .bind(&lead.phone)
    .bind(user)
    .bind(outcome)
    .bind(notes)
    .fetch_one(&state.db)
    .await?;
    apply_outcome(state, lead.id, outcome).await?;
    Ok(call)
}

async fn apply_outcome(state: &AppState, lead_id: Uuid, outcome: &str) -> anyhow::Result<()> {
    let db = &state.db;
    let before = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(lead_id).fetch_one(db).await?;
    if matches!(outcome, "connected" | "voicemail" | "not_interested" | "meeting_booked") {
        sqlx::query("update leads set last_contacted_at = now() where id = $1").bind(lead_id).execute(db).await?;
    }
    let status = match outcome {
        "meeting_booked" => "meeting_booked",
        "not_interested" => "not_interested",
        "wrong_number" => {
            sqlx::query("update leads set phone = '' where id = $1").bind(lead_id).execute(db).await?;
            sqlx::query(crate::twilio::STOP_TEXTS).bind(lead_id).bind("Wrong number").execute(db).await?;
            sqlx::query(STOP_CALLS).bind(lead_id).bind("Wrong number").execute(db).await?;
            return Ok(());
        }
        _ => return Ok(()),
    };
    let why = if status == "meeting_booked" { "They booked a meeting on a call" } else { "Not interested on a call" };
    stop_sequence(db, lead_id, why).await?;
    let lead = sqlx::query_as::<_, Lead>(
        "update leads set status = $2 where id = $1 and status not in ('meeting_booked', 'unsubscribed', 'bounced', 'do_not_contact') returning *",
    )
    .bind(lead_id)
    .bind(status)
    .fetch_optional(db)
    .await?;
    if let Some(lead) = lead {
        crate::hooks::team_set_status(state, &before.status, &lead).await;
    }
    Ok(())
}

/// Twilio's word for how the lead's leg ended, and how long it lasted, from the <Number> status callback.
pub async fn on_status(db: &PgPool, org_id: Uuid, parent_sid: &str, status: &str, duration: Option<i32>) -> sqlx::Result<()> {
    sqlx::query(
        "update calls c set call_status = $3, duration_secs = coalesce($4, c.duration_secs) from projects p
         where p.id = c.project_id and p.org_id = $1 and c.twilio_sid = $2",
    )
    .bind(org_id)
    .bind(parent_sid)
    .bind(status)
    .bind(duration)
    .execute(db)
    .await?;
    Ok(())
}

/// A finished recording of the call. Only its SID and Twilio's URL are kept, and the app plays it through Splee.
pub async fn on_recording(db: &PgPool, org_id: Uuid, call_sid: &str, recording_sid: &str, url: &str, secs: Option<i32>) -> sqlx::Result<()> {
    sqlx::query(
        "update calls c set recording_sid = $3, recording_url = $4, recording_secs = $5 from projects p
         where p.id = c.project_id and p.org_id = $1 and c.twilio_sid = $2",
    )
    .bind(org_id)
    .bind(call_sid)
    .bind(recording_sid)
    .bind(url)
    .bind(secs)
    .execute(db)
    .await?;
    Ok(())
}

/// One of Twilio's real-time transcription callbacks. Final segments are kept as they come, and once the transcription
/// stops they're put in order and the agent is asked for a summary. A transcription that stops twice is summarized once.
pub async fn on_transcription(db: &PgPool, org_id: Uuid, call_sid: &str, event: &str, params: &[(String, String)]) -> sqlx::Result<()> {
    let param = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or_default();
    match event {
        "transcription-content" if param("Final") == "true" => {
            let data: Value = serde_json::from_str(param("TranscriptionData")).unwrap_or_default();
            let text = data["transcript"].as_str().unwrap_or_default().trim();
            if text.is_empty() {
                return Ok(());
            }
            // The track's label, as twiml_transcribe gave it, or else which way the audio went, which is the other way round on
            // a call the lead made.
            sqlx::query(
                "update calls c set transcript_parts = c.transcript_parts || jsonb_build_array(jsonb_build_object(
                   'seq', $3::bigint, 'text', $5::text,
                   'who', case when $4 in ('Us', 'Lead') then $4 when ($4 = 'inbound_track') <> c.inbound then 'Us' else 'Lead' end))
                 from projects p
                 where p.id = c.project_id and p.org_id = $1 and c.twilio_sid = $2 and c.transcribed_at is null",
            )
            .bind(org_id)
            .bind(call_sid)
            .bind(param("SequenceId").parse::<i64>().unwrap_or_default())
            .bind(param("Track"))
            .bind(text)
            .execute(db)
            .await?;
        }
        "transcription-stopped" => {
            let done: Option<(Uuid, Uuid, bool)> = sqlx::query_as(
                "update calls c set transcribed_at = now(), transcript = coalesce((
                   select string_agg((part ->> 'who') || ': ' || (part ->> 'text'), E'\\n' order by (part ->> 'seq')::bigint)
                   from jsonb_array_elements(c.transcript_parts) part), '')
                 from projects p
                 where p.id = c.project_id and p.org_id = $1 and c.twilio_sid = $2 and c.transcribed_at is null
                 returning c.id, c.project_id, c.transcript <> ''",
            )
            .bind(org_id)
            .bind(call_sid)
            .fetch_optional(db)
            .await?;
            if let Some((id, project_id, _)) = done.filter(|(.., said)| *said) {
                agent::enqueue(db, kind::SUMMARIZE_CALL, Some(project_id), Some(org_id), json!({ "call_id": id })).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

// Calls leads make

/// The notes a lead's call is kept with, the same as for one taken in Aircall or Dialpad (cloud_phone.rs).
const CALLED_IN: &str = "They called in.";
const NOBODY_ANSWERED: &str = "They called and nobody answered.";
const LEFT_MESSAGE: &str = "They called and left a voicemail.";
const CALL_BACK: &str = "They called and nobody answered. Call them back.";

/// A lead's call to one of the organization's numbers, and whose browsers ring for it.
pub struct Incoming {
    /// The lead with that number most recently contacted in any of the organization's projects, or None for a number no lead
    /// has.
    pub lead: Option<Lead>,
    /// The lead's owner while they're on the team and not away, as push::lead_recipients has it, or else everyone on the team
    /// who isn't away, up to the 10 Twilio rings at once.
    pub members: Vec<Uuid>,
    pub transcribe: bool,
}

/// Finds who's calling and whose browsers ring, and keeps the call on the lead as not answered until someone does. Twilio
/// asking again for the same CallSid keeps it once.
pub async fn incoming(db: &PgPool, org_id: Uuid, call_sid: &str, from: &str) -> sqlx::Result<Incoming> {
    let lead: Option<Lead> = match phone::normalize(from, "") {
        Some(number) => {
            sqlx::query_as(
                "select l.* from leads l join projects p on p.id = l.project_id where p.org_id = $1 and l.phone = $2
                 order by l.last_contacted_at desc nulls last, l.created_at desc limit 1",
            )
            .bind(org_id)
            .bind(number)
            .fetch_optional(db)
            .await?
        }
        None => None,
    };
    if let Some(lead) = &lead {
        sqlx::query(
            "insert into calls (project_id, lead_id, segment_id, kind, owner_id, status, to_number, called_at, closed_at, outcome, notes,
                                twilio_sid, call_status, inbound)
             values ($1, $2, $3, 'manual', $4, 'done', $5, now(), now(), 'no_answer', $6, $7, 'ringing', true)
             on conflict (twilio_sid) do nothing",
        )
        .bind(lead.project_id)
        .bind(lead.id)
        .bind(lead.segment_id)
        .bind(lead.owner_id)
        .bind(&lead.phone)
        .bind(NOBODY_ANSWERED)
        .bind(call_sid)
        .execute(db)
        .await?;
    }
    let members: Vec<Uuid> = sqlx::query_scalar(
        "select m.user_id from memberships m
         where m.org_id = $1 and not m.away
           and (m.user_id = $2 or not exists (select 1 from memberships o where o.org_id = $1 and o.user_id = $2 and not o.away))
         order by m.created_at limit $3",
    )
    .bind(org_id)
    .bind(lead.as_ref().and_then(|l| l.owner_id))
    .bind(MAX_RINGING)
    .fetch_all(db)
    .await?;
    let transcribe: Option<bool> = sqlx::query_scalar("select call_transcription from twilio_accounts where org_id = $1").bind(org_id).fetch_optional(db).await?;
    Ok(Incoming { lead, members, transcribe: transcribe.unwrap_or(false) })
}

/// A team member's browser answered a lead's call, from the <Client> status callback, whose To names them: it's theirs now.
/// The callback can come after dialed_url finished the call, which keeps its end.
pub async fn on_answered(db: &PgPool, org_id: Uuid, parent_sid: &str, to: &str) -> sqlx::Result<()> {
    let Some(user) = caller(to) else { return Ok(()) };
    sqlx::query(
        "update calls c set called_by = $3, outcome = 'connected', notes = $4,
                            call_status = case when c.call_status = 'ringing' then 'in-progress' else c.call_status end
         from projects p
         where p.id = c.project_id and p.org_id = $1 and c.twilio_sid = $2 and c.inbound and c.called_by is null
           and c.call_status in ('ringing', 'in-progress', 'completed')
           and exists (select 1 from memberships m where m.org_id = $1 and m.user_id = $3)",
    )
    .bind(org_id)
    .bind(parent_sid)
    .bind(user)
    .bind(CALLED_IN)
    .execute(db)
    .await?;
    Ok(())
}

/// What became of a lead's call once the browsers stopped ringing.
#[derive(Debug)]
pub enum Dialed {
    Answered,
    /// Nobody took it. `first` is false when Twilio asks again.
    Missed { lead: Box<Lead>, first: bool },
    /// A call from a number no lead has, which nothing was kept for.
    Unknown,
}

/// Keeps how a lead's call went once the browsers stop ringing, with Twilio's DialCallStatus. Answered, it's connected and the
/// lead was just contacted. Missed, the lead gets a call back on the Calls page: one opens for their owner due now, or the one
/// already open moves up to now. It's a manual call apart from any call step, so a reply that ends the sequence leaves it.
/// They also get the organization's missed-call text (text_back). Done once, while the call is still ringing or on, so asking
/// again changes nothing.
pub async fn on_dialed(db: &PgPool, org_id: Uuid, call_sid: &str, status: &str, duration: Option<i32>) -> sqlx::Result<Dialed> {
    let answered = matches!(status, "completed" | "answered");
    let mut tx = db.begin().await?;
    let changed: Option<(Uuid, String)> = sqlx::query_as(
        "update calls c set call_status = $3, duration_secs = case when $4 then $5 end, outcome = case when $4 then 'connected' else c.outcome end,
                            notes = case when $4 then $6 else c.notes end
         from projects p
         where p.id = c.project_id and p.org_id = $1 and c.twilio_sid = $2 and c.inbound and c.call_status in ('ringing', 'in-progress')
         returning c.lead_id, c.to_number",
    )
    .bind(org_id)
    .bind(call_sid)
    .bind(status)
    .bind(answered)
    .bind(duration)
    .bind(CALLED_IN)
    .fetch_optional(&mut *tx)
    .await?;
    let call = match changed {
        Some(call) => Some((call, true)),
        None => sqlx::query_as::<_, (Uuid, String)>(
            "select c.lead_id, c.to_number from calls c join projects p on p.id = c.project_id where p.org_id = $1 and c.twilio_sid = $2 and c.inbound",
        )
        .bind(org_id)
        .bind(call_sid)
        .fetch_optional(&mut *tx)
        .await?
        .map(|call| (call, false)),
    };
    let Some(((lead_id, number), first)) = call else { return Ok(Dialed::Unknown) };
    if answered {
        if first {
            sqlx::query("update leads set last_contacted_at = now() where id = $1").bind(lead_id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        return Ok(Dialed::Answered);
    }
    if first {
        // Two of the lead's calls missed at once would each find no call back or text yet and add one. Waiting on the lead's
        // row here lets the second see what the first added, since each statement below reads what's committed when it starts.
        sqlx::query("select 1 from leads where id = $1 for update").bind(lead_id).execute(&mut *tx).await?;
        sqlx::query(
            "with moved as (update calls set due_at = least(due_at, now()) where lead_id = $1 and status = 'open' and kind = 'manual' returning id)
             insert into calls (project_id, lead_id, segment_id, kind, owner_id, status, due_at, to_number, instructions)
             select project_id, id, segment_id, 'manual', owner_id, 'open', now(), phone, $2 from leads
             where id = $1 and phone <> '' and not exists (select 1 from moved)",
        )
        .bind(lead_id)
        .bind(CALL_BACK)
        .execute(&mut *tx)
        .await?;
        text_back(&mut tx, lead_id, &number).await?;
    }
    let lead: Lead = sqlx::query_as("select * from leads where id = $1").bind(lead_id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Dialed::Missed { lead: Box::new(lead), first })
}

/// Queues the organization's missed-call text to `to`, the number a lead nobody answered called from, when it has one and an
/// SMS sender. The sender's tick sends it like a reply. A lead who opted out, a number on the do-not-contact list, or a lead
/// texted back in the last day gets none.
async fn text_back(conn: &mut sqlx::PgConnection, lead_id: Uuid, to: &str) -> sqlx::Result<()> {
    if to.is_empty() {
        return Ok(());
    }
    let found: Option<(String, String, String, bool)> = sqlx::query_as(
        "select a.missed_call_text, coalesce(ps.sender_name, ''), coalesce(p.company ->> 'name', ''),
                not exists (select 1 from text_messages o join projects op on op.id = o.project_id
                            where op.org_id = p.org_id and o.to_number = $2 and o.status in ('sent', 'delivered'))
         from leads l
         join projects p on p.id = l.project_id
         join twilio_accounts a on a.org_id = p.org_id
         left join project_settings ps on ps.project_id = l.project_id
         where l.id = $1 and a.missed_call_text <> '' and a.sms_from <> '' and l.status not in ('unsubscribed', 'do_not_contact')
           and not exists (select 1 from suppressions x where x.org_id = p.org_id and x.kind = 'phone' and x.value = $2)
           and not exists (select 1 from text_messages t where t.lead_id = l.id and t.kind = 'missed_call' and t.created_at > now() - interval '1 day')",
    )
    .bind(lead_id)
    .bind(to)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((text, sender, company, first)) = found else { return Ok(()) };
    sqlx::query(
        "insert into text_messages (project_id, lead_id, segment_id, channel, kind, to_number, body, status, due_at)
         select project_id, id, segment_id, 'sms', 'missed_call', $2, $3, 'approved', now() from leads where id = $1",
    )
    .bind(lead_id)
    .bind(to)
    .bind(crate::twilio::finish_sms(&text, &sender, &company, to, first))
    .execute(conn)
    .await?;
    Ok(())
}

/// on_dialed, and a push to whoever hears about the lead the first time it says they missed the call.
pub async fn after_ringing(state: &AppState, org_id: Uuid, sid: &str, status: &str, duration: Option<i32>) -> sqlx::Result<Dialed> {
    let dialed = on_dialed(&state.db, org_id, sid, status, duration).await?;
    if let Dialed::Missed { lead, first: true } = &dialed {
        crate::push::spawn_lead_alert(state, lead, crate::push::Pref::HotLeads, crate::push::missed_call(&state.cfg.public_url, lead));
    }
    Ok(dialed)
}

/// A lead nobody answered is done leaving a message of `secs`: their call says they left one. The recording itself comes
/// with on_recording.
pub async fn on_message(db: &PgPool, org_id: Uuid, call_sid: &str, secs: i32) -> sqlx::Result<()> {
    if secs < 1 {
        return Ok(());
    }
    sqlx::query(
        "update calls c set notes = $3 from projects p
         where p.id = c.project_id and p.org_id = $1 and c.twilio_sid = $2 and c.inbound and c.outcome = 'no_answer'",
    )
    .bind(org_id)
    .bind(call_sid)
    .bind(LEFT_MESSAGE)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    use super::*;

    #[test]
    fn local_presence_calls_from_the_same_area_code_then_the_same_country() {
        let local: Vec<String> = ["+12125550100", "+13105550100", "+442071838750", "+61291234567"].map(String::from).to_vec();
        let from = |to: &str| caller_id(to, &local, "+14155550100");
        assert_eq!(from("+12125550134"), "+12125550100");
        assert_eq!(from("+13105550134"), "+13105550100");
        // The default when it's local too, and when no number is.
        assert_eq!(from("+14155550134"), "+14155550100");
        assert_eq!(from("+16175550134"), "+14155550100");
        assert_eq!(from("+447700900123"), "+442071838750");
        assert_eq!(from("+61412345678"), "+61291234567");
        assert_eq!(from("+4915112345678"), "+14155550100");
        // +1 is more than the US: Vancouver gets the Toronto number, and Jamaica none of the US ones.
        let local: Vec<String> = ["+12125550100", "+14165550100"].map(String::from).to_vec();
        assert_eq!(caller_id("+16045550134", &local, "+14155550100"), "+14165550100");
        assert_eq!(caller_id("+18765550134", &local, "+13105550100"), "+13105550100");
        assert_eq!(caller_id("+16175550134", &local, "+14165550100"), "+12125550100");
        // Off, there are no local numbers.
        assert_eq!(caller_id("+12125550134", &[], "+14155550100"), "+14155550100");
    }

    #[test]
    fn calling_hours_are_eight_to_eight_in_the_leads_zone() {
        let at = |s: &str| s.parse::<Ts>().unwrap();
        // 07:30 in London is too early, 08:00 is fine, 19:59 is fine and 20:00 is too late.
        let london = zone("+447700900123", "", "America/New_York");
        assert_eq!(london, Tz::Europe__London);
        assert_eq!(outside_hours(london, at("2026-11-12T07:30:00Z")).as_deref(), Some("It's 07:30 where they are. Call between 8:00 and 20:00 their time."));
        assert_eq!(outside_hours(london, at("2026-11-12T08:00:00Z")), None);
        assert_eq!(outside_hours(london, at("2026-11-12T19:59:00Z")), None);
        assert!(outside_hours(london, at("2026-11-12T20:00:00Z")).is_some());
        // A US number keeps several zones, so it's the project's. A bad project zone is UTC.
        let us = zone("+14155550134", "US", "America/Los_Angeles");
        assert_eq!(us, Tz::America__Los_Angeles);
        assert!(outside_hours(us, at("2026-11-12T15:00:00Z")).is_some(), "07:00 in Los Angeles");
        assert_eq!(outside_hours(us, at("2026-11-12T17:00:00Z")), None);
        assert_eq!(zone("", "", "Not/AZone"), Tz::UTC);
    }

    #[test]
    fn the_access_token_lets_one_member_place_calls_and_expires() {
        let user = Uuid::parse_str("0e5c3a8e-1f43-4a4e-9a51-3c1e0c9b7a10").unwrap();
        let token = access_token("AC1", "SK1", "secret", "AP1", &identity(user), false, 1_800_000_000);
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let decode = |p: &str| serde_json::from_slice::<Value>(&URL_SAFE_NO_PAD.decode(p).unwrap()).unwrap();
        assert_eq!(decode(parts[0]), json!({ "alg": "HS256", "typ": "JWT", "cty": "twilio-fpa;v=1" }));
        let claims = decode(parts[1]);
        assert_eq!((claims["iss"].as_str(), claims["sub"].as_str(), claims["jti"].as_str()), (Some("SK1"), Some("AC1"), Some("SK1-1800000000")));
        assert_eq!(claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(), TOKEN_SECS);
        assert_eq!(claims["grants"]["identity"], "u0e5c3a8e1f434a4e9a513c1e0c9b7a10");
        assert_eq!(claims["grants"]["voice"], json!({ "incoming": { "allow": false }, "outgoing": { "application_sid": "AP1" } }));
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(b"secret").unwrap();
        mac.update(format!("{}.{}", parts[0], parts[1]).as_bytes());
        assert!(mac.verify_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).is_ok());
        assert_eq!(caller(&format!("client:{}", identity(user))), Some(user));
        assert_eq!(caller("client:someone"), None);
        assert_eq!(caller(&identity(user)), None);
    }

    #[test]
    fn twiml_dials_from_the_org_number_and_records() {
        let twiml = twiml_dial("https://splee.test/", "+14155550100", "+447700900123", false);
        assert!(twiml.contains("<Dial callerId=\"+14155550100\""), "{twiml}");
        assert!(twiml.contains("record=\"record-from-answer-dual\" recordingStatusCallback=\"https://splee.test/api/twilio/calls/recording\""));
        assert!(twiml.contains("<Number statusCallback=\"https://splee.test/api/twilio/calls/status\" statusCallbackEvent=\"completed\">+447700900123</Number>"));
        assert!(!twiml.contains("<Start>"));
        let transcribed = twiml_dial("https://splee.test", "+14155550100", "+447700900123", true);
        assert!(transcribed.contains("<Start><Transcription statusCallbackUrl=\"https://splee.test/api/twilio/calls/transcription\" track=\"both_tracks\""));
        assert_eq!(twiml_refuse("Not <now> & later"), "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response><Say>Not &lt;now&gt; &amp; later</Say><Hangup/></Response>");
    }
}
