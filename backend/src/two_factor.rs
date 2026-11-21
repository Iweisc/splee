//! Two-factor sign-in with an authenticator app: TOTP (RFC 6238 with HMAC-SHA1, 6 digits, 30-second steps) and one-time
//! backup codes. The routes are in routes/auth.rs, the tables in migration 0174.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use qrcode::QrCode;
use qrcode::render::svg;
use sha1::Sha1;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::crypto::{decrypt, encrypt, keyed_hash, random_token, sha256_hex};

const STEP_SECONDS: i64 = 30;
const BACKUP_CODES: usize = 10;
const BACKUP_PURPOSE: &str = "backup-code";
const PENDING_LOGIN_MINUTES: i64 = 5;

/// A new 160-bit key, the length RFC 4226 recommends, encrypted for two_factor.secret_enc, and its base32 for the app.
pub fn new_key(app_secret: &str) -> (String, String) {
    let mut key = [0u8; 20];
    OsRng.fill_bytes(&mut key);
    (encrypt(app_secret, &hex::encode(key)), base32(&key))
}

/// RFC 4648 base32 without padding, which is how authenticator apps take a key.
fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let (mut out, mut buffer, mut bits) = (String::new(), 0u32, 0);
    for &b in bytes {
        // Only the low `bits` bits are read, so the high ones may shift out.
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[(buffer >> bits) as usize & 31] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[(buffer << (5 - bits)) as usize & 31] as char);
    }
    out
}

/// The otpauth:// URI apps read from the QR code.
pub fn otpauth_uri(email: &str, secret: &str) -> String {
    let email: String = email
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~@".contains(&b) { char::from(b).to_string() } else { format!("%{b:02X}") })
        .collect();
    format!("otpauth://totp/Splee:{email}?secret={secret}&issuer=Splee&algorithm=SHA1&digits=6&period=30")
}

/// The URI as a QR code in an SVG data URL, for an <img>.
pub fn qr_data_url(uri: &str) -> anyhow::Result<String> {
    let svg = QrCode::new(uri.as_bytes())?.render::<svg::Color>().min_dimensions(200, 200).build();
    Ok(format!("data:image/svg+xml;base64,{}", STANDARD.encode(svg)))
}

pub(crate) fn code_at(key: &[u8], step: i64) -> u32 {
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(&step.to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = usize::from(hash[hash.len() - 1] & 0x0f);
    let bin = u32::from_be_bytes([hash[offset], hash[offset + 1], hash[offset + 2], hash[offset + 3]]) & 0x7fff_ffff;
    bin % 1_000_000
}

pub(crate) fn current_step() -> i64 {
    Utc::now().timestamp() / STEP_SECONDS
}

/// Codes are shown as "123 456" and backup codes as "abcde-fghij", and people type them either way.
fn compact(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_lowercase()
}

/// The step whose code `code` is, within one step of now so a phone clock up to 30 seconds off still works.
pub fn matching_step(app_secret: &str, secret_enc: &str, code: &str) -> anyhow::Result<Option<i64>> {
    let code = compact(code);
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    let key = hex::decode(decrypt(app_secret, secret_enc)?)?;
    let code: u32 = code.parse()?;
    let now = current_step();
    Ok((now - 1..=now + 1).find(|&step| code_at(&key, step) == code))
}

pub async fn enabled(db: &PgPool, user_id: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar("select exists (select 1 from two_factor where user_id = $1 and enabled_at is not null)")
        .bind(user_id)
        .fetch_one(db)
        .await
}

/// Takes a code from the member's app or one of their backup codes, each only once. An app code for a step at or before
/// the last one taken is refused, so a code someone saw over a shoulder is useless once it's been used.
pub async fn take_code(db: &PgPool, app_secret: &str, user_id: Uuid, code: &str) -> anyhow::Result<bool> {
    let secret_enc: Option<String> = sqlx::query_scalar("select secret_enc from two_factor where user_id = $1 and enabled_at is not null")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    let Some(secret_enc) = secret_enc else { return Ok(false) };
    if let Some(step) = matching_step(app_secret, &secret_enc, code)? {
        let taken = sqlx::query("update two_factor set last_step = $2 where user_id = $1 and last_step < $2")
            .bind(user_id)
            .bind(step)
            .execute(db)
            .await?;
        return Ok(taken.rows_affected() == 1);
    }
    let used = sqlx::query("update backup_codes set used_at = now() where user_id = $1 and code_hash = $2 and used_at is null")
        .bind(user_id)
        .bind(keyed_hash(app_secret, BACKUP_PURPOSE, &compact(code)))
        .execute(db)
        .await?;
    Ok(used.rows_affected() == 1)
}

/// Replaces the member's backup codes with ten new ones like "k7m2p-q9x4t", 50 random bits each. Only their keyed hashes
/// are kept, so these are shown once.
pub async fn replace_backup_codes(conn: &mut PgConnection, app_secret: &str, user_id: Uuid) -> sqlx::Result<Vec<String>> {
    sqlx::query("delete from backup_codes where user_id = $1").bind(user_id).execute(&mut *conn).await?;
    let mut codes = Vec::with_capacity(BACKUP_CODES);
    for _ in 0..BACKUP_CODES {
        let mut bytes = [0u8; 7];
        OsRng.fill_bytes(&mut bytes);
        let raw = base32(&bytes).to_lowercase();
        let code = format!("{}-{}", &raw[..5], &raw[5..10]);
        sqlx::query("insert into backup_codes (user_id, code_hash) values ($1, $2)")
            .bind(user_id)
            .bind(keyed_hash(app_secret, BACKUP_PURPOSE, &compact(&code)))
            .execute(&mut *conn)
            .await?;
        codes.push(code);
    }
    Ok(codes)
}

/// After a correct password for an account with two-factor on: a token the login page sends back with the code.
pub async fn start_login(conn: &mut PgConnection, user_id: Uuid) -> sqlx::Result<String> {
    sqlx::query("delete from pending_logins where expires_at < now()").execute(&mut *conn).await?;
    let token = random_token();
    sqlx::query("insert into pending_logins (token_hash, user_id, expires_at) values ($1, $2, $3)")
        .bind(sha256_hex(&token))
        .bind(user_id)
        .bind(Utc::now() + Duration::minutes(PENDING_LOGIN_MINUTES))
        .execute(&mut *conn)
        .await?;
    Ok(token)
}

/// The user and email a live pending sign-in is for.
pub async fn pending_login(db: &PgPool, token: &str) -> sqlx::Result<Option<(Uuid, String)>> {
    sqlx::query_as(
        "select p.user_id, u.email::text from pending_logins p join users u on u.id = p.user_id
         where p.token_hash = $1 and p.expires_at > now()",
    )
    .bind(sha256_hex(token))
    .fetch_optional(db)
    .await
}

/// Uses up a pending sign-in. False when another request already did.
pub async fn finish_login(db: impl sqlx::PgExecutor<'_>, token: &str) -> sqlx::Result<bool> {
    let done = sqlx::query("delete from pending_logins where token_hash = $1 and expires_at > now()").bind(sha256_hex(token)).execute(db).await?;
    Ok(done.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::{base32, code_at, compact, otpauth_uri};

    #[test]
    fn codes_match_rfc_6238() {
        // The SHA1 vectors from RFC 6238 appendix B, cut to the 6 digits apps show.
        let key = b"12345678901234567890";
        for (time, code) in [(59, 287_082), (1_111_111_109, 81_804), (1_234_567_890, 5_924), (2_000_000_000, 279_037)] {
            assert_eq!(code_at(key, time / 30), code, "at {time}");
        }
    }

    #[test]
    fn keys_and_uris_are_what_apps_read() {
        assert_eq!(base32(b"12345678901234567890"), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(base32(b"f"), "MY");
        assert_eq!(
            otpauth_uri("ana+x@acme.test", "ABC"),
            "otpauth://totp/Splee:ana%2Bx@acme.test?secret=ABC&issuer=Splee&algorithm=SHA1&digits=6&period=30"
        );
        assert_eq!(compact(" K7M2P-q9x4t "), "k7m2pq9x4t");
    }
}
