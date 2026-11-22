//! Voicemail drop: each team member records the voicemail they leave once, and when a call from the Calls page reaches the
//! lead's voicemail, one click plays it on the lead's side and hangs up, so the caller moves on while it plays. The browser
//! records it as a WAV, since that's what Twilio's <Play> takes, and Twilio fetches it from a link only it is given.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

use super::util_scope;
use crate::auth::Auth;
use crate::calls;
use crate::error::{AppError, AppResult};
use crate::models::{Call, Ts};
use crate::state::AppState;
use crate::twilio::{Account, Client};

const MIN_SECS: f64 = 2.0;
const MAX_SECS: f64 = 60.0;
/// A minute of 16 kHz mono 16-bit audio, with room for the header. The browser records at 8 kHz, half that.
const MAX_BYTES: usize = 2_000_000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/voicemail", get(mine).put(record).delete(remove).layer(DefaultBodyLimit::max(MAX_BYTES)))
        .route("/voicemail/audio", get(listen))
        .route("/voicemails/{file}", get(audio))
        .route("/calls/{id}/voicemail", post(leave))
}

#[derive(Serialize, FromRow)]
struct Voicemail {
    secs: i32,
    created_at: Ts,
}

/// Where Twilio fetches a voicemail from.
fn audio_url(public_url: &str, token: &str) -> String {
    format!("{public_url}/api/voicemails/{token}.wav")
}

/// How long a PCM WAV plays for, from its format and the length of its data, or why it isn't one. The format's fields have to
/// agree with each other, so a header can't pass a long recording off as a short one.
fn wav_secs(bytes: &[u8]) -> Result<f64, &'static str> {
    const NOT_WAV: &str = "That isn't a WAV recording";
    const NOT_PCM: &str = "The recording has to be plain PCM audio";
    let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as u32;
    let u32_at = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(NOT_WAV);
    }
    let (mut at, mut byte_rate, mut data) = (12usize, None, None);
    while at < bytes.len() {
        let head = bytes.get(at..at.saturating_add(8)).ok_or(NOT_WAV)?;
        let len = u32_at(head, 4) as usize;
        let body = bytes.get(at + 8..(at + 8).saturating_add(len)).ok_or(NOT_WAV)?;
        let id = &head[0..4];
        // One of each, or the length would come from one chunk while another holds the audio.
        if (id == b"fmt " && byte_rate.is_some()) || (id == b"data" && data.is_some()) {
            return Err(NOT_WAV);
        }
        match id {
            b"fmt " => {
                if body.len() < 16 || u16_at(body, 0) != 1 {
                    return Err(NOT_PCM);
                }
                let (channels, rate, per_sec, align, bits) = (u16_at(body, 2), u32_at(body, 4), u32_at(body, 8), u16_at(body, 12), u16_at(body, 14));
                let usual = (1..=2).contains(&channels) && matches!(bits, 8 | 16) && (8_000..=48_000).contains(&rate);
                if !usual || align != channels * bits / 8 || per_sec != rate * align {
                    return Err(NOT_PCM);
                }
                byte_rate = Some(per_sec);
            }
            b"data" => data = Some(len),
            _ => {}
        }
        // Chunks are padded to an even length, though the last one's pad byte is often left off.
        at = (at + 8).saturating_add(len).saturating_add(len % 2);
    }
    match (byte_rate, data) {
        (Some(byte_rate), Some(data)) => Ok(data as f64 / byte_rate as f64),
        _ => Err(NOT_WAV),
    }
}

async fn mine(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Option<Voicemail>>> {
    let found = sqlx::query_as("select secs, created_at from voicemails where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(auth.user_id)
        .fetch_optional(&state.db)
        .await?;
    Ok(Json(found))
}

/// Keeps a new recording in place of the last one, under a new link, so Twilio never plays the old one from its cache.
async fn record(State(state): State<AppState>, auth: Auth, body: Bytes) -> AppResult<Json<Voicemail>> {
    let secs = wav_secs(&body).map_err(|e| AppError::BadRequest(e.into()))?;
    if secs < MIN_SECS {
        return Err(AppError::BadRequest("Record at least two seconds".into()));
    }
    // Half a second over, since the browser stops the recording a moment after the minute.
    if secs > MAX_SECS + 0.5 {
        return Err(AppError::BadRequest("Keep it to a minute or less".into()));
    }
    let saved = sqlx::query_as(
        "insert into voicemails (org_id, user_id, audio, secs, token) values ($1, $2, $3, $4, $5)
         on conflict (org_id, user_id) do update set audio = $3, secs = $4, token = $5, created_at = now()
         returning secs, created_at",
    )
    .bind(auth.org_id)
    .bind(auth.user_id)
    .bind(body.as_ref())
    .bind(secs.round() as i32)
    .bind(crate::crypto::random_token())
    .fetch_one(&state.db)
    .await?;
    Ok(Json(saved))
}

async fn remove(State(state): State<AppState>, auth: Auth) -> AppResult<StatusCode> {
    sqlx::query("delete from voicemails where org_id = $1 and user_id = $2").bind(auth.org_id).bind(auth.user_id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn wav(audio: Vec<u8>, cache: &'static str) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "audio/wav"), (header::CACHE_CONTROL, cache)], audio)
}

/// Your own recording, to play back on the Calls page.
async fn listen(State(state): State<AppState>, auth: Auth) -> AppResult<impl IntoResponse> {
    let audio: Option<Vec<u8>> = sqlx::query_scalar("select audio from voicemails where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(auth.user_id)
        .fetch_optional(&state.db)
        .await?;
    Ok(wav(audio.ok_or(AppError::NotFound)?, "private, no-store"))
}

/// The recording for Twilio, which has no session, so the token in the link is the key. A new recording gets a new link,
/// which is why it can be cached.
async fn audio(State(state): State<AppState>, Path(file): Path<String>) -> AppResult<impl IntoResponse> {
    let token = file.strip_suffix(".wav").ok_or(AppError::NotFound)?;
    let audio: Option<Vec<u8>> = sqlx::query_scalar("select audio from voicemails where token = $1").bind(token).fetch_optional(&state.db).await?;
    Ok(wav(audio.ok_or(AppError::NotFound)?, "public, max-age=86400"))
}

#[derive(FromRow)]
struct Placed {
    status: String,
    twilio_sid: Option<String>,
    called_by: Option<Uuid>,
}

/// Plays your voicemail to the lead and hangs up their side, which ends the call in your browser too. Logging it stays with
/// you, like any call.
async fn leave(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<StatusCode> {
    let call: Call = util_scope::load(&state.db, "calls", id, auth.org_id).await?;
    let placed: Placed = sqlx::query_as("select status, twilio_sid, called_by from calls where id = $1").bind(call.id).fetch_one(&state.db).await?;
    if placed.status != "open" {
        return Err(AppError::Conflict("This call was already logged, skipped or cancelled".into()));
    }
    let Some(sid) = placed.twilio_sid.filter(|_| placed.called_by == Some(auth.user_id)) else {
        return Err(AppError::Conflict("A voicemail can only go into a call you're on from the Calls page".into()));
    };
    let token: Option<String> = sqlx::query_scalar("select token from voicemails where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(auth.user_id)
        .fetch_optional(&state.db)
        .await?;
    let token = token.ok_or_else(|| AppError::Conflict("Record your voicemail on the Calls page first".into()))?;
    let account = Account::of_org(&state.db, auth.org_id).await?.ok_or_else(|| AppError::Conflict("Turn on calling in the Twilio card first".into()))?;
    let twiml = calls::twiml_play(&audio_url(&state.cfg.public_url, &token));
    let played = Client::for_account(&state, &account)?.play_to_callee(&sid, &twiml).await.map_err(|e| AppError::BadRequest(e.reason().into()))?;
    if !played {
        return Err(AppError::Conflict("They haven't picked up. Leave it once their voicemail answers".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use reqwest::Method;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use super::*;
    use crate::routes::calls::tests::{SAM, Seeded};
    use crate::twilio::fake;

    /// `secs` of silence as a mono 16-bit PCM WAV at 8 kHz, the way the browser records it.
    fn recording(secs: u32) -> Vec<u8> {
        let data = secs * 16_000;
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&[1, 0, 1, 0]);
        w.extend_from_slice(&8_000u32.to_le_bytes());
        w.extend_from_slice(&16_000u32.to_le_bytes());
        w.extend_from_slice(&[2, 0, 16, 0]);
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data.to_le_bytes());
        w.resize(w.len() + data as usize, 0);
        w
    }

    async fn put(s: &Seeded, session: &str, audio: Vec<u8>) -> (u16, Value) {
        let res = s.client.put(format!("{}/voicemail", s.base)).header("cookie", format!("splee_session={session}")).body(audio).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    async fn get(s: &Seeded, path: &str, session: Option<&str>) -> (u16, String, Vec<u8>) {
        let mut req = s.client.get(format!("{}{path}", s.base));
        if let Some(session) = session {
            req = req.header("cookie", format!("splee_session={session}"));
        }
        let res = req.send().await.unwrap();
        let kind = res.headers().get("content-type").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
        (res.status().as_u16(), kind, res.bytes().await.unwrap().to_vec())
    }

    async fn leave(s: &Seeded, session: &str, call: Uuid) -> (u16, Value) {
        s.call(session, Method::POST, &format!("/calls/{call}/voicemail"), json!(null)).await
    }

    async fn token(s: &Seeded, user: Uuid) -> String {
        sqlx::query_scalar("select token from voicemails where user_id = $1").bind(user).fetch_one(s.db()).await.unwrap()
    }

    #[test]
    fn only_pcm_wavs_have_a_length() {
        assert_eq!(wav_secs(&recording(3)), Ok(3.0));
        assert_eq!(wav_secs(b"ID3 an mp3"), Err("That isn't a WAV recording"));
        let mut float = recording(3);
        float[20] = 3;
        assert_eq!(wav_secs(&float), Err("The recording has to be plain PCM audio"));
        // Two minutes that say they play at twice the rate.
        let mut doubled = recording(120);
        doubled[28..32].copy_from_slice(&32_000u32.to_le_bytes());
        assert_eq!(wav_secs(&doubled), Err("The recording has to be plain PCM audio"));
        let mut stereo = recording(3);
        stereo[22] = 2;
        assert_eq!(wav_secs(&stereo), Err("The recording has to be plain PCM audio"));
        // A second data or format chunk.
        let mut twice = recording(120);
        twice.extend_from_slice(&recording(3)[36..]);
        assert_eq!(wav_secs(&twice), Err("That isn't a WAV recording"));
        let mut twice = recording(3);
        twice.splice(36..36, recording(3)[12..36].to_vec());
        assert_eq!(wav_secs(&twice), Err("That isn't a WAV recording"));
        // Cut short, in the data or in the format.
        assert_eq!(wav_secs(&recording(3)[..1000]), Err("That isn't a WAV recording"));
        assert_eq!(wav_secs(&recording(3)[..30]), Err("That isn't a WAV recording"));
    }

    #[sqlx::test]
    async fn each_member_keeps_one_recording_that_twilio_fetches_by_its_link(db: PgPool) {
        let s = Seeded::start(db).await;
        assert_eq!(s.call("jo", Method::GET, "/voicemail", json!(null)).await, (200, Value::Null));
        assert_eq!(put(&s, "jo", b"ID3 an mp3".to_vec()).await.1["error"], "That isn't a WAV recording");
        assert_eq!(put(&s, "jo", recording(1)).await.1["error"], "Record at least two seconds");
        assert_eq!(put(&s, "jo", recording(61)).await.1["error"], "Keep it to a minute or less");
        assert_eq!(put(&s, "nobody", recording(3)).await.0, 401);

        let (status, saved) = put(&s, "jo", recording(3)).await;
        assert_eq!((status, saved["secs"].as_i64()), (200, Some(3)));
        assert_eq!(s.call("jo", Method::GET, "/voicemail", json!(null)).await.1["secs"], 3);
        assert_eq!(get(&s, "/voicemail/audio", Some("jo")).await, (200, "audio/wav".into(), recording(3)));
        // Only Jo's own, and Twilio's link needs no session.
        assert_eq!(s.call("acme", Method::GET, "/voicemail", json!(null)).await.1, Value::Null);
        assert_eq!(get(&s, "/voicemail/audio", Some("acme")).await.0, 404);
        let first = token(&s, s.jo).await;
        assert_eq!(get(&s, &format!("/voicemails/{first}.wav"), None).await, (200, "audio/wav".into(), recording(3)));
        assert_eq!(get(&s, &format!("/voicemails/{first}"), None).await.0, 404);

        // Recording again moves it to a new link.
        assert_eq!(put(&s, "jo", recording(5)).await.1["secs"], 5);
        let second = token(&s, s.jo).await;
        assert_ne!(first, second);
        assert_eq!(get(&s, &format!("/voicemails/{first}.wav"), None).await.0, 404);
        assert_eq!(get(&s, &format!("/voicemails/{second}.wav"), None).await.2, recording(5));

        assert_eq!(s.call("jo", Method::DELETE, "/voicemail", json!(null)).await.0, 204);
        assert_eq!(s.call("jo", Method::GET, "/voicemail", json!(null)).await.1, Value::Null);
        assert_eq!(get(&s, &format!("/voicemails/{second}.wav"), None).await.0, 404);
    }

    #[sqlx::test]
    async fn the_caller_leaves_their_voicemail_on_the_leads_side_of_their_call(db: PgPool) {
        let s = Seeded::start(db).await;
        let call = s.open_call(s.sam).await;
        assert_eq!(leave(&s, "jo", call).await.1["error"], "A voicemail can only go into a call you're on from the Calls page");

        // Still ringing.
        let placed = "update calls set twilio_sid = $2, called_by = $3 where id = $1";
        sqlx::query(placed).bind(call).bind(fake::RINGING).bind(s.jo).execute(s.db()).await.unwrap();
        assert_eq!(leave(&s, "jo", call).await.1["error"], "Record your voicemail on the Calls page first");
        assert_eq!(put(&s, "jo", recording(3)).await.0, 200);
        put(&s, "acme", recording(4)).await;
        assert_eq!(leave(&s, "jo", call).await.1["error"], "They haven't picked up. Leave it once their voicemail answers");

        sqlx::query(placed).bind(call).bind(None::<String>).bind(None::<Uuid>).execute(s.db()).await.unwrap();
        assert!(s.voice(call, s.jo, SAM, "CA1").await.1.contains("<Dial"));
        // Someone else's call, or another organization's.
        assert_eq!(leave(&s, "acme", call).await.1["error"], "A voicemail can only go into a call you're on from the Calls page");
        assert_eq!(leave(&s, "beta", call).await.0, 404);
        assert!(s.sent.lock().unwrap().is_empty());

        assert_eq!(leave(&s, "jo", call).await.0, 204);
        let sent = s.sent.lock().unwrap().pop().unwrap();
        assert_eq!(sent["Call"], "CA1x");
        assert_eq!(sent["Twiml"], calls::twiml_play(&format!("https://splee.test/api/voicemails/{}.wav", token(&s, s.jo).await)));

        // Not once it's logged.
        assert_eq!(s.call("jo", Method::POST, &format!("/calls/{call}/log"), json!({ "outcome": "voicemail" })).await.0, 200);
        assert_eq!(leave(&s, "jo", call).await.1["error"], "This call was already logged, skipped or cancelled");
        assert!(s.sent.lock().unwrap().is_empty());
    }
}
