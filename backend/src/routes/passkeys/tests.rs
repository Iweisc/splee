use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};

use super::*;
use crate::auth::hash_password;
use crate::routes::auth::tests::{Answer, ana, app_code, code, last_step, refused, signed_in, token, turn_on};
use crate::webauthn::P256_SPKI_PREFIX;

const PASSWORD: &str = "correct horse";
const UP: u8 = 0x01;
const UV: u8 = 0x04;
const AT: u8 = 0x40;

/// A software authenticator with one P-256 passkey, standing in for a phone, laptop or security key.
struct Authenticator {
    key: EcdsaKeyPair,
    credential_id: Vec<u8>,
}

impl Authenticator {
    fn new() -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let mut credential_id = vec![0u8; 16];
        rng.fill(&mut credential_id).unwrap();
        Authenticator { key, credential_id }
    }

    /// What getPublicKey() gives.
    fn spki(&self) -> Vec<u8> {
        [&P256_SPKI_PREFIX[..], self.key.public_key().as_ref()].concat()
    }
}

/// What the browser and the authenticator put together, each part one a test may get wrong.
struct Ceremony {
    challenge: String,
    origin: &'static str,
    rp_id: &'static str,
    flags: u8,
    count: u32,
}

/// Right for the test server's PUBLIC_URL, http://localhost:3424, with the user present and verified.
fn ceremony(challenge: String) -> Ceremony {
    Ceremony { challenge, origin: "http://localhost:3424", rp_id: "localhost", flags: UP | UV, count: 1 }
}

fn client_data(kind: &str, c: &Ceremony) -> Vec<u8> {
    json!({ "type": kind, "challenge": c.challenge, "origin": c.origin, "crossOrigin": false }).to_string().into_bytes()
}

fn authenticator_data(c: &Ceremony, attested: Option<&[u8]>) -> Vec<u8> {
    let mut data = digest(&SHA256, c.rp_id.as_bytes()).as_ref().to_vec();
    data.push(c.flags | if attested.is_some() { AT } else { 0 });
    data.extend(c.count.to_be_bytes());
    if let Some(id) = attested {
        data.extend([0u8; 16]);
        data.extend((id.len() as u16).to_be_bytes());
        data.extend(id);
        // Where the COSE key goes. Splee takes the key from getPublicKey() instead, so this is never read.
        data.extend([0xa5, 0x01, 0x02]);
    }
    data
}

fn attestation(a: &Authenticator, c: &Ceremony) -> NewPasskey {
    NewPasskey {
        name: "  MacBook ".into(),
        credential_id: b64url(&a.credential_id),
        client_data_json: b64url(&client_data("webauthn.create", c)),
        authenticator_data: b64url(&authenticator_data(c, Some(&a.credential_id))),
        public_key: b64url(&a.spki()),
        alg: ES256,
        transports: vec!["internal".into(), "hybrid".into(), "carrier-pigeon".into()],
    }
}

fn assertion(a: &Authenticator, c: &Ceremony, user_id: Uuid) -> Assertion {
    let client_data = client_data("webauthn.get", c);
    let data = authenticator_data(c, None);
    let signed = [&data[..], digest(&SHA256, &client_data).as_ref()].concat();
    let signature = a.key.sign(&SystemRandom::new(), &signed).unwrap();
    Assertion {
        credential_id: b64url(&a.credential_id),
        client_data_json: b64url(&client_data),
        authenticator_data: b64url(&data),
        signature: b64url(signature.as_ref()),
        user_handle: Some(b64url(user_id.as_bytes())),
    }
}

async fn options_for(state: &AppState, auth: &Auth, proof: &str) -> AppResult<Json<Value>> {
    add_options(State(state.clone()), auth.clone(), Json(ProofBody { proof: proof.into() })).await
}

async fn challenge_for(state: &AppState, auth: &Auth) -> String {
    let Json(options) = options_for(state, auth, PASSWORD).await.unwrap();
    options["challenge"].as_str().unwrap().to_string()
}

async fn sign_in_challenge(state: &AppState) -> String {
    let Json(options) = sign_in_options(State(state.clone())).await.unwrap();
    assert_eq!((&options["rpId"], &options["userVerification"]), (&json!("localhost"), &json!("preferred")));
    options["challenge"].as_str().unwrap().to_string()
}

async fn add_passkey(state: &AppState, auth: &Auth, body: NewPasskey) -> AppResult<Json<Vec<Passkey>>> {
    add(State(state.clone()), auth.clone(), Json(body)).await
}

/// Adds `a` with a new challenge, made in `change` first.
async fn add_with(state: &AppState, auth: &Auth, a: &Authenticator, change: impl FnOnce(&mut Ceremony)) -> AppResult<Json<Vec<Passkey>>> {
    let mut c = ceremony(challenge_for(state, auth).await);
    change(&mut c);
    add_passkey(state, auth, attestation(a, &c)).await
}

/// Adds `a` as an authenticator that hasn't counted yet.
async fn register(state: &AppState, auth: &Auth, a: &Authenticator) -> Vec<Passkey> {
    add_with(state, auth, a, |c| c.count = 0).await.unwrap().0
}

async fn use_passkey(state: &AppState, body: Assertion) -> Answer {
    sign_in_with_passkey(State(state.clone()), CookieJar::new(), Json(body)).await
}

/// Signs in with `a` on a new challenge, made in `change` first.
async fn sign_in_as(state: &AppState, a: &Authenticator, user_id: Uuid, change: impl FnOnce(&mut Ceremony)) -> Answer {
    let mut c = ceremony(sign_in_challenge(state).await);
    change(&mut c);
    use_passkey(state, assertion(a, &c, user_id)).await
}

fn could_not_check<T>(answer: &AppResult<T>) -> bool {
    matches!(answer, Err(AppError::BadRequest(message)) if message == COULD_NOT_CHECK)
}

fn expired<T>(answer: &AppResult<T>) -> bool {
    matches!(answer, Err(AppError::BadRequest(message)) if message == EXPIRED)
}

/// The stored counter and whether the passkey was ever used.
async fn counter(state: &AppState, a: &Authenticator) -> (i64, bool) {
    sqlx::query_as("select sign_count, last_used_at is not null from passkeys where credential_id = $1")
        .bind(&a.credential_id)
        .fetch_one(&state.db)
        .await
        .unwrap()
}

async fn rows(state: &AppState, table: &str) -> i64 {
    sqlx::query_scalar(&format!("select count(*) from {table}")).fetch_one(&state.db).await.unwrap()
}

async fn expire_challenges(state: &AppState) {
    sqlx::query("update passkey_challenges set expires_at = now() - interval '1 second'").execute(&state.db).await.unwrap();
}

async fn events(state: &AppState, auth: &Auth) -> Vec<(String, Value)> {
    sqlx::query_as("select action, details from audit_events where actor_user_id = $1 order by created_at")
        .bind(auth.user_id)
        .fetch_all(&state.db)
        .await
        .unwrap()
}

fn event(action: &str, details: Value) -> (String, Value) {
    (action.to_string(), details)
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_passkey_signs_in_and_moves_its_counter(db: PgPool) {
    let (state, auth) = ana(db).await;
    let Json(options) = options_for(&state, &auth, PASSWORD).await.unwrap();
    assert_eq!(options["rp"], json!({ "id": "localhost", "name": "Splee" }));
    assert_eq!(options["user"]["id"], b64url(auth.user_id.as_bytes()));
    assert_eq!(options["authenticatorSelection"]["residentKey"], "required");
    let challenge = options["challenge"].as_str().unwrap().to_string();
    let stored: Vec<String> = sqlx::query_scalar("select challenge_hash from passkey_challenges").fetch_all(&state.db).await.unwrap();
    assert_eq!(stored, [sha256_hex(&challenge)], "only the challenge's hash is kept");

    let a = Authenticator::new();
    let mut c = ceremony(challenge);
    c.count = 0;
    let Json(added) = add_passkey(&state, &auth, attestation(&a, &c)).await.unwrap();
    assert_eq!(added.len(), 1);
    assert_eq!((added[0].name.as_str(), added[0].user_verified, added[0].last_used_at), ("MacBook", true, None));
    let transports: Vec<String> = sqlx::query_scalar("select unnest(transports) from passkeys").fetch_all(&state.db).await.unwrap();
    assert_eq!(transports, ["internal", "hybrid"]);
    assert_eq!(counter(&state, &a).await, (0, false));

    assert!(signed_in(&sign_in_as(&state, &a, auth.user_id, |_| {}).await));
    assert_eq!(counter(&state, &a).await, (1, true));
    assert!(signed_in(&sign_in_as(&state, &a, auth.user_id, |c| c.count = 7).await));
    assert_eq!(counter(&state, &a).await.0, 7);
    let Json(listed) = list(State(state.clone()), auth.clone()).await.unwrap();
    assert!(listed[0].last_used_at.is_some());

    // An authenticator that never counts signs in every time, and so does a browser that leaves out the user handle.
    let b = Authenticator::new();
    register(&state, &auth, &b).await;
    assert!(signed_in(&sign_in_as(&state, &b, auth.user_id, |c| c.count = 0).await));
    let mut no_handle = assertion(&b, &Ceremony { count: 0, ..ceremony(sign_in_challenge(&state).await) }, auth.user_id);
    no_handle.user_handle = None;
    assert!(signed_in(&use_passkey(&state, no_handle).await));

    let added = event("user.passkey_added", json!({ "name": "MacBook" }));
    let used = event("user.signed_in", json!({ "method": "passkey", "user_verified": true }));
    assert_eq!(events(&state, &auth).await, [added.clone(), used.clone(), used.clone(), added, used.clone(), used]);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn adding_refuses_what_webauthn_refuses(db: PgPool) {
    let (state, auth) = ana(db).await;
    let a = Authenticator::new();
    assert!(could_not_check(&add_with(&state, &auth, &a, |c| c.origin = "https://evil.test").await));
    assert!(could_not_check(&add_with(&state, &auth, &a, |c| c.rp_id = "evil.test").await));
    assert!(could_not_check(&add_with(&state, &auth, &a, |c| c.flags = UV).await), "the user wasn't present");
    assert!(expired(&add_with(&state, &auth, &a, |c| c.challenge = "made-up".into()).await));
    let for_signing_in = sign_in_challenge(&state).await;
    assert!(expired(&add_with(&state, &auth, &a, |c| c.challenge = for_signing_in).await), "a sign-in's challenge");

    let c = ceremony(challenge_for(&state, &auth).await);
    let mut wrong_type = attestation(&a, &c);
    wrong_type.client_data_json = b64url(&client_data("webauthn.get", &c));
    assert!(could_not_check(&add_passkey(&state, &auth, wrong_type).await));
    let mut not_its_key = attestation(&a, &c);
    not_its_key.alg = RS256;
    assert!(could_not_check(&add_passkey(&state, &auth, not_its_key).await));
    assert_eq!(rows(&state, "passkeys").await, 0);

    let c = ceremony(challenge_for(&state, &auth).await);
    assert!(add_passkey(&state, &auth, attestation(&a, &c)).await.is_ok());
    let b = Authenticator::new();
    assert!(expired(&add_passkey(&state, &auth, attestation(&b, &c)).await), "a challenge works once");
    let c = ceremony(challenge_for(&state, &auth).await);
    expire_challenges(&state).await;
    assert!(expired(&add_passkey(&state, &auth, attestation(&b, &c)).await), "a challenge works for 5 minutes");
    let c = ceremony(challenge_for(&state, &auth).await);
    assert!(matches!(add_passkey(&state, &auth, attestation(&a, &c)).await, Err(AppError::Conflict(_))), "the same passkey twice");
    assert_eq!(rows(&state, "passkeys").await, 1);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn another_members_challenge_cannot_add_a_passkey(db: PgPool) {
    let (state, ana) = ana(db).await;
    let bo_id: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('bo@acme.test', $1) returning id")
        .bind(hash_password(PASSWORD).unwrap())
        .fetch_one(&state.db)
        .await
        .unwrap();
    sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(ana.org_id).bind(bo_id).execute(&state.db).await.unwrap();
    let bo = Auth { user_id: bo_id, org_id: ana.org_id, role: "member".into(), token_hash: String::new() };

    let bos = ceremony(challenge_for(&state, &bo).await);
    let a = Authenticator::new();
    assert!(expired(&add_passkey(&state, &ana, attestation(&a, &bos)).await));
    assert_eq!(rows(&state, "passkeys").await, 0);
    let Json(added) = add_passkey(&state, &bo, attestation(&a, &bos)).await.unwrap();
    assert_eq!(added.len(), 1, "it still works for Bo");
    let Json(anas) = list(State(state.clone()), ana.clone()).await.unwrap();
    assert!(anas.is_empty());
    assert!(signed_in(&sign_in_as(&state, &a, bo_id, |c| c.count = 2).await));
    assert!(refused(&sign_in_as(&state, &a, ana.user_id, |c| c.count = 3).await), "a user handle that isn't the passkey's user");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn adding_or_removing_a_passkey_takes_the_password_or_a_code(db: PgPool) {
    let (state, auth) = ana(db).await;
    assert!(matches!(options_for(&state, &auth, "wrong horse").await, Err(AppError::BadRequest(_))));
    assert_eq!(rows(&state, "passkey_challenges").await, 0, "no challenge without the proof");
    let a = Authenticator::new();
    assert!(expired(&add_passkey(&state, &auth, attestation(&a, &ceremony("made-up".into()))).await), "and nothing to add with");

    let id = register(&state, &auth, &a).await[0].id;
    let rename_to = |id: Uuid, name: &str| rename(State(state.clone()), auth.clone(), Path(id), Json(RenameBody { name: name.into() }));
    let Json(renamed) = rename_to(id, " Work laptop ").await.unwrap();
    assert_eq!(renamed[0].name, "Work laptop");
    assert!(matches!(rename_to(Uuid::new_v4(), "Mine now").await, Err(AppError::NotFound)));
    assert!(matches!(rename_to(id, &"x".repeat(61)).await, Err(AppError::BadRequest(_))));

    let remove_with = |proof: &str| remove(State(state.clone()), auth.clone(), Path(id), Json(ProofBody { proof: proof.into() }));
    assert!(matches!(remove_with("wrong horse").await, Err(AppError::BadRequest(_))));
    assert_eq!(rows(&state, "passkeys").await, 1);
    let Json(left) = remove_with(PASSWORD).await.unwrap();
    assert!(left.is_empty());
    assert!(refused(&sign_in_as(&state, &a, auth.user_id, |_| {}).await), "a removed passkey doesn't sign in");

    let actions: Vec<String> = events(&state, &auth).await.into_iter().map(|(action, _)| action).collect();
    assert_eq!(actions, ["user.passkey_added", "user.passkey_removed"]);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn signing_in_refuses_what_webauthn_refuses(db: PgPool) {
    let (state, auth) = ana(db).await;
    let (a, ana) = (Authenticator::new(), auth.user_id);
    register(&state, &auth, &a).await;

    assert!(refused(&sign_in_as(&state, &a, ana, |c| c.origin = "https://evil.test").await));
    assert!(refused(&sign_in_as(&state, &a, ana, |c| c.challenge = "made-up".into()).await));
    let for_adding = challenge_for(&state, &auth).await;
    assert!(refused(&sign_in_as(&state, &a, ana, |c| c.challenge = for_adding).await), "an add's challenge");
    let mut c = ceremony(sign_in_challenge(&state).await);
    expire_challenges(&state).await;
    assert!(refused(&use_passkey(&state, assertion(&a, &c, ana)).await), "a challenge works for 5 minutes");
    // Each of these is counted and recorded, now that the passkey says whose sign-in it is.
    assert!(refused(&sign_in_as(&state, &a, ana, |c| c.rp_id = "evil.test").await));
    assert!(refused(&sign_in_as(&state, &a, ana, |c| c.flags = UV).await), "the user wasn't present");
    let other = Authenticator::new();
    let mut forged = assertion(&other, &ceremony(sign_in_challenge(&state).await), ana);
    forged.credential_id = b64url(&a.credential_id);
    assert!(refused(&use_passkey(&state, forged).await), "signed with another key");
    assert_eq!(counter(&state, &a).await, (0, false));

    c = ceremony(sign_in_challenge(&state).await);
    c.count = 5;
    assert!(signed_in(&use_passkey(&state, assertion(&a, &c, ana)).await));
    c.count = 6;
    assert!(refused(&use_passkey(&state, assertion(&a, &c, ana)).await), "a challenge works once");
    assert!(refused(&sign_in_as(&state, &a, ana, |c| c.count = 3).await), "the counter went backwards");
    assert!(refused(&sign_in_as(&state, &a, ana, |c| c.count = 5).await), "or stood still");
    assert_eq!(counter(&state, &a).await.0, 5);
    assert!(signed_in(&sign_in_as(&state, &a, ana, |c| c.count = 6).await));

    let failed = events(&state, &auth).await.into_iter().filter(|(action, _)| action == "user.sign_in_failed").count();
    assert_eq!(failed, 5, "the wrong RP ID, presence, key and both counters");
    let attempts: i64 = sqlx::query_scalar("select count(*) from login_failures").fetch_one(&state.db).await.unwrap();
    assert_eq!(attempts, 0, "a sign-in clears them");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_verified_user_skips_the_code_and_an_unverified_one_is_asked_for_it(db: PgPool) {
    let (state, auth) = ana(db).await;
    let a = Authenticator::new();
    register(&state, &auth, &a).await;
    assert!(signed_in(&sign_in_as(&state, &a, auth.user_id, |c| c.flags = UP).await), "without two-factor, presence is enough");

    turn_on(&state, &auth).await;
    let verified = sign_in_as(&state, &a, auth.user_id, |c| c.count = 2).await;
    assert!(signed_in(&verified), "the PIN, fingerprint or face is the second factor");
    let pending = token(sign_in_as(&state, &a, auth.user_id, |c| (c.flags, c.count) = (UP, 3)).await);
    assert_eq!(counter(&state, &a).await.0, 3);
    let next = app_code(&state, &auth, last_step(&state, &auth).await + 1).await;
    assert!(signed_in(&code(&state, &pending, &next).await));

    let methods: Vec<Value> = events(&state, &auth).await.into_iter().filter(|(action, _)| action == "user.signed_in").map(|(_, d)| d).collect();
    let passkey = |verified: bool| json!({ "method": "passkey", "user_verified": verified });
    // turn_on signs in with the password once, and the code finishes a sign-in like a password's.
    assert_eq!(methods, [passkey(false), json!({}), passkey(true), json!({})]);
}
