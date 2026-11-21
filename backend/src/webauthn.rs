//! Checks what a browser sends when a passkey is added or signs in (routes/passkeys.rs). Registrations ask for attestation
//! "none", and the browser hands over the new key as SPKI DER (AuthenticatorAttestationResponse.getPublicKey()) and the
//! authenticator data on its own (getAuthenticatorData()), so nothing here parses CBOR. Keys are ES256 or RS256, and ring
//! verifies both.

use reqwest::Url;
use ring::digest::{SHA256, digest};
use ring::signature::{self, UnparsedPublicKey, VerificationAlgorithm};
use serde::Deserialize;

/// COSE algorithm numbers.
pub const ES256: i32 = -7;
pub const RS256: i32 = -257;

const USER_PRESENT: u8 = 0x01;
const USER_VERIFIED: u8 = 0x04;
const ATTESTED_CREDENTIAL: u8 = 0x40;

/// A P-256 SubjectPublicKeyInfo up to its uncompressed point: id-ecPublicKey, prime256v1 and a 66-byte BIT STRING.
pub(crate) const P256_SPKI_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03,
    0x42, 0x00,
];
/// The rsaEncryption OID an RSA SubjectPublicKeyInfo's algorithm starts with.
const RSA_ENCRYPTION: [u8; 11] = [0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];

/// The relying party: PUBLIC_URL's origin, which the browser's client data must name, and its host, the RP ID.
pub struct Rp {
    pub id: String,
    pub origin: String,
}

impl Rp {
    pub fn new(public_url: &str) -> anyhow::Result<Rp> {
        let url = Url::parse(public_url)?;
        let id = url.host_str().ok_or_else(|| anyhow::anyhow!("PUBLIC_URL has no host"))?.to_string();
        Ok(Rp { id, origin: url.origin().ascii_serialization() })
    }
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(rename = "crossOrigin", default)]
    cross_origin: bool,
}

/// The challenge a clientDataJSON answers, once its type ("webauthn.create" or "webauthn.get") and origin are right.
/// The caller still has to find and use up that challenge.
pub fn challenge(client_data_json: &[u8], kind: &str, rp: &Rp) -> Result<String, &'static str> {
    let data: ClientData = serde_json::from_slice(client_data_json).map_err(|_| "client data")?;
    if data.kind != kind {
        return Err("type");
    }
    if data.origin != rp.origin || data.cross_origin {
        return Err("origin");
    }
    Ok(data.challenge)
}

pub struct AuthData {
    pub user_verified: bool,
    pub sign_count: u32,
    /// Only in a registration's.
    pub credential_id: Option<Vec<u8>>,
}

/// Reads authenticator data made for this RP ID with the user present.
pub fn authenticator_data(data: &[u8], rp: &Rp) -> Result<AuthData, &'static str> {
    if data.len() < 37 {
        return Err("authenticator data");
    }
    if data[..32] != *digest(&SHA256, rp.id.as_bytes()).as_ref() {
        return Err("RP ID");
    }
    let flags = data[32];
    if flags & USER_PRESENT == 0 {
        return Err("user presence");
    }
    let sign_count = u32::from_be_bytes([data[33], data[34], data[35], data[36]]);
    // A 16-byte AAGUID, the id's length and the id. The COSE key after it is what getPublicKey() already gave as SPKI.
    let credential_id = match flags & ATTESTED_CREDENTIAL {
        0 => None,
        _ => {
            let len = data.get(53..55).ok_or("authenticator data")?;
            let len = usize::from(u16::from_be_bytes([len[0], len[1]]));
            Some(data.get(55..55 + len).ok_or("authenticator data")?.to_vec())
        }
    };
    Ok(AuthData { user_verified: flags & USER_VERIFIED != 0, sign_count, credential_id })
}

/// One DER element: its tag, its contents and what follows it. Lengths up to 64 KB, plenty for an RSA key.
fn der(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = match first {
        0..=0x7f => (usize::from(first), rest),
        0x81 => (usize::from(*rest.first()?), rest.get(1..)?),
        0x82 => (usize::from(u16::from_be_bytes([*rest.first()?, *rest.get(1)?])), rest.get(2..)?),
        _ => return None,
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// The key in the form ring takes: the uncompressed point for ES256, the PKCS#1 RSAPublicKey for RS256.
fn raw_key(spki: &[u8], alg: i32) -> Option<&[u8]> {
    match alg {
        ES256 => spki.strip_prefix(&P256_SPKI_PREFIX).filter(|point| point.len() == 65 && point[0] == 0x04),
        RS256 => {
            let (0x30, spki, []) = der(spki)? else { return None };
            let (0x30, algorithm, rest) = der(spki)? else { return None };
            let (0x03, bits, []) = der(rest)? else { return None };
            if !algorithm.starts_with(&RSA_ENCRYPTION) {
                return None;
            }
            bits.strip_prefix(&[0u8])
        }
        _ => None,
    }
}

/// Whether a new passkey's key is one Splee can check signatures with, so one that could never sign in isn't saved.
pub fn usable_key(spki: &[u8], alg: i32) -> bool {
    raw_key(spki, alg).is_some()
}

/// Checks a sign-in's signature, which covers the authenticator data and the SHA-256 of the client data.
pub fn verify(spki: &[u8], alg: i32, authenticator_data: &[u8], client_data_json: &[u8], sig: &[u8]) -> bool {
    let Some(key) = raw_key(spki, alg) else { return false };
    let algorithm: &'static dyn VerificationAlgorithm = match alg {
        ES256 => &signature::ECDSA_P256_SHA256_ASN1,
        _ => &signature::RSA_PKCS1_2048_8192_SHA256,
    };
    let signed = [authenticator_data, digest(&SHA256, client_data_json).as_ref()].concat();
    UnparsedPublicKey::new(algorithm, key).verify(&signed, sig).is_ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn rp() -> Rp {
        Rp::new("https://app.splee.test/").unwrap()
    }

    #[test]
    fn the_rp_is_public_urls_origin_and_host() {
        let rp = rp();
        assert_eq!((rp.id.as_str(), rp.origin.as_str()), ("app.splee.test", "https://app.splee.test"));
        assert_eq!(Rp::new("http://localhost:3333").unwrap().origin, "http://localhost:3333");
    }

    #[test]
    fn client_data_must_be_the_right_type_from_this_origin() {
        let data = |kind: &str, origin: &str, cross: bool| json!({ "type": kind, "challenge": "abc", "origin": origin, "crossOrigin": cross }).to_string();
        let check = |json: String| challenge(json.as_bytes(), "webauthn.get", &rp());
        assert_eq!(check(data("webauthn.get", "https://app.splee.test", false)), Ok("abc".into()));
        assert_eq!(check(data("webauthn.create", "https://app.splee.test", false)), Err("type"));
        assert_eq!(check(data("webauthn.get", "https://evil.test", false)), Err("origin"));
        assert_eq!(check(data("webauthn.get", "https://app.splee.test", true)), Err("origin"));
        assert_eq!(check("not json".into()), Err("client data"));
    }

    #[test]
    fn authenticator_data_must_be_for_this_rp_with_the_user_present() {
        let data = |rp_id: &str, flags: u8| [digest(&SHA256, rp_id.as_bytes()).as_ref(), &[flags, 0, 0, 1, 2]].concat();
        let read = authenticator_data(&data("app.splee.test", USER_PRESENT | USER_VERIFIED), &rp()).unwrap();
        assert_eq!((read.user_verified, read.sign_count, read.credential_id), (true, 258, None));
        assert!(!authenticator_data(&data("app.splee.test", USER_PRESENT), &rp()).unwrap().user_verified);
        assert_eq!(authenticator_data(&data("splee.test", USER_PRESENT), &rp()).err(), Some("RP ID"));
        assert_eq!(authenticator_data(&data("app.splee.test", USER_VERIFIED), &rp()).err(), Some("user presence"));
        assert_eq!(authenticator_data(&data("app.splee.test", USER_PRESENT | ATTESTED_CREDENTIAL), &rp()).err(), Some("authenticator data"));
        assert_eq!(authenticator_data(&[0; 36], &rp()).err(), Some("authenticator data"));
    }

    #[test]
    fn an_rsa_key_is_read_out_of_its_spki() {
        let pkcs1 = [0x30, 0x06, 0x02, 0x01, 0x03, 0x02, 0x01, 0x01];
        let algorithm = [&RSA_ENCRYPTION[..], &[0x05, 0x00]].concat();
        let bits = [&[0x03, 9, 0x00][..], &pkcs1].concat();
        let body = [&[0x30, algorithm.len() as u8][..], &algorithm, &bits].concat();
        let spki = [&[0x30, body.len() as u8][..], &body].concat();
        assert_eq!(raw_key(&spki, RS256), Some(&pkcs1[..]));
        assert_eq!(raw_key(&spki, ES256), None);
        assert_eq!(raw_key(&spki[..spki.len() - 1], RS256), None);
        assert!(!usable_key(&spki, -8), "only ES256 and RS256");
    }
}
