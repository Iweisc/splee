//! Passkeys: adding them from the Account page and signing in with one, a discoverable credential, so no email is typed.
//! The checks are in webauthn.rs. Adding one takes the same proof as turning two-factor off, given when the challenge is
//! made, so a stolen session can't add its own. A sign-in where the authenticator verified the user (a PIN, fingerprint or
//! face) counts as both factors. Without that, two-factor still asks for its code.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use axum::extract::{Path, State};
use axum::routing::{patch, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::auth::{ProofBody, SignIn, check_proof, count_attempt, hold_user, sign_in};
use crate::audit;
use crate::auth::Auth;
use crate::crypto::sha256_hex;
use crate::error::{AppError, AppResult};
use crate::models::Passkey;
use crate::state::AppState;
use crate::two_factor;
use crate::webauthn::{self, ES256, RS256, Rp};

const CHALLENGE_MINUTES: i64 = 5;
const NAME_CHARS: usize = 60;
const TRANSPORTS: &[&str] = &["usb", "nfc", "ble", "internal", "hybrid", "smart-card"];
const COULD_NOT_CHECK: &str = "Splee couldn't check that passkey. Try again.";
const EXPIRED: &str = "This took too long or was already used. Try again.";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/passkeys", post(add).get(list))
        .route("/auth/passkeys/options", post(add_options))
        .route("/auth/passkeys/{id}", patch(rename).delete(remove))
        .route("/auth/passkeys/sign-in/options", post(sign_in_options))
        .route("/auth/passkeys/sign-in", post(sign_in_with_passkey))
}

fn rp(state: &AppState) -> AppResult<Rp> {
    Rp::new(&state.cfg.public_url).map_err(AppError::Internal)
}

fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode(value: &str) -> AppResult<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(value.trim_end_matches('=')).map_err(|_| AppError::BadRequest(COULD_NOT_CHECK.into()))
}

/// Logged, not shown: the browser made what it sent, so the member can't do anything about the reason.
fn refuse(why: &'static str) -> AppError {
    tracing::info!(why, "refused a passkey");
    AppError::BadRequest(COULD_NOT_CHECK.into())
}

/// A new random challenge, stored hashed, for adding a passkey as `user_id` or, with None, for signing in.
async fn new_challenge(db: &PgPool, user_id: Option<Uuid>) -> sqlx::Result<String> {
    sqlx::query("delete from passkey_challenges where expires_at < now()").execute(db).await?;
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let challenge = b64url(&bytes);
    sqlx::query(&format!(
        "insert into passkey_challenges (challenge_hash, user_id, expires_at) values ($1, $2, now() + interval '{CHALLENGE_MINUTES} minutes')"
    ))
    .bind(sha256_hex(&challenge))
    .bind(user_id)
    .execute(db)
    .await?;
    Ok(challenge)
}

/// Uses up a live challenge made for `user_id`, or for a sign-in with None. Another member's challenge isn't used up.
async fn take_challenge(db: &PgPool, challenge: &str, user_id: Option<Uuid>) -> AppResult<()> {
    let taken = sqlx::query("delete from passkey_challenges where challenge_hash = $1 and user_id is not distinct from $2 and expires_at > now()")
        .bind(sha256_hex(challenge))
        .bind(user_id)
        .execute(db)
        .await?;
    if taken.rows_affected() == 0 {
        return Err(AppError::BadRequest(EXPIRED.into()));
    }
    Ok(())
}

async fn passkeys(db: &PgPool, user_id: Uuid) -> sqlx::Result<Vec<Passkey>> {
    sqlx::query_as::<_, Passkey>("select id, name, user_verified, created_at, last_used_at from passkeys where user_id = $1 order by created_at")
        .bind(user_id)
        .fetch_all(db)
        .await
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<Passkey>>> {
    Ok(Json(passkeys(&state.db, auth.user_id).await?))
}

fn passkey_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.chars().count() > NAME_CHARS {
        return Err(AppError::BadRequest(format!("Keep the name under {NAME_CHARS} characters")));
    }
    Ok(if name.is_empty() { "Passkey".into() } else { name.into() })
}

/// The options for navigator.credentials.create(), after the member's password or a code. Asks for a discoverable
/// credential, so it can sign in without an email, and leaves out the member's passkeys already on that authenticator.
async fn add_options(State(state): State<AppState>, auth: Auth, Json(body): Json<ProofBody>) -> AppResult<Json<Value>> {
    check_proof(&state, &auth, &body.proof).await?;
    let rp = rp(&state)?;
    let (email, name): (String, String) = sqlx::query_as("select email::text, name from users where id = $1").bind(auth.user_id).fetch_one(&state.db).await?;
    let existing: Vec<(Vec<u8>, Vec<String>)> =
        sqlx::query_as("select credential_id, transports from passkeys where user_id = $1").bind(auth.user_id).fetch_all(&state.db).await?;
    let challenge = new_challenge(&state.db, Some(auth.user_id)).await?;
    let display_name = if name.is_empty() { &email } else { &name };
    let exclude: Vec<Value> = existing.iter().map(|(id, transports)| json!({ "type": "public-key", "id": b64url(id), "transports": transports })).collect();
    Ok(Json(json!({
        "challenge": challenge,
        "rp": { "id": rp.id, "name": "Splee" },
        "user": { "id": b64url(auth.user_id.as_bytes()), "name": email, "displayName": display_name },
        "pubKeyCredParams": [{ "type": "public-key", "alg": ES256 }, { "type": "public-key", "alg": RS256 }],
        "authenticatorSelection": { "residentKey": "required", "requireResidentKey": true, "userVerification": "preferred" },
        "attestation": "none",
        "timeout": CHALLENGE_MINUTES * 60_000,
        "excludeCredentials": exclude,
    })))
}

/// A new passkey, from the browser's AuthenticatorAttestationResponse. All but `name`, `alg` and `transports` are base64url.
#[derive(Deserialize)]
struct NewPasskey {
    #[serde(default)]
    name: String,
    credential_id: String,
    client_data_json: String,
    authenticator_data: String,
    /// getPublicKey(), SPKI DER.
    public_key: String,
    /// getPublicKeyAlgorithm().
    alg: i32,
    #[serde(default)]
    transports: Vec<String>,
}

/// Attestation is "none", so there's no signature to check yet. The first sign-in checks the key.
async fn add(State(state): State<AppState>, auth: Auth, Json(body): Json<NewPasskey>) -> AppResult<Json<Vec<Passkey>>> {
    let rp = rp(&state)?;
    let client_data = decode(&body.client_data_json)?;
    let challenge = webauthn::challenge(&client_data, "webauthn.create", &rp).map_err(refuse)?;
    take_challenge(&state.db, &challenge, Some(auth.user_id)).await?;
    let authenticator_data = webauthn::authenticator_data(&decode(&body.authenticator_data)?, &rp).map_err(refuse)?;
    let credential_id = decode(&body.credential_id)?;
    if credential_id.is_empty() || authenticator_data.credential_id.as_deref() != Some(&credential_id[..]) {
        return Err(refuse("credential id"));
    }
    let public_key = decode(&body.public_key)?;
    if !webauthn::usable_key(&public_key, body.alg) {
        return Err(refuse("public key"));
    }
    let name = passkey_name(&body.name)?;
    let transports: Vec<String> = body.transports.iter().filter(|t| TRANSPORTS.contains(&t.as_str())).cloned().collect();
    let saved = sqlx::query(
        "insert into passkeys (user_id, credential_id, public_key, alg, sign_count, transports, name, user_verified)
         values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(auth.user_id)
    .bind(&credential_id)
    .bind(&public_key)
    .bind(body.alg)
    .bind(i64::from(authenticator_data.sign_count))
    .bind(&transports)
    .bind(&name)
    .bind(authenticator_data.user_verified)
    .execute(&state.db)
    .await;
    if let Err(e) = saved {
        return Err(match AppError::from(e) {
            AppError::Conflict(_) => AppError::Conflict("That passkey is already on a Splee account.".into()),
            other => other,
        });
    }
    audit::record_account(&state.db, auth.user_id, "user.passkey_added", json!({ "name": name })).await;
    Ok(Json(passkeys(&state.db, auth.user_id).await?))
}

#[derive(Deserialize)]
struct RenameBody {
    name: String,
}

async fn rename(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<RenameBody>) -> AppResult<Json<Vec<Passkey>>> {
    let name = passkey_name(&body.name)?;
    let renamed = sqlx::query("update passkeys set name = $3 where id = $1 and user_id = $2").bind(id).bind(auth.user_id).bind(&name).execute(&state.db).await?;
    if renamed.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(passkeys(&state.db, auth.user_id).await?))
}

/// Takes the password or a code, like adding one. A passkey is never the last way in: the password or a Google or
/// Microsoft account it was added with is still there.
async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<ProofBody>) -> AppResult<Json<Vec<Passkey>>> {
    check_proof(&state, &auth, &body.proof).await?;
    let name: String = sqlx::query_scalar("delete from passkeys where id = $1 and user_id = $2 returning name")
        .bind(id)
        .bind(auth.user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    audit::record_account(&state.db, auth.user_id, "user.passkey_removed", json!({ "name": name })).await;
    Ok(Json(passkeys(&state.db, auth.user_id).await?))
}

/// The options for navigator.credentials.get(), public. No allowCredentials, so the browser offers any passkey it has for Splee.
async fn sign_in_options(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let rp = rp(&state)?;
    let challenge = new_challenge(&state.db, None).await?;
    Ok(Json(json!({ "challenge": challenge, "rpId": rp.id, "userVerification": "preferred", "timeout": CHALLENGE_MINUTES * 60_000 })))
}

/// The browser's AuthenticatorAssertionResponse, base64url.
#[derive(Deserialize)]
struct Assertion {
    credential_id: String,
    client_data_json: String,
    authenticator_data: String,
    signature: String,
    /// The user.id given when the passkey was added, which is the user's id.
    user_handle: Option<String>,
}

/// Answers like POST /auth/login. Once the passkey is known, each try counts toward its member's sign-in limit.
async fn sign_in_with_passkey(State(state): State<AppState>, jar: CookieJar, Json(body): Json<Assertion>) -> AppResult<(CookieJar, Json<SignIn>)> {
    let rp = rp(&state)?;
    let client_data = decode(&body.client_data_json)?;
    let challenge = webauthn::challenge(&client_data, "webauthn.get", &rp).map_err(refuse)?;
    take_challenge(&state.db, &challenge, None).await?;
    let found: Option<(Uuid, Uuid, Vec<u8>, i32, i64, String)> = sqlx::query_as(
        "select p.id, p.user_id, p.public_key, p.alg, p.sign_count, u.email::text from passkeys p join users u on u.id = p.user_id
         where p.credential_id = $1",
    )
    .bind(decode(&body.credential_id)?)
    .fetch_optional(&state.db)
    .await?;
    let Some((passkey_id, user_id, public_key, alg, stored_count, email)) = found else {
        return Err(AppError::BadRequest("That passkey isn't on a Splee account anymore. Sign in another way.".into()));
    };
    count_attempt(&state.db, &email).await?;
    let checked = check_assertion(&rp, &body, &client_data, user_id, &public_key, alg, stored_count);
    let authenticator_data = match checked {
        Ok(data) => data,
        Err(why) => {
            audit::record_account(&state.db, user_id, "user.sign_in_failed", json!({ "reason": "passkey" })).await;
            return Err(refuse(why));
        }
    };
    let count = i64::from(authenticator_data.sign_count);
    let mut tx = state.db.begin().await?;
    hold_user(&mut tx, user_id, None).await?;
    // Matched on the counter again, so of two sign-ins racing with the same count only one gets through, and on the row,
    // so a passkey removed in the meantime doesn't sign in.
    let used = sqlx::query(
        "update passkeys set sign_count = greatest(sign_count, $2), last_used_at = now()
         where id = $1 and ($2 = 0 or sign_count = 0 or sign_count < $2)",
    )
    .bind(passkey_id)
    .bind(count)
    .execute(&mut *tx)
    .await?;
    if used.rows_affected() == 0 {
        return Err(refuse("passkey gone"));
    }
    let verified = authenticator_data.user_verified;
    if !verified && two_factor::enabled(&state.db, user_id).await? {
        let two_factor_token = two_factor::start_login(&mut tx, user_id).await?;
        tx.commit().await?;
        return Ok((jar, Json(SignIn::Code { two_factor_token })));
    }
    sign_in(&state, jar, tx, user_id, &email, json!({ "method": "passkey", "user_verified": verified })).await
}

/// Everything about a sign-in that doesn't need the database. A counter that didn't move past the stored one, when both
/// count, means the passkey may have been copied.
fn check_assertion(
    rp: &Rp,
    body: &Assertion,
    client_data: &[u8],
    user_id: Uuid,
    public_key: &[u8],
    alg: i32,
    stored_count: i64,
) -> Result<webauthn::AuthData, &'static str> {
    let handle = body.user_handle.as_deref().filter(|h| !h.is_empty()).map(|h| URL_SAFE_NO_PAD.decode(h.trim_end_matches('=')));
    if handle.is_some_and(|h| h.ok().as_deref() != Some(&user_id.as_bytes()[..])) {
        return Err("user handle");
    }
    let raw = URL_SAFE_NO_PAD.decode(body.authenticator_data.trim_end_matches('=')).map_err(|_| "authenticator data")?;
    let data = webauthn::authenticator_data(&raw, rp)?;
    let signature = URL_SAFE_NO_PAD.decode(body.signature.trim_end_matches('=')).map_err(|_| "signature")?;
    if !webauthn::verify(public_key, alg, &raw, client_data, &signature) {
        return Err("signature");
    }
    let count = i64::from(data.sign_count);
    if count != 0 && stored_count != 0 && count <= stored_count {
        return Err("sign counter");
    }
    Ok(data)
}

#[cfg(test)]
mod tests;
