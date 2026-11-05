use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

/// 32 random hex bytes. Used for session tokens, invite tokens and API keys.
pub fn random_token() -> String {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    format!("{}{}", hex::encode(a.as_bytes()), hex::encode(b.as_bytes()))
}

pub fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
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
