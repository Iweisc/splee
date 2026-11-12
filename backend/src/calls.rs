//! Calls to leads. A follow-up step of kind "call" becomes an open call for the lead's owner when it comes due, and the
//! sequence waits for someone to log what happened or skip it (LAST_STEP). Calls are made from the browser through the
//! organization's Twilio account: Splee mints a short-lived Access Token, the Voice SDK calls the TwiML App, and Twilio asks
//! /api/twilio/voice what to do, which dials the lead only after the checks in `authorize`. Twilio's callbacks then bring
//! the length, the recording and, with transcription on, the transcript the agent summarizes.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Timelike;
use chrono_tz::Tz;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use crate::agent::{self, kind};
use crate::models::{Call, Lead, Ts};
use crate::phone;
use crate::state::AppState;

/// Calls go to a lead between 8:00 and 20:00 their time.
pub const FIRST_HOUR: u32 = 8;
pub const LAST_HOUR: u32 = 20;
pub const OUTCOMES: &[&str] = &["connected", "voicemail", "no_answer", "wrong_number", "not_interested", "meeting_booked"];
/// How long a browser's token to place calls lasts. The page asks for a new one for each call.
pub const TOKEN_SECS: i64 = 600;

/// The lead's last finished step of the email sequence, as `last` (step, at): the last email sent, or a call step done or
/// skipped after it. Joined laterally on a lead aliased `l`, `join {LAST_STEP} on true`.
pub const LAST_STEP: &str = "lateral (select x.step, x.at from (
       select e.step, e.sent_at as at from email_messages e
       where e.lead_id = l.id and e.direction = 'outbound' and e.kind in ('initial', 'follow_up') and e.status = 'sent'
       union all
       select c.step, c.closed_at from calls c where c.lead_id = l.id and c.kind = 'sequence' and c.status in ('done', 'skipped')
     ) x order by x.step desc limit 1) last";

/// Whether the lead `l` of project `p` is on the organization's do-not-contact list, by number, email or domain.
const SUPPRESSED: &str = "exists (select 1 from suppressions x where x.org_id = p.org_id
       and ((x.kind = 'phone' and x.value = l.phone) or x.value = l.email or x.value = split_part(l.email::text, '@', 2)))";

/// Takes a lead's open calls off. $1 is the lead, $2 the reason shown on them.
pub const STOP_CALLS: &str = "update calls set status = 'cancelled', cancel_reason = $2, closed_at = now() where lead_id = $1 and status = 'open'";

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

/// A Twilio Access Token with a Voice grant that lets the browser place calls through the TwiML App, and nothing else: no
/// incoming calls. It's a JWT signed with the API key's secret, as Twilio's helper libraries make it.
pub fn access_token(account_sid: &str, key_sid: &str, key_secret: &str, app_sid: &str, identity: &str, now: i64) -> String {
    let header = json!({ "alg": "HS256", "typ": "JWT", "cty": "twilio-fpa;v=1" });
    let claims = json!({
        "jti": format!("{key_sid}-{now}"),
        "iss": key_sid,
        "sub": account_sid,
        "iat": now,
        "nbf": now,
        "exp": now + TOKEN_SECS,
        "grants": { "identity": identity, "voice": { "incoming": { "allow": false }, "outgoing": { "application_sid": app_sid } } },
    });
    let input = format!("{}.{}", URL_SAFE_NO_PAD.encode(header.to_string()), URL_SAFE_NO_PAD.encode(claims.to_string()));
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key_secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// What Twilio says to the caller instead of dialing, then it hangs up.
pub fn twiml_refuse(reason: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response><Say>{}</Say><Hangup/></Response>", xml(reason))
}

/// Dials the lead from the organization's number, recording both sides from when they answer. With transcription on, Twilio
/// transcribes the call too: the inbound track is the caller in the browser, the outbound one the lead.
pub fn twiml_dial(public_url: &str, from: &str, to: &str, transcribe: bool) -> String {
    let start = if transcribe {
        format!(
            "<Start><Transcription statusCallbackUrl=\"{}\" track=\"both_tracks\" inboundTrackLabel=\"Us\" outboundTrackLabel=\"Lead\"/></Start>",
            xml(&transcription_url(public_url))
        )
    } else {
        String::new()
    };
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

/// Checks that `user_id` may call the lead of call `call_id` now: they're on the lead's organization, `to` (when given) is the
/// lead's number, the lead isn't on the do-not-contact list, it's within their calling hours, and the organization's Twilio
/// is set up for calls. Err is the reason, in words for the caller.
pub async fn authorize(db: &PgPool, org_id: Uuid, user_id: Uuid, call_id: Uuid, to: Option<&str>, now: Ts) -> sqlx::Result<Result<Dial, String>> {
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
    let (suppressed, project_tz): (bool, String) = sqlx::query_as(&format!(
        "select {SUPPRESSED} or l.status in ('unsubscribed', 'do_not_contact'), coalesce(ps.timezone, 'UTC')
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
    let account: Option<(String, String, String, String, String, String, bool)> = sqlx::query_as(
        "select account_sid, api_key_sid, api_secret_enc, twiml_app_sid, voice_from, sms_from, call_transcription from twilio_accounts where org_id = $1",
    )
    .bind(org_id)
    .fetch_optional(db)
    .await?;
    let Some((account_sid, api_key_sid, api_secret_enc, twiml_app_sid, voice_from, sms_from, transcribe)) = account else {
        return Ok(Err("Connect Twilio on the Integrations page to call from Splee.".into()));
    };
    let from = if voice_from.is_empty() && sms_from.starts_with('+') { sms_from } else { voice_from };
    if twiml_app_sid.is_empty() || from.is_empty() {
        return Ok(Err("Turn on calling in the Twilio card on the Integrations page first.".into()));
    }
    Ok(Ok(Dial { call, lead, from, transcribe, account_sid, api_key_sid, api_secret_enc, twiml_app_sid }))
}

/// Every minute with the sender: call steps that came due become open calls, and open calls of leads who replied, booked or
/// went on the do-not-contact list are cancelled.
pub async fn tick(db: &PgPool) -> anyhow::Result<()> {
    schedule(db).await?;
    cancel_stale(db, None).await?;
    Ok(())
}

/// Opens the call step of each lead whose step is due now, for the lead's owner. A lead with no number, or whose number is on
/// the do-not-contact list, gets it skipped instead, so their sequence goes on. Campaigns brought over from another tool keep
/// their own steps, which are all emails.
pub async fn schedule(db: &PgPool) -> sqlx::Result<u64> {
    let done = sqlx::query(&format!(
        "with due as (
           select l.id as lead_id, l.project_id, l.segment_id, l.owner_id, l.phone, last.step + 1 as step,
                  coalesce(ps.follow_ups -> last.step ->> 'instructions', '') as instructions,
                  last.at + make_interval(days => greatest(1, coalesce((ps.follow_ups -> last.step ->> 'delay_days')::int, 3))) as due_at,
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
             and jsonb_typeof(ps.follow_ups) = 'array' and jsonb_array_length(ps.follow_ups) > last.step
             and ps.follow_ups -> last.step ->> 'kind' = 'call'
             and coalesce((ps.follow_ups -> last.step ->> 'enabled')::boolean, false)
             and not exists (select 1 from email_messages i where i.lead_id = l.id and i.direction = 'inbound')
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
/// paths that change a lead do it at once too. This catches the rest, like an address added to the list by hand.
pub async fn cancel_stale(db: &PgPool, project: Option<Uuid>) -> sqlx::Result<u64> {
    let done = sqlx::query(&format!(
        "update calls c set status = 'cancelled', closed_at = now(), cancel_reason = r.why
         from (select o.id, case
                 when {SUPPRESSED} or l.status in ('unsubscribed', 'do_not_contact') then 'On the do-not-contact list'
                 when l.status = 'meeting_booked' then 'They booked a meeting'
                 when l.status = 'not_interested' then 'Marked not interested'
                 when l.status in ('replied', 'hot') or exists (select 1 from email_messages i where i.lead_id = l.id and i.direction = 'inbound')
                   then 'They replied'
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

/// Stops everything still to go to the lead: unsent emails, LinkedIn steps, texts and other open calls.
async fn stop_sequence(db: &PgPool, lead_id: Uuid, why: &str) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query(
        "update email_messages set status = 'cancelled', error = $2
         where lead_id = $1 and direction = 'outbound' and kind in ('initial', 'follow_up') and status in ('draft', 'approved', 'scheduled')",
    )
    .bind(lead_id)
    .bind(why)
    .execute(&mut *tx)
    .await?;
    sqlx::query(crate::mail::STOP_LINKEDIN).bind(lead_id).execute(&mut *tx).await?;
    sqlx::query(crate::twilio::STOP_TEXTS).bind(lead_id).bind(why).execute(&mut *tx).await?;
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
            let who = if matches!(param("Track"), "inbound_track" | "Us") { "Us" } else { "Lead" };
            let part = json!([{ "seq": param("SequenceId").parse::<i64>().unwrap_or_default(), "who": who, "text": text }]);
            sqlx::query(
                "update calls c set transcript_parts = c.transcript_parts || $3 from projects p
                 where p.id = c.project_id and p.org_id = $1 and c.twilio_sid = $2 and c.transcribed_at is null",
            )
            .bind(org_id)
            .bind(call_sid)
            .bind(part)
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

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
        let token = access_token("AC1", "SK1", "secret", "AP1", &identity(user), 1_800_000_000);
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
