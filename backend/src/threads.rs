//! Threads through Meta's Threads API, with a long-lived user access token the user made for their own Meta app, with the
//! threads_basic, threads_content_publish, threads_read_replies and threads_keyword_search permissions. The token goes in the
//! query string, as Meta's docs show it, so errors drop the URL.

use std::time::Duration;

use chrono::Utc;
use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::Value;

use crate::social::Failure;

const API: &str = "https://graph.threads.net/v1.0";
/// Threads' limit on a post's text, in characters.
pub const CHARS: usize = 500;
const FIELDS: &str = "id,text,username,permalink,timestamp";

pub struct Threads {
    client: reqwest::Client,
    api: String,
    token: String,
    /// The account's Threads user id, or "me" before connecting has read it.
    user_id: String,
}

/// Who the token belongs to.
pub struct Account {
    pub id: String,
    pub username: String,
}

/// A post's link as Splee keeps it: its permalink plus the API's id for it, which a reply needs and the permalink doesn't
/// carry. Threads ignores the extra parameter, so the link still opens the post.
pub fn post_url(link: &str) -> Option<String> {
    let url = Url::parse(link.trim()).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if url.scheme() != "https" || !matches!(host.as_str(), "threads.net" | "www.threads.net" | "threads.com" | "www.threads.com") {
        return None;
    }
    let id = url.query_pairs().find(|(k, _)| k == "id")?.1.into_owned();
    permalink_url(&url, &id)
}

/// The API's id for the post at a link in the form post_url gives.
pub fn post_id(link: &str) -> Option<String> {
    let link = post_url(link)?;
    Some(link.rsplit_once("?id=")?.1.to_string())
}

fn permalink_url(url: &Url, id: &str) -> Option<String> {
    let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let [user, "post", code] = path.as_slice() else { return None };
    let user = user.strip_prefix('@')?.to_ascii_lowercase();
    let user_ok = !user.is_empty() && user.chars().all(|c| c.is_ascii_alphanumeric() || "._".contains(c));
    let code_ok = !code.is_empty() && code.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
    (user_ok && code_ok && digits(id)).then(|| format!("https://www.threads.com/@{user}/post/{code}?id={id}"))
}

/// The link Splee keeps for a post the API returned, from its permalink and id.
pub fn link(post: &Value) -> Option<String> {
    let url = Url::parse(post["permalink"].as_str()?).ok()?;
    permalink_url(&url, post["id"].as_str()?)
}

/// Ids go into paths, so they have to be the numbers Threads uses.
fn digits(id: &str) -> bool {
    (1..=30).contains(&id.len()) && id.chars().all(|c| c.is_ascii_digit())
}

impl Threads {
    pub fn new(token: &str, user_id: &str) -> Result<Self, Failure> {
        Self::at(API, token, user_id)
    }

    fn at(api: &str, token: &str, user_id: &str) -> Result<Self, Failure> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().map_err(|e| Failure::Account(e.to_string()))?;
        Ok(Self { client, api: api.into(), token: token.into(), user_id: if user_id.is_empty() { "me".into() } else { user_id.into() } })
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url, Failure> {
        let mut url = Url::parse_with_params(&format!("{}{path}", self.api), query).map_err(|e| Failure::Post(e.to_string()))?;
        url.query_pairs_mut().append_pair("access_token", &self.token);
        Ok(url)
    }

    /// An expired token and rate limits are about the account. Anything else, like a deleted post or a request that timed out
    /// and may have gone through, is about the post. Meta's permission errors stay with the post too, since one post can hit
    /// them when others don't, and an account failure would retry it every hour.
    async fn call(&self, request: RequestBuilder) -> Result<Value, Failure> {
        let res = request.send().await.map_err(|e| Failure::Post(format!("Threads didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        let code = body["error"]["code"].as_i64().unwrap_or_default();
        let said = body["error"]["message"].as_str().filter(|m| !m.is_empty()).map(|m| format!(" ({m})")).unwrap_or_default();
        Err(match status {
            s if s.is_success() => return Ok(body),
            // A server error may come after the post was saved, whatever the error says, so it never goes back in the queue.
            s if s.is_server_error() => Failure::Post(format!("Threads answered {s}{said}")),
            StatusCode::UNAUTHORIZED => Failure::Account(format!("Threads didn't accept the access token{said}. Make a new one and connect Threads again")),
            _ if code == 190 => Failure::Account(format!("Threads didn't accept the access token{said}. Make a new one and connect Threads again")),
            StatusCode::TOO_MANY_REQUESTS => Failure::Account("Threads is rate limiting the account".into()),
            _ if matches!(code, 4 | 17 | 32 | 613) => Failure::Account("Threads is rate limiting the account".into()),
            StatusCode::NOT_FOUND => Failure::Post("Threads couldn't find it. It may have been deleted".into()),
            s => Failure::Post(format!("Threads answered {s}{said}")),
        })
    }

    /// The token's account, which checks it.
    pub async fn me(&self) -> Result<Account, Failure> {
        let me = self.call(self.client.get(self.url("/me", &[("fields", "id,username")])?)).await?;
        match (me["id"].as_str().filter(|id| digits(id)), me["username"].as_str()) {
            (Some(id), Some(username)) => Ok(Account { id: id.into(), username: username.into() }),
            _ => Err(Failure::Account("Threads didn't say which account this is".into())),
        }
    }

    /// Posts from the last month matching `query`, newest first.
    pub async fn search(&self, query: &str, limit: u64) -> Result<Vec<Value>, Failure> {
        let since = (Utc::now() - chrono::Duration::days(30)).timestamp().to_string();
        let limit = limit.to_string();
        let query = [("q", query), ("search_type", "RECENT"), ("fields", FIELDS), ("limit", &limit), ("since", &since)];
        let found = self.call(self.client.get(self.url("/keyword_search", &query)?)).await?;
        Ok(found["data"].as_array().cloned().unwrap_or_default())
    }

    /// The post at a link in the form post_url gives.
    pub async fn post_at(&self, link: &str) -> Result<Value, Failure> {
        let id = post_id(link).ok_or_else(|| Failure::Post("That isn't a link to a Threads post".into()))?;
        self.call(self.client.get(self.url(&format!("/{id}"), &[("fields", FIELDS)])?)).await
    }

    /// The replies directly under a post.
    pub async fn replies(&self, post: &Value) -> Result<Vec<Value>, Failure> {
        let id = post["id"].as_str().filter(|id| digits(id)).ok_or_else(|| Failure::Post("Threads didn't give the post's id".into()))?;
        let found = self.call(self.client.get(self.url(&format!("/{id}/replies"), &[("fields", FIELDS)])?)).await?;
        Ok(found["data"].as_array().cloned().unwrap_or_default())
    }

    /// Posts `text`, as a reply to the post at `reply_to` when there is one, and returns its link. Threads makes a container
    /// first and publishes it second.
    pub async fn post(&self, reply_to: Option<&str>, text: &str) -> Result<String, Failure> {
        let mut query = vec![("media_type", "TEXT"), ("text", text)];
        let parent = reply_to.map(|link| post_id(link).ok_or_else(|| Failure::Post("That isn't a link to a Threads post".into()))).transpose()?;
        if let Some(id) = &parent {
            query.push(("reply_to_id", id));
        }
        let made = self.call(self.client.post(self.url(&format!("/{}/threads", self.user_id), &query)?)).await?;
        let container = made["id"].as_str().filter(|id| digits(id)).ok_or_else(|| Failure::Post("Threads didn't make the post".into()))?;
        let published = self.call(self.client.post(self.url(&format!("/{}/threads_publish", self.user_id), &[("creation_id", container)])?)).await?;
        // It's out now, so not getting its link back doesn't fail it.
        let Some(id) = published["id"].as_str().filter(|id| digits(id)) else { return Ok(String::new()) };
        let Ok(url) = self.url(&format!("/{id}"), &[("fields", "id,permalink")]) else { return Ok(String::new()) };
        Ok(self.call(self.client.get(url)).await.ok().and_then(|p| link(&p)).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, Query, State};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::json;

    use super::*;

    #[test]
    fn post_links_keep_the_id_a_reply_needs() {
        let canonical = Some("https://www.threads.com/@ana.b/post/DCx-9_q?id=18012345".to_string());
        assert_eq!(post_url("https://www.threads.net/@Ana.B/post/DCx-9_q?id=18012345&xmt=1"), canonical);
        assert_eq!(post_url("https://threads.com/@ana.b/post/DCx-9_q/?id=18012345"), canonical);
        assert_eq!(post_id("https://www.threads.com/@ana.b/post/DCx-9_q?id=18012345").as_deref(), Some("18012345"));
        assert_eq!(link(&json!({ "id": "18012345", "permalink": "https://www.threads.net/@ana.b/post/DCx-9_q" })), canonical);
        for link in [
            "https://www.threads.com/@ana/post/DCx9q",
            "https://www.threads.com/@ana/post/DCx9q?id=12a",
            "https://www.threads.com/@ana?id=1",
            "http://www.threads.com/@ana/post/DCx9q?id=1",
            "https://example.com/@ana/post/DCx9q?id=1",
        ] {
            assert_eq!(post_url(link), None, "{link}");
        }
    }

    type Sent = Arc<Mutex<Vec<String>>>;

    /// Threads as its docs describe it. The token picks whether the account works, and the text how posting goes.
    async fn fake_threads() -> (String, Sent) {
        fn token(q: &HashMap<String, String>) -> Option<(StatusCode, Json<Value>)> {
            (q["access_token"] != "GOOD").then(|| {
                let error = json!({ "error": { "message": "Error validating access token: Session has expired", "type": "OAuthException", "code": 190 } });
                (StatusCode::BAD_REQUEST, Json(error))
            })
        }
        async fn me(Query(q): Query<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            token(&q).unwrap_or((StatusCode::OK, Json(json!({ "id": "1789", "username": "acme" }))))
        }
        async fn search(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            assert_eq!((q["search_type"].as_str(), q["access_token"].as_str()), ("RECENT", "GOOD"));
            Json(json!({ "data": [{ "id": "42", "text": q["q"], "username": "bo", "permalink": "https://www.threads.net/@bo/post/AbC", "timestamp": "2026-11-01T10:00:00+0000" }] }))
        }
        async fn media(Path(id): Path<String>) -> (StatusCode, Json<Value>) {
            match id.as_str() {
                "42" | "77" => (StatusCode::OK, Json(json!({ "id": id, "permalink": format!("https://www.threads.net/@acme/post/P{id}") }))),
                _ => (StatusCode::BAD_REQUEST, Json(json!({ "error": { "message": "Unsupported get request", "type": "GraphMethodException", "code": 100 } }))),
            }
        }
        async fn create(State(sent): State<Sent>, Path(user): Path<String>, Query(q): Query<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            assert_eq!((user.as_str(), q["media_type"].as_str()), ("1789", "TEXT"));
            if let Some(refused) = token(&q) {
                return refused;
            }
            match q["text"].as_str() {
                "fast" => (StatusCode::BAD_REQUEST, Json(json!({ "error": { "message": "Application request limit reached", "code": 4 } }))),
                "down" => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": { "message": "Session has expired", "code": 190 } }))),
                "deleted" => (StatusCode::BAD_REQUEST, Json(json!({ "error": { "message": "The requested resource does not exist", "code": 100 } }))),
                text => {
                    sent.lock().unwrap().push(format!("{text} reply_to={}", q.get("reply_to_id").map_or("none", String::as_str)));
                    (StatusCode::OK, Json(json!({ "id": "5001" })))
                }
            }
        }
        async fn publish(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            assert_eq!(q["creation_id"], "5001");
            Json(json!({ "id": "77" }))
        }
        let sent = Sent::default();
        let app = Router::new()
            .route("/me", get(me))
            .route("/keyword_search", get(search))
            .route("/{id}", get(media))
            .route("/{user}/threads", post(create))
            .route("/{user}/threads_publish", post(publish))
            .with_state(sent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, sent)
    }

    #[tokio::test]
    async fn the_account_search_and_posting_work_and_failures_say_whose_problem_it_is() {
        let (base, sent) = fake_threads().await;
        let me = Threads::at(&base, "GOOD", "").unwrap().me().await.ok().unwrap();
        assert_eq!((me.id.as_str(), me.username.as_str()), ("1789", "acme"));
        assert!(matches!(Threads::at(&base, "OLD", "").unwrap().me().await, Err(Failure::Account(e)) if e.contains("connect Threads again")));

        let t = Threads::at(&base, "GOOD", "1789").unwrap();
        let found = t.search("cold email", 10).await.ok().unwrap();
        assert_eq!(link(&found[0]).as_deref(), Some("https://www.threads.com/@bo/post/AbC?id=42"));

        let url = t.post(Some("https://www.threads.com/@bo/post/AbC?id=42"), "Try this").await.ok().unwrap();
        assert_eq!(url, "https://www.threads.com/@acme/post/P77?id=77");
        t.post(None, "A lesson").await.ok().unwrap();
        assert_eq!(*sent.lock().unwrap(), ["Try this reply_to=42", "A lesson reply_to=none"]);

        assert!(matches!(t.post(Some("https://www.threads.com/@bo/post/AbC"), "Hi").await, Err(Failure::Post(_))));
        assert!(matches!(t.post(None, "deleted").await, Err(Failure::Post(e)) if e.contains("does not exist")));
        assert!(matches!(t.post(None, "fast").await, Err(Failure::Account(e)) if e.contains("rate limiting")));
        // It may have been saved before the error, so it fails even when the error names the token.
        assert!(matches!(t.post(None, "down").await, Err(Failure::Post(_))));
        assert!(matches!(Threads::at(&base, "OLD", "1789").unwrap().post(None, "Hi").await, Err(Failure::Account(_))));
    }
}
