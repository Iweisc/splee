use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// 32 random hex bytes. Used for session tokens, invite tokens and API keys.
pub fn random_token() -> String {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    format!("{}{}", hex::encode(a.as_bytes()), hex::encode(b.as_bytes()))
}

pub fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// A hex HMAC-SHA256 of `input` keyed with APP_SECRET and `purpose`, for values that are matched but must never be stored,
/// like a website visitor's IP. Unlike a plain hash, nobody without the secret can test guesses against it.
pub fn keyed_hash(secret: &str, purpose: &str, input: &str) -> String {
    let mut mac = <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(format!("{purpose}.{input}").as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Whether `sig` is keyed_hash's output for `input`, compared in constant time.
pub fn verify_keyed_hash(secret: &str, purpose: &str, input: &str, sig: &str) -> bool {
    let Ok(sig) = hex::decode(sig) else { return false };
    let mut mac = <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(format!("{purpose}.{input}").as_bytes());
    mac.verify_slice(&sig).is_ok()
}

fn cipher(secret: &str) -> Aes256Gcm {
    let key = Sha256::digest(secret.as_bytes());
    Aes256Gcm::new_from_slice(&key).expect("sha256 output is 32 bytes")
}

/// Mailbox passwords are encrypted at rest with a key derived from APP_SECRET.
pub fn encrypt(secret: &str, plaintext: &str) -> String {
    let nonce_bytes: [u8; 12] = uuid::Uuid::new_v4().as_bytes()[..12].try_into().unwrap();
    let ciphertext = cipher(secret)
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .expect("aes-gcm encryption does not fail for in-memory input");
    let mut out = nonce_bytes.to_vec();
    out.extend(ciphertext);
    STANDARD.encode(out)
}

pub fn decrypt(secret: &str, encoded: &str) -> anyhow::Result<String> {
    if encoded.is_empty() {
        return Ok(String::new());
    }
    let raw = STANDARD.decode(encoded)?;
    anyhow::ensure!(raw.len() > 12, "ciphertext too short");
    let (nonce, ciphertext) = raw.split_at(12);
    let plain = cipher(secret)
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| anyhow::anyhow!("could not decrypt secret, was APP_SECRET changed?"))?;
    Ok(String::from_utf8(plain)?)
}

fn state_mac(secret: &str, purpose: &str, nonce: &str, user_id: Uuid, org_id: Uuid) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(format!("{purpose}.{nonce}.{user_id}.{org_id}").as_bytes());
    mac
}

/// The state of an OAuth sign-in Splee starts: the nonce and an HMAC binding it to what it's for, the user and the org.
/// The nonce's hash is stored with the user and org, and the callback deletes that row, so a state works once.
pub fn sign_state(secret: &str, purpose: &str, nonce: &str, user_id: Uuid, org_id: Uuid) -> String {
    format!("{nonce}.{}", hex::encode(state_mac(secret, purpose, nonce, user_id, org_id).finalize().into_bytes()))
}

/// The nonce of a state this server signed. The user and org are checked against the stored row's.
pub fn state_nonce(state: &str) -> Option<&str> {
    state.split_once('.').map(|(nonce, _)| nonce).filter(|n| !n.is_empty())
}

pub fn verify_state(secret: &str, purpose: &str, state: &str, user_id: Uuid, org_id: Uuid) -> bool {
    let Some((nonce, sig)) = state.split_once('.') else { return false };
    let Ok(sig) = hex::decode(sig) else { return false };
    state_mac(secret, purpose, nonce, user_id, org_id).verify_slice(&sig).is_ok()
}
