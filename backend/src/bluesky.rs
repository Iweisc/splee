//! Bluesky through the AT Protocol, as the account a project connected with its handle and an app password. Each use signs in
//! with createSession at bsky.social, then talks to the account's own server (its PDS), which passes app.bsky calls on.

use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::{Value, json};

use crate::social::Failure;

const ENTRYWAY: &str = "https://bsky.social";
/// Bluesky's limit on a post's text, in graphemes, so an emoji with a skin tone counts once (see social::length).
pub const GRAPHEMES: usize = 300;

pub struct Login {
    pub handle: String,
    pub password: String,
}

pub struct Bluesky {
    client: reqwest::Client,
    token: String,
    did: String,
    /// The handle as Bluesky spells it, without the @.
    pub handle: String,
    pds: String,
}

/// A post, by its author's DID or handle and its record key.
#[derive(Debug, PartialEq)]
pub struct PostRef {
    pub author: String,
    pub rkey: String,
}

impl PostRef {
    /// Reads bsky.app post links and at:// URIs.
    pub fn parse(link: &str) -> Option<Self> {
        let link = link.trim();
        let (author, rkey) = match link.strip_prefix("at://") {
            Some(rest) => match rest.split('/').collect::<Vec<_>>().as_slice() {
                [author, "app.bsky.feed.post", rkey] => (*author, *rkey),
                _ => return None,
            },
            None => {
                let rest = ["https://", "http://"].iter().fold(link, |s, p| s.strip_prefix(p).unwrap_or(s));
                let (host, path) = rest.split_once('/')?;
                if !matches!(host.to_ascii_lowercase().as_str(), "bsky.app" | "www.bsky.app") {
                    return None;
                }
                match path.split(['?', '#']).next()?.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>().as_slice() {
                    ["profile", author, "post", rkey] => (*author, *rkey),
                    _ => return None,
                }
            }
        };
        let did = author.starts_with("did:") && author.len() > 8 && author.chars().all(|c| c.is_ascii_alphanumeric() || ":._-".contains(c));
        let handle = author.contains('.') && author.chars().all(|c| c.is_ascii_alphanumeric() || ".-".contains(c));
        let rkey_ok = (1..=512).contains(&rkey.len()) && rkey.chars().all(|c| c.is_ascii_alphanumeric() || "._~:-".contains(c));
        // Handles are case-insensitive, DIDs aren't.
        let author = if did { author.to_string() } else { author.to_ascii_lowercase() };
        ((did || handle) && rkey_ok).then(|| Self { author, rkey: rkey.into() })
    }

    pub fn uri(&self) -> String {
        format!("at://{}/app.bsky.feed.post/{}", self.author, self.rkey)
    }

    /// The one link Splee keeps for it. With a DID it keeps working after the author changes their handle.
    pub fn url(&self) -> String {
        format!("https://bsky.app/profile/{}/post/{}", self.author, self.rkey)
    }
}

/// Bluesky only makes a link clickable when a facet marks its bytes in the text. Only full https:// and http:// links count.
pub fn link_facets(text: &str) -> Vec<Value> {
    let mut facets = Vec::new();
    let mut at = 0;
    while let Some(found) = ["https://", "http://"].iter().filter_map(|p| text[at..].find(p)).min() {
        let start = at + found;
        let end = text[start..].find(char::is_whitespace).map_or(text.len(), |n| start + n);
        // A link at the end of a sentence or in brackets doesn't take the punctuation with it.
        let link = text[start..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '"', '\'']);
        if Url::parse(link).is_ok_and(|u| u.host_str().is_some()) {
            facets.push(json!({
                "index": { "byteStart": start, "byteEnd": start + link.len() },
                "features": [{ "$type": "app.bsky.richtext.facet#link", "uri": link }],
            }));
        }
        at = end;
    }
    facets
}

impl Bluesky {
    pub async fn sign_in(login: &Login) -> Result<Self, Failure> {
        Self::sign_in_at(ENTRYWAY, login).await
    }

    async fn sign_in_at(base: &str, login: &Login) -> Result<Self, Failure> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().map_err(|e| Failure::Account(e.to_string()))?;
        let res = client
            .post(format!("{base}/xrpc/com.atproto.server.createSession"))
            .json(&json!({ "identifier": login.handle, "password": login.password }))
            .send()
            .await
            .map_err(|e| Failure::Account(format!("Bluesky didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        match (body["accessJwt"].as_str(), body["did"].as_str()) {
            (Some(token), Some(did)) if status.is_success() => Ok(Self {
                client,
                token: token.into(),
                did: did.into(),
                handle: body["handle"].as_str().unwrap_or(&login.handle).into(),
                pds: pds(&body).unwrap_or_else(|| base.into()),
            }),
            _ => Err(Failure::Account(sign_in_error(status, &body))),
        }
    }

    /// 401s, expired sessions and rate limits are about the account. Anything else, like a deleted post or a request that
    /// timed out and may have gone through, is about the post.
    async fn call(&self, request: RequestBuilder) -> Result<Value, Failure> {
        let res = request.bearer_auth(&self.token).send().await.map_err(|e| Failure::Post(format!("Bluesky didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        let error = body["error"].as_str().unwrap_or_default();
        let said = body["message"].as_str().filter(|m| !m.is_empty()).or(Some(error).filter(|e| !e.is_empty())).map(|m| format!(" ({m})")).unwrap_or_default();
        Err(match status {
            s if s.is_success() => return Ok(body),
            StatusCode::UNAUTHORIZED => Failure::Account(format!("Bluesky signed the account out{said}. Connect Bluesky again")),
            _ if matches!(error, "ExpiredToken" | "InvalidToken" | "AccountTakedown") => {
                Failure::Account(format!("Bluesky signed the account out{said}. Connect Bluesky again"))
            }
            StatusCode::TOO_MANY_REQUESTS => Failure::Account("Bluesky is rate limiting the account".into()),
            StatusCode::NOT_FOUND => Failure::Post("Bluesky couldn't find it. It may have been deleted".into()),
            _ if error == "NotFound" => Failure::Post("Bluesky couldn't find it. It may have been deleted".into()),
            s => Failure::Post(format!("Bluesky answered {s}{said}")),
        })
    }

    fn get(&self, method: &str, query: &[(&str, &str)]) -> Result<RequestBuilder, Failure> {
        let url = Url::parse_with_params(&format!("{}/xrpc/{method}", self.pds), query).map_err(|e| Failure::Post(e.to_string()))?;
        Ok(self.client.get(url))
    }

    /// Posts from the last month, newest first. `query` takes Bluesky's search syntax.
    pub async fn search(&self, query: &str, limit: u64) -> Result<Vec<Value>, Failure> {
        let since = (Utc::now() - chrono::Duration::days(30)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let limit = limit.to_string();
        let found = self.call(self.get("app.bsky.feed.searchPosts", &[("q", query), ("sort", "latest"), ("limit", &limit), ("since", &since)])?).await?;
        Ok(found["posts"].as_array().cloned().unwrap_or_default())
    }

    /// A post and its direct replies, as app.bsky.feed.getPostThread gives them.
    pub async fn thread(&self, post: &PostRef) -> Result<Value, Failure> {
        let uri = self.resolve(post).await?;
        let found = self.call(self.get("app.bsky.feed.getPostThread", &[("uri", &uri), ("depth", "1"), ("parentHeight", "0")])?).await?;
        let thread = &found["thread"];
        match thread["$type"].as_str().unwrap_or_default() {
            "app.bsky.feed.defs#blockedPost" => Err(Failure::Post("The author blocks the account, or the account blocks them".into())),
            _ if !thread["post"]["cid"].is_string() => Err(Failure::Post("Bluesky couldn't find that post. It may have been deleted".into())),
            _ => Ok(thread.clone()),
        }
    }

    /// The post's at:// URI with its author's DID, which getPostThread needs.
    async fn resolve(&self, post: &PostRef) -> Result<String, Failure> {
        if post.author.starts_with("did:") {
            return Ok(post.uri());
        }
        let found = self.call(self.get("com.atproto.identity.resolveHandle", &[("handle", &post.author)])?).await?;
        let did = found["did"].as_str().ok_or_else(|| Failure::Post(format!("Bluesky doesn't know @{}", post.author)))?;
        Ok(PostRef { author: did.into(), rkey: post.rkey.clone() }.uri())
    }

    /// Posts `text`, as a reply to the post at `reply_to` when there is one, and returns the new post's link.
    pub async fn post(&self, reply_to: Option<&str>, text: &str) -> Result<String, Failure> {
        let mut record = json!({ "$type": "app.bsky.feed.post", "text": text, "createdAt": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true) });
        let facets = link_facets(text);
        if !facets.is_empty() {
            record["facets"] = facets.into();
        }
        if let Some(link) = reply_to {
            let target = PostRef::parse(link).ok_or_else(|| Failure::Post("That isn't a link to a Bluesky post".into()))?;
            let mut thread = self.thread(&target).await?;
            let post = thread["post"].take();
            if post["viewer"]["replyDisabled"] == true {
                return Err(Failure::Post("The author turned off replies to this post".into()));
            }
            // A reply names the post it answers and the post that started the thread, which is the same post for a top-level one.
            let parent = json!({ "uri": post["uri"], "cid": post["cid"] });
            let root = Some(&post["record"]["reply"]["root"]).filter(|r| r["cid"].is_string()).cloned().unwrap_or_else(|| parent.clone());
            record["reply"] = json!({ "root": root, "parent": parent });
        }
        let create = json!({ "repo": self.did, "collection": "app.bsky.feed.post", "record": record });
        let made = self.call(self.client.post(format!("{}/xrpc/com.atproto.repo.createRecord", self.pds)).json(&create)).await?;
        let rkey = made["uri"].as_str().and_then(PostRef::parse).map(|r| r.rkey).unwrap_or_default();
        Ok(if rkey.is_empty() { String::new() } else { format!("https://bsky.app/profile/{}/post/{rkey}", self.handle) })
    }
}

/// The account's own server from the session's DID document. Only Bluesky's hosts are taken, so the token goes nowhere else.
fn pds(session: &Value) -> Option<String> {
    let services = session["didDoc"]["service"].as_array()?;
    let endpoint = services.iter().find(|s| s["id"].as_str().is_some_and(|id| id.ends_with("#atproto_pds")))?["serviceEndpoint"].as_str()?;
    let url = Url::parse(endpoint).ok()?;
    let host = url.host_str()?;
    (url.scheme() == "https" && host.ends_with(".bsky.network")).then(|| endpoint.trim_end_matches('/').to_string())
}

fn sign_in_error(status: StatusCode, body: &Value) -> String {
    let error = body["error"].as_str().unwrap_or_default();
    match (status, error) {
        (_, "AuthFactorTokenRequired") => "Bluesky asked for an email code. Use an app password, not the account's own password".into(),
        (_, "AccountTakedown") => "Bluesky has suspended the account".into(),
        (StatusCode::TOO_MANY_REQUESTS, _) => "Bluesky is rate limiting sign-ins for the account. Try again in a few minutes".into(),
        (StatusCode::UNAUTHORIZED, _) | (_, "AuthenticationRequired") => {
            "Wrong Bluesky handle or app password. Make an app password at bsky.app/settings/app-passwords".into()
        }
        _ => format!("Bluesky didn't let Splee sign in ({})", body["message"].as_str().filter(|m| !m.is_empty()).unwrap_or(status.as_str())),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::{Query, State};
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn post_links_become_at_uris() {
        let did = PostRef { author: "did:plc:abc123".into(), rkey: "3kq2xyz".into() };
        for link in ["https://bsky.app/profile/did:plc:abc123/post/3kq2xyz", "bsky.app/profile/did:plc:abc123/post/3kq2xyz?ref=x", "at://did:plc:abc123/app.bsky.feed.post/3kq2xyz"] {
            assert_eq!(PostRef::parse(link).as_ref(), Some(&did), "{link}");
        }
        assert_eq!(did.uri(), "at://did:plc:abc123/app.bsky.feed.post/3kq2xyz");
        let handle = PostRef::parse("https://bsky.app/profile/Ana.bsky.social/post/3kq2xyz").unwrap();
        assert_eq!(handle.uri(), "at://ana.bsky.social/app.bsky.feed.post/3kq2xyz");
        for link in [
            "https://bsky.app/profile/ana.bsky.social",
            "https://example.com/profile/ana.bsky.social/post/3kq2xyz",
            "at://did:plc:abc123/app.bsky.feed.like/3kq2xyz",
            "https://bsky.app/profile/not a handle/post/3kq2xyz",
            "",
        ] {
            assert_eq!(PostRef::parse(link), None, "{link}");
        }
    }

    #[test]
    fn links_get_facets_over_their_bytes() {
        // "Café " is 6 bytes, so the link starts at byte 6.
        let facets = link_facets("Café https://acme.com/guide. And (http://b.io)");
        assert_eq!(facets.len(), 2);
        assert_eq!(facets[0]["index"], json!({ "byteStart": 6, "byteEnd": 28 }));
        assert_eq!(facets[0]["features"][0]["uri"], "https://acme.com/guide");
        assert_eq!(facets[1]["features"][0]["uri"], "http://b.io");
        assert!(link_facets("no links, just https:// alone").is_empty());
    }

    #[test]
    fn only_bluesky_hosts_are_taken_as_the_server() {
        let session = |endpoint: &str| json!({ "didDoc": { "service": [{ "id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": endpoint }] } });
        assert_eq!(pds(&session("https://morel.us-east.host.bsky.network")).as_deref(), Some("https://morel.us-east.host.bsky.network"));
        assert_eq!(pds(&session("https://evil.example.com")), None);
        assert_eq!(pds(&session("http://morel.us-east.host.bsky.network")), None);
    }

    type Posts = Arc<Mutex<Vec<Value>>>;

    /// Bluesky as its docs describe it. The password picks how the sign-in goes, and the post text how createRecord does.
    async fn fake_bluesky() -> (String, Posts) {
        async fn session(Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            match body["password"].as_str().unwrap() {
                "app-pass" => (StatusCode::OK, Json(json!({ "accessJwt": "JWT", "did": "did:plc:me", "handle": "acme.bsky.social" }))),
                "own-pass" => (StatusCode::UNAUTHORIZED, Json(json!({ "error": "AuthFactorTokenRequired", "message": "A sign in code has been sent" }))),
                _ => (StatusCode::UNAUTHORIZED, Json(json!({ "error": "AuthenticationRequired", "message": "Invalid identifier or password" }))),
            }
        }
        async fn thread(Query(q): Query<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            match q["uri"].as_str() {
                "at://did:plc:ana/app.bsky.feed.post/reply1" => (StatusCode::OK, Json(json!({ "thread": { "post": {
                    "uri": "at://did:plc:ana/app.bsky.feed.post/reply1", "cid": "cid-reply",
                    "record": { "text": "Me too", "reply": { "root": { "uri": "at://did:plc:bo/app.bsky.feed.post/root1", "cid": "cid-root" },
                                                            "parent": { "uri": "at://did:plc:bo/app.bsky.feed.post/root1", "cid": "cid-root" } } } } } }))),
                _ => (StatusCode::BAD_REQUEST, Json(json!({ "error": "NotFound", "message": "Post not found" }))),
            }
        }
        async fn resolve(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            assert_eq!(q["handle"], "ana.bsky.social");
            Json(json!({ "did": "did:plc:ana" }))
        }
        async fn create(State(posts): State<Posts>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            assert_eq!(headers["authorization"], "Bearer JWT");
            match body["record"]["text"].as_str().unwrap() {
                "expired" => (StatusCode::BAD_REQUEST, Json(json!({ "error": "ExpiredToken", "message": "Token has expired" }))),
                "fast" => (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": "RateLimitExceeded" }))),
                _ => {
                    posts.lock().unwrap().push(body);
                    (StatusCode::OK, Json(json!({ "uri": "at://did:plc:me/app.bsky.feed.post/new1", "cid": "cid-new" })))
                }
            }
        }
        let posts = Posts::default();
        let app = Router::new()
            .route("/xrpc/com.atproto.server.createSession", post(session))
            .route("/xrpc/app.bsky.feed.getPostThread", get(thread))
            .route("/xrpc/com.atproto.identity.resolveHandle", get(resolve))
            .route("/xrpc/com.atproto.repo.createRecord", post(create))
            .with_state(posts.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, posts)
    }

    fn login(password: &str) -> Login {
        Login { handle: "acme.bsky.social".into(), password: password.into() }
    }

    #[tokio::test]
    async fn sign_in_failures_say_what_to_fix() {
        let (base, _) = fake_bluesky().await;
        let fails = |password| {
            let base = base.clone();
            async move {
                match Bluesky::sign_in_at(&base, &login(password)).await {
                    Err(Failure::Account(message)) => message,
                    _ => panic!("signed in"),
                }
            }
        };
        assert!(fails("wrong").await.starts_with("Wrong Bluesky handle or app password"));
        assert!(fails("own-pass").await.contains("Use an app password"));
        assert_eq!(Bluesky::sign_in_at(&base, &login("app-pass")).await.ok().unwrap().handle, "acme.bsky.social");
    }

    #[tokio::test]
    async fn replies_name_the_thread_and_failures_say_whose_problem_it_is() {
        let (base, posts) = fake_bluesky().await;
        let bsky = Bluesky::sign_in_at(&base, &login("app-pass")).await.ok().unwrap();

        let url = bsky.post(Some("https://bsky.app/profile/ana.bsky.social/post/reply1"), "Try https://acme.com").await.ok().unwrap();
        assert_eq!(url, "https://bsky.app/profile/acme.bsky.social/post/new1");
        let sent = posts.lock().unwrap().pop().unwrap();
        assert_eq!((sent["repo"].as_str(), sent["collection"].as_str()), (Some("did:plc:me"), Some("app.bsky.feed.post")));
        assert_eq!(sent["record"]["reply"]["root"], json!({ "uri": "at://did:plc:bo/app.bsky.feed.post/root1", "cid": "cid-root" }));
        assert_eq!(sent["record"]["reply"]["parent"], json!({ "uri": "at://did:plc:ana/app.bsky.feed.post/reply1", "cid": "cid-reply" }));
        assert_eq!(sent["record"]["facets"][0]["index"], json!({ "byteStart": 4, "byteEnd": 20 }));

        bsky.post(None, "A lesson").await.ok().unwrap();
        assert!(posts.lock().unwrap().pop().unwrap()["record"].get("reply").is_none());

        assert!(matches!(bsky.post(Some("at://did:plc:ana/app.bsky.feed.post/gone1"), "Hi").await, Err(Failure::Post(m)) if m.contains("couldn't find")));
        assert!(matches!(bsky.post(None, "expired").await, Err(Failure::Account(m)) if m.contains("Connect Bluesky again")));
        assert!(matches!(bsky.post(None, "fast").await, Err(Failure::Account(m)) if m.contains("rate limiting")));
        assert!(matches!(bsky.post(Some("https://example.com/post/1"), "Hi").await, Err(Failure::Post(_))));
    }
}
