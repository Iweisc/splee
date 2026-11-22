//! Calls with the project's leads kept from a cloud phone, Aircall (aircall/) or Dialpad (dialpad/), once its webhook says one
//! ended. A call closes the lead's open call step, like one logged on the Calls page, or is kept as a call of its own, and its
//! transcript goes to the agent for a summary like a Twilio call's (calls.rs).

use serde_json::json;
use uuid::Uuid;

use crate::agent::{self, kind};
use crate::error::AppResult;
use crate::models::{Call, Lead, Ts};
use crate::state::AppState;

/// Which phone a call came from, and so which of calls' columns has its id there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phone {
    Aircall,
    Dialpad,
}

impl Phone {
    fn column(self) -> &'static str {
        match self {
            Phone::Aircall => "aircall_id",
            Phone::Dialpad => "dialpad_id",
        }
    }
}

/// The project and organization of the connection an event came through.
#[derive(Debug, Clone, Copy)]
pub struct Project {
    pub id: Uuid,
    pub org_id: Uuid,
}

/// A call as a phone's events give it.
#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    pub id: i64,
    /// The other side's number in E.164.
    pub number: String,
    pub inbound: bool,
    pub started_at: Ts,
    pub answered_at: Option<Ts>,
    pub ended_at: Option<Ts>,
    /// The team member who made or took it, lowercased.
    pub user_email: String,
    /// Where the recording or voicemail is, which only the team can open, or empty while there's neither.
    pub asset: String,
    /// The call step it was started from on the Calls page, when the phone hands that back, as Dialpad does.
    pub step: Option<Uuid>,
}

impl Ended {
    /// The outcome the Calls page knows, and a note for a call the lead made.
    fn outcome(&self) -> (&'static str, &'static str) {
        match (self.answered_at.is_some(), self.inbound) {
            (true, false) => ("connected", ""),
            (true, true) => ("connected", "They called in."),
            (false, false) => ("no_answer", ""),
            (false, true) if !self.asset.is_empty() => ("no_answer", "They called and left a voicemail."),
            (false, true) => ("no_answer", "They called and nobody answered."),
        }
    }

    /// How long they talked, without the ringing.
    fn talk_secs(&self) -> Option<i32> {
        Some((self.ended_at? - self.answered_at?).num_seconds() as i32).filter(|s| *s >= 0)
    }
}

/// The call Splee kept with `id` from `phone`.
pub async fn kept(state: &AppState, phone: Phone, project: Project, id: i64) -> sqlx::Result<Option<Uuid>> {
    let sql = format!("select id from calls where project_id = $1 and {} = $2", phone.column());
    sqlx::query_scalar(&sql).bind(project.id).bind(id).fetch_optional(&state.db).await
}

/// Keeps a call with one of the project's leads, once: it closes the step its caller started from the Calls page, or else the
/// lead's first open call step not called through Twilio, like one logged on the Calls page, or is kept as a call of its own.
/// A repeated event, or the recording arriving later, only adds a recording the call didn't have. Returns the call, or None
/// when no lead in the project has that number.
///
/// The call and what it does to the lead are kept together, since a retry finds the call and goes no further. These calls
/// are only connected or not answered, and of what calls::apply_outcome does, that only moves last_contacted_at.
pub async fn record(state: &AppState, phone: Phone, project: Project, e: &Ended) -> AppResult<Option<Uuid>> {
    let column = phone.column();
    let mut tx = state.db.begin().await?;
    let sql = format!("update calls set recording_url = case when $3 <> '' then $3 else recording_url end where project_id = $1 and {column} = $2 returning id");
    let again: Option<Uuid> = sqlx::query_scalar(&sql).bind(project.id).bind(e.id).bind(&e.asset).fetch_optional(&mut *tx).await?;
    if again.is_some() {
        tx.commit().await?;
        return Ok(again);
    }
    let user: Option<Uuid> =
        sqlx::query_scalar("select u.id from users u join memberships m on m.user_id = u.id where m.org_id = $1 and lower(u.email::text) = $2")
            .bind(project.org_id)
            .bind(&e.user_email)
            .fetch_optional(&mut *tx)
            .await?;
    // A step with a call through Twilio is left for whoever made it to log in the browser, which a closed step would refuse.
    let dialed: Option<(Uuid, Uuid)> = match (e.step, user.filter(|_| !e.inbound)) {
        // The phone says which step, and it has to be one of this project's, still open, for this number.
        (Some(step), _) => sqlx::query_as(
            "select c.id, c.lead_id from calls c join leads l on l.id = c.lead_id
             where c.id = $1 and c.project_id = $2 and c.status = 'open' and l.phone = $3 and c.twilio_sid is null",
        )
        .bind(step)
        .bind(project.id)
        .bind(&e.number)
        .fetch_optional(&mut *tx)
        .await?,
        // Aircall doesn't, so it's the step they put in Aircall from the Calls page, if the call started within half an hour of
        // that. The latest one put there before the call started wins, so a late event doesn't take a newer step's place. The
        // minute after allows for Aircall's clock running behind.
        (None, Some(user)) if phone == Phone::Aircall => sqlx::query_as(
            "select c.id, c.lead_id from calls c join leads l on l.id = c.lead_id
             where c.project_id = $1 and c.status = 'open' and c.aircall_dialed_by = $2 and l.phone = $3 and c.twilio_sid is null
               and c.aircall_dialed_at between $4 - interval '30 minutes' and $4 + interval '1 minute'
             order by c.aircall_dialed_at desc limit 1",
        )
        .bind(project.id)
        .bind(user)
        .bind(&e.number)
        .bind(e.started_at)
        .fetch_optional(&mut *tx)
        .await?,
        _ => None,
    };
    let lead: Option<Lead> = sqlx::query_as(
        "select * from leads where project_id = $1 and (id = $3 or $3::uuid is null and phone = $2)
         order by last_contacted_at desc nulls last, created_at desc limit 1",
    )
    .bind(project.id)
    .bind(&e.number)
    .bind(dialed.map(|(_, lead)| lead))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(lead) = lead else { return Ok(None) };
    let (outcome, notes) = e.outcome();
    let (call_status, ended) = (if e.answered_at.is_some() { "completed" } else { "no-answer" }, e.ended_at.unwrap_or(e.started_at));
    let sql = format!(
        "update calls set status = 'done', outcome = $2, notes = $3, closed_at = $4, called_by = coalesce(called_by, $5), called_at = $6,
           {column} = $7, to_number = $8, call_status = $9, duration_secs = $10, recording_url = $11
         where id = coalesce($12, (select id from calls where lead_id = $1 and status = 'open' and twilio_sid is null order by due_at limit 1))
           and status = 'open'
         returning *"
    );
    let open: Option<Call> = sqlx::query_as(&sql)
        .bind(lead.id)
        .bind(outcome)
        .bind(notes)
        .bind(ended)
        .bind(user)
        .bind(e.started_at)
        .bind(e.id)
        .bind(&e.number)
        .bind(call_status)
        .bind(e.talk_secs())
        .bind(&e.asset)
        .bind(dialed.map(|(call, _)| call))
        .fetch_optional(&mut *tx)
        .await?;
    let call: Option<Call> = match open {
        Some(call) => Some(call),
        // A delivery of the same call at the same moment keeps it first, and this one finds it below.
        None => {
            let sql = format!(
                "insert into calls (project_id, lead_id, segment_id, kind, owner_id, status, to_number, called_by, called_at, closed_at, outcome, notes,
                                    {column}, call_status, duration_secs, recording_url)
                 values ($1, $2, $3, 'manual', $4, 'done', $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                 on conflict (project_id, {column}) where {column} is not null do nothing returning *"
            );
            sqlx::query_as(&sql)
                .bind(lead.project_id)
                .bind(lead.id)
                .bind(lead.segment_id)
                .bind(lead.owner_id)
                .bind(&e.number)
                .bind(user)
                .bind(e.started_at)
                .bind(ended)
                .bind(outcome)
                .bind(notes)
                .bind(e.id)
                .bind(call_status)
                .bind(e.talk_secs())
                .bind(&e.asset)
                .fetch_optional(&mut *tx)
                .await?
        }
    };
    let Some(call) = call else {
        tx.commit().await?;
        return Ok(kept(state, phone, project, e.id).await?);
    };
    if outcome == "connected" {
        sqlx::query("update leads set last_contacted_at = greatest(last_contacted_at, $2) where id = $1")
            .bind(lead.id)
            .bind(ended)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Some(call.id))
}

/// Puts the phone's transcript on the call and asks the agent for a summary, once. Saved with its summary run, since a retry
/// finds the transcript and doesn't ask again.
pub async fn transcribed(state: &AppState, project: Project, call: Uuid, transcript: &str) -> AppResult<()> {
    if transcript.is_empty() {
        return Ok(());
    }
    let mut tx = state.db.begin().await?;
    let saved: Option<Uuid> = sqlx::query_scalar("update calls set transcript = $2, transcribed_at = now() where id = $1 and transcribed_at is null returning id")
        .bind(call)
        .bind(transcript)
        .fetch_optional(&mut *tx)
        .await?;
    if saved.is_some() {
        agent::enqueue(&mut *tx, kind::SUMMARIZE_CALL, Some(project.id), Some(project.org_id), json!({ "call_id": call })).await?;
    }
    tx.commit().await?;
    Ok(())
}
