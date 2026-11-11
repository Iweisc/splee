//! Nostr as the project's own key: kind-1 notes (NIP-01) signed with BIP-340 Schnorr and sent to every relay in the project's
//! list over websockets. A note counts as posted once one relay answers OK true. Replies carry NIP-10 e and p tags, and search
//! uses NIP-50, which only the relays that list it in their NIP-11 document support. Relays are user input, so only wss:// at
//! public addresses is reached (see hooks::public_addrs).

use std::collections::HashSet;
use std::time::Duration;

use bech32::{Bech32, Hrp};
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio::sync::OnceCell;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::hooks;
use crate::mail::imap::{Stream, tls_connector};
use crate::social::Failure;

/// Relays take far longer notes, but a reply or post past this is no longer a note.
pub const CHARS: usize = 5_000;
pub const DEFAULT_RELAYS: [&str; 3] = ["wss://relay.damus.io", "wss://nos.lol", "wss://relay.nostr.band"];
pub const MAX_RELAYS: usize = 10;
/// Searched when none of the project's relays support NIP-50.
const SEARCH_RELAY: &str = "wss://relay.nostr.band";
/// Short in tests, which wait it out for a relay that never answers.
const TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 10 });
const SUBSCRIPTION: &str = "splee";

type Ws = WebSocketStream<Box<dyn Stream>>;

/// The secret key from an nsec or 64 hex characters.
pub fn secret_key(raw: &str) -> Option<SigningKey> {
    let raw = raw.trim();
    let bytes = match bech32::decode(raw) {
        Ok((hrp, data)) if hrp.as_str() == "nsec" => data,
        _ => hex::decode(raw).ok()?,
    };
    SigningKey::from_slice(&bytes).ok().filter(|_| bytes.len() == 32)
}

pub fn pubkey(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_bytes())
}

/// The public key as an npub, which people know the account by.
pub fn npub(pubkey: &str) -> String {
    encode("npub", &hex::decode(pubkey).unwrap_or_default())
}

fn encode(hrp: &str, data: &[u8]) -> String {
    bech32::encode::<Bech32>(Hrp::parse(hrp).expect("a fixed prefix"), data).unwrap_or_default()
}

/// The link Splee keeps for a note.
pub fn link(id: &str) -> String {
    format!("https://njump.me/{}", encode("note", &hex::decode(id).unwrap_or_default()))
}

/// The note's id, from its link on any Nostr site (njump.me, primal.net/e/…), a nostr: URI, or a bare note1 or nevent1.
pub fn note_id(link: &str) -> Option<String> {
    let link = link.trim();
    let last = match Url::parse(link) {
        Ok(url) if url.scheme() == "nostr" => url.path().to_string(),
        Ok(url) if url.scheme() == "https" => url.path_segments()?.rfind(|s| !s.is_empty())?.to_string(),
        Ok(_) => return None,
        Err(_) => link.to_string(),
    };
    let (hrp, data) = bech32::decode(&last).ok()?;
    let id = match hrp.as_str() {
        "note" => data,
        "nevent" => tlv_id(&data)?,
        _ => return None,
    };
    (id.len() == 32).then(|| hex::encode(id))
}

/// The event id in an nevent's type-length-value entries, which is type 0. Its relays and author aren't needed.
fn tlv_id(mut data: &[u8]) -> Option<Vec<u8>> {
    while let [kind, len, rest @ ..] = data {
        let value = rest.get(..*len as usize)?;
        if *kind == 0 {
            return Some(value.to_vec());
        }
        data = &rest[*len as usize..];
    }
    None
}

/// A relay's address as wss://host[:port][/path], from what the user typed.
pub fn relay_url(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_end_matches('/');
    let url = Url::parse(&if raw.contains("://") { raw.to_string() } else { format!("wss://{raw}") }).ok()?;
    let host = url.host_str()?;
    if url.scheme() != "wss" || !host.contains('.') || !url.username().is_empty() || url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    Some(url.as_str().trim_end_matches('/').to_string())
}

/// The relay list as the user typed it, one per line or comma-separated. Empty means the defaults.
pub fn relay_list(raw: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for r in raw.iter().flat_map(|r| r.split([',', '\n'])).map(str::trim).filter(|r| !r.is_empty()) {
        let url = relay_url(r).ok_or_else(|| format!("{r} isn't a relay address. Write it like wss://nos.lol"))?;
        if !out.contains(&url) {
            out.push(url);
        }
    }
    if out.len() > MAX_RELAYS {
        return Err(format!("Use up to {MAX_RELAYS} relays"));
    }
    Ok(if out.is_empty() { DEFAULT_RELAYS.map(String::from).to_vec() } else { out })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u64,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

impl Event {
    /// Its id as NIP-01 defines it: the SHA-256 of [0, pubkey, created_at, kind, tags, content] as compact JSON.
    fn hash(&self) -> [u8; 32] {
        let serialized = json!([0, self.pubkey, self.created_at, self.kind, self.tags, self.content]).to_string();
        Sha256::digest(serialized.as_bytes()).into()
    }

    pub fn sign(key: &SigningKey, created_at: i64, kind: u64, tags: Vec<Vec<String>>, content: &str) -> Self {
        let mut event = Self { id: String::new(), pubkey: pubkey(key), created_at, kind, tags, content: content.into(), sig: String::new() };
        let hash = event.hash();
        let aux: [u8; 32] = hex::decode(crate::crypto::random_token()).ok().and_then(|b| b.try_into().ok()).unwrap_or_default();
        let sig = key.sign_raw(&hash, &aux).expect("a 32-byte message always signs");
        event.id = hex::encode(hash);
        event.sig = hex::encode(sig.to_bytes());
        event
    }

    /// Whether the id matches the content and the signature the pubkey. Relays can send anything, so nothing is shown before.
    pub fn verify(&self) -> bool {
        let hash = self.hash();
        let Ok(key) = hex::decode(&self.pubkey).map_err(|_| ()).and_then(|k| VerifyingKey::from_slice(&k).map_err(|_| ())) else { return false };
        let sig = hex::decode(&self.sig).ok().and_then(|s| <[u8; 64]>::try_from(s).ok()).and_then(|s| Signature::from_bytes(&s).ok());
        hex::encode(hash) == self.id && sig.is_some_and(|sig| key.verify_raw(&hash, &sig).is_ok())
    }

    fn tag<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Vec<String>> + 'a {
        self.tags.iter().filter(move |t| t.first().is_some_and(|n| n == name) && t.len() >= 2)
    }

    /// The thread's first note: the one marked root, or the first e tag of an older unmarked reply, or this note itself.
    pub fn root(&self) -> String {
        let marked = self.tag("e").find(|t| t.get(3).is_some_and(|m| m == "root"));
        let unmarked = self.tag("e").find(|t| t.get(3).is_none_or(|m| m.is_empty()));
        marked.or(unmarked).map_or_else(|| self.id.clone(), |t| t[1].clone())
    }
}

/// NIP-10 tags for a reply to `parent`, found on `relay`: e tags marking the thread's root and the note answered, and p tags
/// for the note's author and everyone it tagged, so they all see the reply.
pub fn reply_tags(parent: &Event, relay: &str, own_pubkey: &str) -> Vec<Vec<String>> {
    let root = parent.root();
    let mut tags = vec![vec!["e".to_string(), root.clone(), relay.to_string(), "root".to_string()]];
    if root != parent.id {
        tags.push(vec!["e".into(), parent.id.clone(), relay.into(), "reply".into()]);
    }
    let mut people = vec![parent.pubkey.clone()];
    people.extend(parent.tag("p").map(|t| t[1].clone()));
    let mut seen = HashSet::new();
    for p in people.into_iter().filter(|p| p != own_pubkey && p.len() == 64) {
        if seen.insert(p.clone()) {
            tags.push(vec!["p".into(), p]);
        }
    }
    tags
}

/// How one relay took a note.
#[derive(Clone, Debug, PartialEq)]
enum Sent {
    Accepted,
    /// OK false, with the relay's reason, like "rate-limited: slow down" or "invalid: bad signature".
    Refused(String),
    /// Nothing went out.
    Unreachable(String),
    /// It went out and the relay didn't answer, so it may have kept it.
    NoAnswer,
}

#[derive(Clone, Copy)]
enum Dial {
    /// wss:// at public addresses only.
    Public,
    /// Plain ws:// and http:// to anywhere, for the fake relays in tests.
    #[cfg(test)]
    Local,
}

pub struct Nostr {
    key: SigningKey,
    pubkey: String,
    relays: Vec<String>,
    dial: Dial,
    /// The relays that support NIP-50, looked up once, and the one searched when none do.
    search_relays: OnceCell<Vec<String>>,
    search_fallback: String,
}

impl Nostr {
    pub fn new(key: SigningKey, relays: Vec<String>) -> Self {
        Self::with(key, relays, Dial::Public)
    }

    fn with(key: SigningKey, relays: Vec<String>, dial: Dial) -> Self {
        Self { pubkey: pubkey(&key), key, relays, dial, search_relays: OnceCell::new(), search_fallback: SEARCH_RELAY.into() }
    }

    async fn connect(&self, relay: &str) -> anyhow::Result<Ws> {
        let url = Url::parse(relay)?;
        let host = url.host_str().unwrap_or_default().to_string();
        let stream: Box<dyn Stream> = match self.dial {
            Dial::Public => {
                anyhow::ensure!(url.scheme() == "wss", "only wss:// relays are allowed");
                let addrs = tokio::time::timeout(TIMEOUT, hooks::public_addrs(&url)).await.map_err(|_| anyhow::anyhow!("timed out"))??;
                let tcp = tokio::time::timeout(TIMEOUT, TcpStream::connect(&addrs[..])).await.map_err(|_| anyhow::anyhow!("timed out"))??;
                let tls = tls_connector()?.connect(ServerName::try_from(host)?, tcp);
                Box::new(tokio::time::timeout(TIMEOUT, tls).await.map_err(|_| anyhow::anyhow!("timed out"))??)
            }
            #[cfg(test)]
            Dial::Local => Box::new(TcpStream::connect((host.as_str(), url.port().unwrap_or(80))).await?),
        };
        let (ws, _) = tokio::time::timeout(TIMEOUT, tokio_tungstenite::client_async(relay, stream)).await.map_err(|_| anyhow::anyhow!("timed out"))??;
        Ok(ws)
    }

    async fn send_to(&self, relay: &str, event: &Event) -> Sent {
        let mut ws = match self.connect(relay).await {
            Ok(ws) => ws,
            Err(e) => return Sent::Unreachable(format!("{e:#}")),
        };
        // Once the send starts, the relay may have the note, so a failure from here on is NoAnswer: a retry would sign a second one.
        if !matches!(tokio::time::timeout(TIMEOUT, ws.send(Message::text(json!(["EVENT", event]).to_string()))).await, Ok(Ok(()))) {
            return Sent::NoAnswer;
        }
        let answer = tokio::time::timeout(TIMEOUT, async {
            while let Some(Ok(msg)) = ws.next().await {
                let Ok(v) = serde_json::from_str::<Value>(msg.to_text().unwrap_or_default()) else { continue };
                if v[0] == "OK" && v[1] == event.id.as_str() {
                    let reason = v[3].as_str().unwrap_or_default().to_string();
                    return if v[2] == true { Sent::Accepted } else { Sent::Refused(reason) };
                }
            }
            Sent::NoAnswer
        })
        .await
        .unwrap_or(Sent::NoAnswer);
        // Closing politely shouldn't hold up an answer we already have.
        tokio::spawn(async move {
            let _ = tokio::time::timeout(TIMEOUT, ws.close(None)).await;
        });
        answer
    }

    /// Sends the note to every relay at once, and returns its link once one keeps it. When none did, it's about the account
    /// unless a relay may have kept it without saying so, or turned down the note itself.
    async fn publish(&self, event: &Event) -> Result<String, Failure> {
        let mut pending: futures::stream::FuturesUnordered<_> =
            self.relays.iter().enumerate().map(|(i, r)| async move { (i, self.send_to(r, event).await) }).collect();
        let mut sent = vec![Sent::NoAnswer; self.relays.len()];
        while let Some((i, answer)) = pending.next().await {
            if answer == Sent::Accepted {
                return Ok(link(&event.id));
            }
            sent[i] = answer;
        }
        if sent.contains(&Sent::NoAnswer) {
            return Err(Failure::Post("No relay confirmed the note, and it may have gone out anyway".into()));
        }
        let host = |r: &str| r.trim_start_matches("wss://").trim_start_matches("ws://").to_string();
        let said: Vec<String> = self.relays.iter().zip(&sent).map(|(r, s)| match s {
            Sent::Refused(m) => format!("{} said {}", host(r), if m.is_empty() { "no" } else { m }),
            Sent::Unreachable(e) => format!("couldn't reach {} ({e})", host(r)),
            _ => String::new(),
        }).collect();
        let about_note = sent.iter().any(|s| matches!(s, Sent::Refused(m) if m.starts_with("invalid:") || m.starts_with("pow:")));
        let message = format!("No relay took the note: {}", said.join(", "));
        Err(if about_note { Failure::Post(message) } else { Failure::Account(message) })
    }

    /// The verified notes a relay has for `filter`, until it says it sent them all.
    async fn query(&self, relay: &str, filter: &Value) -> anyhow::Result<Vec<Event>> {
        let mut ws = self.connect(relay).await?;
        ws.send(Message::text(json!(["REQ", SUBSCRIPTION, filter]).to_string())).await?;
        let (mut found, mut closed) = (Vec::new(), None);
        let _ = tokio::time::timeout(TIMEOUT, async {
            while let Some(Ok(msg)) = ws.next().await {
                let Ok(v) = serde_json::from_str::<Value>(msg.to_text().unwrap_or_default()) else { continue };
                match (v[0].as_str(), v[1] == SUBSCRIPTION) {
                    (Some("EVENT"), true) => found.extend(serde_json::from_value::<Event>(v[2].clone()).ok().filter(Event::verify)),
                    (Some("EOSE"), true) => break,
                    (Some("CLOSED"), true) => {
                        closed = Some(v[2].as_str().unwrap_or("closed").to_string());
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await;
        let _ = ws.close(None).await;
        match closed {
            Some(reason) if found.is_empty() => anyhow::bail!("{reason}"),
            _ => Ok(found),
        }
    }

    /// Asks every relay at once, newest first without repeats, each note with the relay it came from.
    async fn query_all(&self, relays: &[String], filter: &Value) -> Result<Vec<(Event, String)>, Failure> {
        let answers = futures::future::join_all(relays.iter().map(|r| self.query(r, filter))).await;
        if answers.iter().all(Result::is_err) {
            let errors: Vec<String> = answers.into_iter().filter_map(Result::err).map(|e| format!("{e:#}")).collect();
            return Err(Failure::Account(format!("Couldn't reach a relay ({})", errors.join(", "))));
        }
        let mut seen = HashSet::new();
        let mut out: Vec<(Event, String)> =
            relays.iter().zip(answers).flat_map(|(r, a)| a.unwrap_or_default().into_iter().map(move |e| (e, r.clone()))).filter(|(e, _)| seen.insert(e.id.clone())).collect();
        out.sort_by_key(|(e, _)| std::cmp::Reverse(e.created_at));
        Ok(out)
    }

    /// The relays to search: the project's that list NIP-50 in their NIP-11 document, or relay.nostr.band when none do.
    async fn search_relays(&self) -> &[String] {
        self.search_relays
            .get_or_init(|| async {
                let checks = futures::future::join_all(self.relays.iter().map(|r| self.searches(r))).await;
                let found: Vec<String> = self.relays.iter().zip(checks).filter(|(_, ok)| *ok).map(|(r, _)| r.clone()).collect();
                if found.is_empty() { vec![self.search_fallback.clone()] } else { found }
            })
            .await
    }

    async fn searches(&self, relay: &str) -> bool {
        let http = relay.replacen("wss://", "https://", 1).replacen("ws://", "http://", 1);
        let Ok(url) = Url::parse(&http) else { return false };
        let client = match self.dial {
            Dial::Public => match hooks::public_client(&url).await {
                Ok(c) => c,
                Err(_) => return false,
            },
            #[cfg(test)]
            Dial::Local => reqwest::Client::new(),
        };
        let info = client.get(url).header("Accept", "application/nostr+json").timeout(TIMEOUT).send().await;
        let Ok(info) = info else { return false };
        let info: Value = info.json().await.unwrap_or_default();
        info["supported_nips"].as_array().is_some_and(|nips| nips.iter().any(|n| n == 50))
    }

    /// Notes from the last month matching `query`, newest first.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Event>, Failure> {
        let since = (Utc::now() - chrono::Duration::days(30)).timestamp();
        let filter = json!({ "kinds": [1], "search": query, "limit": limit, "since": since });
        let relays = self.search_relays().await.to_vec();
        let mut found: Vec<Event> = self.query_all(&relays, &filter).await?.into_iter().map(|(e, _)| e).collect();
        found.truncate(limit);
        Ok(found)
    }

    /// Every relay this account reads from: its own and the ones it searches.
    async fn read_relays(&self) -> Vec<String> {
        let mut relays = self.relays.clone();
        relays.extend(self.search_relays().await.iter().filter(|r| !self.relays.contains(r)).cloned());
        relays
    }

    /// The note with this id and the relay it was found on.
    pub async fn note(&self, id: &str) -> Result<(Event, String), Failure> {
        let found = self.query_all(&self.read_relays().await, &json!({ "ids": [id] })).await?;
        found.into_iter().find(|(e, _)| e.id == id).ok_or_else(|| Failure::Post("None of the relays has that note. It may have been deleted".into()))
    }

    /// Up to `limit` replies to the note.
    pub async fn replies(&self, id: &str, limit: usize) -> Result<Vec<Event>, Failure> {
        let found = self.query_all(&self.read_relays().await, &json!({ "kinds": [1], "#e": [id], "limit": limit })).await?;
        Ok(found.into_iter().map(|(e, _)| e).take(limit).collect())
    }

    /// Replies to the note at `link` and returns the reply's link. The note is read first for its thread and people, and that
    /// read failing posted nothing, so it's about the account unless the note is gone.
    pub async fn reply(&self, link: &str, content: &str) -> Result<String, Failure> {
        let id = note_id(link).ok_or_else(|| Failure::Post("That isn't a link to a Nostr note".into()))?;
        let (parent, relay) = self.note(&id).await?;
        let tags = reply_tags(&parent, &relay, &self.pubkey);
        self.publish(&Event::sign(&self.key, Utc::now().timestamp(), 1, tags, content)).await
    }

    pub async fn post(&self, content: &str) -> Result<String, Failure> {
        self.publish(&Event::sign(&self.key, Utc::now().timestamp(), 1, Vec::new(), content)).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    use super::*;

    /// BIP-340's test vector 1 key, whose x-only public key is dff1d77f….
    const SECRET: &str = "b7e151628aed2a6abf7158809cf4f3c762e7160f38b4da56a784d9045190cfef";

    /// Signed by nostr-tools 2.23 (finalizeEvent) with SECRET, so it checks the serialization against another implementation,
    /// escapes and non-ASCII included.
    fn known_event() -> Event {
        serde_json::from_value(json!({
            "kind": 1, "created_at": 1762850000,
            "tags": [["e", "5c83da77af1dec6d7289834998ad7aafbd9e2191396d75ec3cc27f5a77226f36", "wss://nos.lol", "root"],
                     ["p", "dff1d77f2a671c5f36183726db2341be58feae1da2deced843240f7b502ba659"]],
            "content": "Warm it up \"slowly\"\nover four weeks, née ✓ \\ done",
            "pubkey": "dff1d77f2a671c5f36183726db2341be58feae1da2deced843240f7b502ba659",
            "id": "37c74dff6aefec08dcc473e074c57bd2d48840007fdbe7e256b8572ca8fce534",
            "sig": "05307cda2b7195be1ca0ebeba2c2e9d1a63fbdf6c6c8290f74990be96397e8dabf86fa5ca4852de8eed7a9e5ccd99650e2a24cc8428e018f9aba69ee13acddb0"
        }))
        .unwrap()
    }

    #[test]
    fn ids_and_signatures_match_nip01_and_bip340() {
        let known = known_event();
        assert!(known.verify());
        assert_eq!(hex::encode(known.hash()), known.id);
        // Any change breaks the id or the signature.
        assert!(!Event { content: "Warm it up".into(), ..known.clone() }.verify());
        assert!(!Event { sig: known.sig.replace('0', "1"), ..known.clone() }.verify());

        let key = secret_key(SECRET).unwrap();
        assert_eq!(pubkey(&key), known.pubkey);
        // BIP-340 test vector 1: a fixed message and aux_rand give a fixed signature.
        let msg = hex::decode("243f6a8885a308d313198a2e03707344a4093822299f31d0082efa98ec4e6c89").unwrap();
        let mut aux = [0u8; 32];
        aux[31] = 1;
        let sig = key.sign_raw(&msg, &aux).unwrap();
        assert_eq!(hex::encode(sig.to_bytes()), "6896bd60eeae296db48a229ff71dfe071bde413e6d43f917dc8dcf8c78de33418906d11ac976abccb20b091292bff4ea897efcb639ea871cfa95f6de339e4b0a");

        // Signing the same note again gives the same id and a signature that verifies.
        let ours = Event::sign(&key, known.created_at, 1, known.tags.clone(), &known.content);
        assert_eq!(ours.id, known.id);
        assert!(ours.verify());
    }

    #[test]
    fn keys_links_and_relays_get_one_form() {
        let nsec = "nsec1kls4zc52a54x40m3tzqfea8nca3ww9s08z6d5448snvsg5vselhsjv8uxn";
        assert_eq!(secret_key(nsec).map(|k| pubkey(&k)), secret_key(SECRET).map(|k| pubkey(&k)));
        assert_eq!(npub(&known_event().pubkey), "npub1mlcawle2vuw97dscxundkg6phev0atsa5t0vakzrys8hk5pt5evssm7a0a");
        for raw in ["npub1mlcawle2vuw97dscxundkg6phev0atsa5t0vakzrys8hk5pt5evssm7a0a", "abc", ""] {
            assert!(secret_key(raw).is_none(), "{raw}");
        }

        let id = known_event().id;
        let note = "note1xlr5mlm2alkq3hxyw0s8f3tm6t2gssqq0ld70cjkhptje28uu56qwxgm3j";
        assert_eq!(link(&id), format!("https://njump.me/{note}"));
        // From nostr-tools, which puts the author and relay before the id.
        let nevent = "nevent1qgsdluwh0u4xw8zlxcvrwfkmydqmuk874cw69hkwmppjgrmm2q46vkgpp4mhxue69uhkummn9ekx7mqqyqmuwn0ldth7czxuc3e7qax900fdfzzqqplahelz26u9wt9glnjngznx0ur";
        for raw in [format!("https://njump.me/{note}"), format!("nostr:{nevent}"), format!("https://primal.net/e/{note}/"), note.to_string()] {
            assert_eq!(note_id(&raw).as_deref(), Some(id.as_str()), "{raw}");
        }
        assert_eq!(note_id("https://njump.me/npub1mlcawle2vuw97dscxundkg6phev0atsa5t0vakzrys8hk5pt5evssm7a0a"), None);

        assert_eq!(relay_url("nos.lol/").as_deref(), Some("wss://nos.lol"));
        assert_eq!(relay_url("wss://relay.example.com:4443/inbox").as_deref(), Some("wss://relay.example.com:4443/inbox"));
        for raw in ["ws://nos.lol", "https://nos.lol", "localhost", "wss://a:b@nos.lol", "wss://nos.lol?x=1"] {
            assert_eq!(relay_url(raw), None, "{raw}");
        }
        assert_eq!(relay_list(&[]).unwrap(), DEFAULT_RELAYS);
        assert_eq!(relay_list(&["nos.lol, wss://nos.lol\nrelay.damus.io".into()]).unwrap(), ["wss://nos.lol", "wss://relay.damus.io"]);
        assert!(relay_list(&["ws://nos.lol".into()]).unwrap_err().contains("ws://nos.lol"));
    }

    #[test]
    fn replies_tag_the_root_the_note_and_its_people() {
        let own = pubkey(&secret_key(SECRET).unwrap());
        let ana = "a".repeat(64);
        let bo = "b".repeat(64);
        let note = |id: &str, tags: Vec<Vec<&str>>| Event {
            id: id.into(),
            pubkey: ana.clone(),
            created_at: 1,
            kind: 1,
            tags: tags.into_iter().map(|t| t.into_iter().map(String::from).collect()).collect(),
            content: String::new(),
            sig: String::new(),
        };
        let s = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // A top-level note is the root itself.
        assert_eq!(reply_tags(&note("r1", vec![]), "wss://nos.lol", &own), [s(&["e", "r1", "wss://nos.lol", "root"]), s(&["p", &ana])]);
        // A reply in a thread keeps the thread's root, marks the note answered, and tags everyone in it except this account.
        let reply = note("c2", vec![vec!["e", "r1", "", "root"], vec!["e", "c1", "", "reply"], vec!["p", &bo], vec!["p", &own], vec!["p", &ana]]);
        assert_eq!(reply_tags(&reply, "wss://nos.lol", &own), [
            s(&["e", "r1", "wss://nos.lol", "root"]),
            s(&["e", "c2", "wss://nos.lol", "reply"]),
            s(&["p", &ana]),
            s(&["p", &bo]),
        ]);
        // Older clients didn't mark them, and the first e tag is the root.
        assert_eq!(note("c3", vec![vec!["e", "r0"], vec!["e", "c1"]]).root(), "r0");
    }

    type Got = Arc<Mutex<Vec<Value>>>;

    /// A relay as NIP-01 describes it, answering EVENT with `answer` ("ok", "refuse", "silent" or "rate") and REQ with `notes`.
    /// It serves its NIP-11 document over plain HTTP when asked for one.
    async fn fake_relay(answer: &'static str, search: bool, notes: Vec<Event>) -> (String, Got) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let got = Got::default();
        let kept = got.clone();
        tokio::spawn(async move {
            loop {
                let (mut tcp, _) = listener.accept().await.unwrap();
                let (got, notes) = (kept.clone(), notes.clone());
                tokio::spawn(async move {
                    let mut head = [0u8; 1024];
                    let n = tcp.peek(&mut head).await.unwrap();
                    if String::from_utf8_lossy(&head[..n]).contains("application/nostr+json") {
                        let body = json!({ "name": "fake", "supported_nips": if search { json!([1, 11, 50]) } else { json!([1, 11]) } }).to_string();
                        let res = format!("HTTP/1.1 200 OK\r\ncontent-type: application/nostr+json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                        tcp.write_all(res.as_bytes()).await.unwrap();
                        return;
                    }
                    let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                    while let Some(Ok(msg)) = ws.next().await {
                        let Ok(v) = serde_json::from_str::<Value>(msg.to_text().unwrap_or_default()) else { continue };
                        got.lock().unwrap().push(v.clone());
                        let reply = match v[0].as_str().unwrap_or_default() {
                            "EVENT" => {
                                let event: Event = serde_json::from_value(v[1].clone()).unwrap();
                                assert!(event.verify());
                                match answer {
                                    "ok" => json!(["OK", event.id, true, ""]),
                                    "refuse" => json!(["OK", event.id, false, "invalid: note too long"]),
                                    "rate" => json!(["OK", event.id, false, "rate-limited: slow down"]),
                                    _ => continue,
                                }
                            }
                            "REQ" => {
                                let filter = &v[2];
                                if filter["search"].is_string() && !search {
                                    json!(["CLOSED", v[1], "unsupported: search"])
                                } else {
                                    let ids: Vec<&str> = filter["ids"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                                    for n in notes.iter().filter(|n| ids.is_empty() || ids.contains(&n.id.as_str())) {
                                        ws.send(Message::text(json!(["EVENT", v[1], n]).to_string())).await.unwrap();
                                    }
                                    json!(["EOSE", v[1]])
                                }
                            }
                            _ => continue,
                        };
                        ws.send(Message::text(reply.to_string())).await.unwrap();
                    }
                });
            }
        });
        (url, got)
    }

    /// The real fallback search relay would be on the internet, so it's one that's down.
    fn nostr(relays: &[&str]) -> Nostr {
        let n = Nostr::with(secret_key(SECRET).unwrap(), relays.iter().map(|r| r.to_string()).collect(), Dial::Local);
        Nostr { search_fallback: "ws://127.0.0.1:1".into(), ..n }
    }

    #[tokio::test]
    async fn a_note_is_posted_once_one_relay_keeps_it() {
        let (ok, got) = fake_relay("ok", false, vec![]).await;
        let (refuses, _) = fake_relay("refuse", false, vec![]).await;
        let (rate, _) = fake_relay("rate", false, vec![]).await;
        let (silent, _) = fake_relay("silent", false, vec![]).await;
        let down = "ws://127.0.0.1:1";

        let posted = nostr(&[&refuses, down, &ok]).post("Warm it up over four weeks").await.ok().unwrap();
        let sent = got.lock().unwrap()[0].clone();
        assert_eq!(sent[0], "EVENT");
        assert_eq!((sent[1]["kind"].as_u64(), sent[1]["content"].as_str(), sent[1]["tags"].clone()), (Some(1), Some("Warm it up over four weeks"), json!([])));
        assert_eq!(posted, link(sent[1]["id"].as_str().unwrap()));

        // Turned down for the note itself fails it. A rate limit or no relay at all is about the account, and the post waits.
        assert!(matches!(nostr(&[&refuses, down]).post("Hi").await, Err(Failure::Post(e)) if e.contains("invalid: note too long")));
        assert!(matches!(nostr(&[&rate, down]).post("Hi").await, Err(Failure::Account(e)) if e.contains("rate-limited")));
        assert!(matches!(nostr(&[down]).post("Hi").await, Err(Failure::Account(e)) if e.contains("couldn't reach")));
        // A relay that took it in and never answered may have kept it.
        assert!(matches!(nostr(&[&rate, &silent]).post("Hi").await, Err(Failure::Post(e)) if e.contains("may have gone out")));
        // One relay that keeps it is enough, without waiting for a silent one.
        let started = std::time::Instant::now();
        assert!(nostr(&[&silent, &ok]).post("Hi").await.is_ok());
        assert!(started.elapsed() < TIMEOUT);
    }

    #[tokio::test]
    async fn replies_find_the_note_first_and_search_only_asks_relays_that_support_it() {
        let ana = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let root = Event::sign(&ana, Utc::now().timestamp() - 60, 1, vec![], "How do you warm up a domain?");
        let (searching, searched) = fake_relay("ok", true, vec![root.clone()]).await;
        let (plain, asked) = fake_relay("ok", false, vec![root.clone()]).await;
        let n = nostr(&[&plain, &searching]);

        let found = n.search("warm up", 10).await.ok().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], root);
        assert!(searched.lock().unwrap().iter().any(|v| v[2]["search"] == "warm up"));
        assert!(asked.lock().unwrap().iter().all(|v| v[2]["search"].is_null()), "the relay without NIP-50 isn't searched");

        let reply = n.reply(&link(&root.id), "Four weeks, 20 a day").await.ok().unwrap();
        let sent = asked.lock().unwrap().iter().find(|v| v[0] == "EVENT").unwrap()[1].clone();
        assert_eq!(reply, link(sent["id"].as_str().unwrap()));
        assert_eq!(sent["tags"], json!([["e", root.id, plain, "root"], ["p", root.pubkey]]));

        let gone = n.reply(&link(&"0".repeat(64)), "Hi").await;
        assert!(matches!(gone, Err(Failure::Post(e)) if e.contains("deleted")));
        // A read that reaches no relay posted nothing, so the reply waits.
        assert!(matches!(nostr(&["ws://127.0.0.1:1"]).reply(&link(&root.id), "Hi").await, Err(Failure::Account(_))));
    }

    #[tokio::test]
    async fn private_relays_are_refused() {
        let key = secret_key(SECRET).unwrap();
        for relay in ["wss://127.0.0.1", "wss://10.0.0.5:7777", "ws://nos.lol"] {
            let n = Nostr::new(key.clone(), vec![relay.into()]);
            assert!(matches!(n.post("Hi").await, Err(Failure::Account(e)) if e.contains("couldn't reach")), "{relay}");
        }
    }
}
