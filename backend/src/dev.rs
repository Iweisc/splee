//! DEV (dev.to) through the Forem API v1, with an API key from the user's DEV settings. Forem's API can read comments but not
//! write them, so Splee only publishes articles there.

use std::time::Duration;

use reqwest::{RequestBuilder, StatusCode};
use serde_json::{Value, json};

use crate::social::Failure;

const API: &str = "https://dev.to/api";
const ACCEPT: &str = "application/vnd.forem.api-v1+json";
/// Forem's limit on an article's title, in characters.
pub const TITLE_CHARS: usize = 128;
pub const MAX_TAGS: usize = 4;

/// Tags as Forem takes them: lowercase letters and digits only, at most 30 characters and 4 tags, without repeats.
pub fn tags(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in raw {
        let tag: String = tag.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
        if !tag.is_empty() && tag.chars().count() <= 30 && !out.contains(&tag) {
            out.push(tag);
        }
    }
    out.truncate(MAX_TAGS);
    out
}

pub struct Dev {
    client: reqwest::Client,
    api: String,
    key: String,
}

impl Dev {
    pub fn new(key: &str) -> Result<Self, Failure> {
        Self::at(API, key)
    }

    fn at(api: &str, key: &str) -> Result<Self, Failure> {
        // DEV turns away requests without a user agent.
        let client = reqwest::Client::builder().user_agent("Splee").timeout(Duration::from_secs(30)).build().map_err(|e| Failure::Account(e.to_string()))?;
        Ok(Self { client, api: api.into(), key: key.into() })
    }

    /// A bad key and rate limits are about the account. Anything else, like a title DEV turns down or a request that timed out
    /// and may have gone through, is about the post.
    async fn call(&self, request: RequestBuilder) -> Result<Value, Failure> {
        let res = request.header("api-key", &self.key).header("accept", ACCEPT).send().await;
        let res = res.map_err(|e| Failure::Post(format!("DEV didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        let said = body["error"].as_str().filter(|m| !m.is_empty()).map(|m| format!(" ({m})")).unwrap_or_default();
        Err(match status {
            s if s.is_success() => return Ok(body),
            // A server error may come after the article was saved, so it never goes back in the queue.
            s if s.is_server_error() => Failure::Post(format!("DEV answered {s}{said}")),
            StatusCode::UNAUTHORIZED => Failure::Account("DEV didn't accept the API key. Make a new one and connect DEV again".into()),
            StatusCode::FORBIDDEN => Failure::Account(format!("DEV won't let the account publish{said}")),
            StatusCode::TOO_MANY_REQUESTS => Failure::Account("DEV is rate limiting the account".into()),
            s => Failure::Post(format!("DEV answered {s}{said}")),
        })
    }

    /// The key's username, which checks the key.
    pub async fn me(&self) -> Result<String, Failure> {
        let me = self.call(self.client.get(format!("{}/users/me", self.api))).await?;
        me["username"].as_str().map(String::from).ok_or_else(|| Failure::Account("DEV didn't say which account this is".into()))
    }

    /// Publishes an article and returns its link.
    pub async fn publish(&self, title: &str, body: &str, tags: &[String]) -> Result<String, Failure> {
        let article = json!({ "article": { "title": title, "body_markdown": body, "published": true, "tags": tags.join(", ") } });
        let made = self.call(self.client.post(format!("{}/articles", self.api)).json(&article)).await?;
        // It's out now, so not getting its link back doesn't fail it.
        Ok(made["url"].as_str().unwrap_or_default().into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    use axum::{Json, Router};

    use super::*;

    #[test]
    fn tags_are_what_forem_takes() {
        let raw = ["Cold Email", "#sales", "sales", "", "b2b", "growth", "extra"].map(String::from);
        assert_eq!(tags(&raw), ["coldemail", "sales", "b2b", "growth"]);
    }

    type Sent = Arc<Mutex<Vec<Value>>>;

    /// Forem as its docs describe it. The key picks whether the account works, and the title how publishing goes.
    async fn fake_dev() -> (String, Sent) {
        fn refused(headers: &HeaderMap) -> Option<(StatusCode, Json<Value>)> {
            assert_eq!((headers["accept"].to_str().unwrap(), headers["user-agent"].to_str().unwrap()), (ACCEPT, "Splee"));
            (headers["api-key"] != "GOOD").then(|| (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized", "status": 401 }))))
        }
        async fn me(headers: HeaderMap) -> (StatusCode, Json<Value>) {
            refused(&headers).unwrap_or((StatusCode::OK, Json(json!({ "id": 1, "username": "acme" }))))
        }
        async fn publish(State(sent): State<Sent>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            if let Some(refused) = refused(&headers) {
                return refused;
            }
            match body["article"]["title"].as_str().unwrap() {
                "fast" => (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": "Rate limit reached, try again in 30 seconds", "status": 429 }))),
                "bad" => (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": "Tag \"no-dashes\" contains non-alphanumeric characters", "status": 422 }))),
                "down" => (StatusCode::BAD_GATEWAY, Json(json!({}))),
                _ => {
                    sent.lock().unwrap().push(body);
                    (StatusCode::CREATED, Json(json!({ "id": 5, "url": "https://dev.to/acme/warm-up-5a1" })))
                }
            }
        }
        let sent = Sent::default();
        let app = Router::new().route("/users/me", get(me)).route("/articles", post(publish)).with_state(sent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, sent)
    }

    #[tokio::test]
    async fn articles_go_out_and_failures_say_whose_problem_it_is() {
        let (base, sent) = fake_dev().await;
        assert_eq!(Dev::at(&base, "GOOD").unwrap().me().await.ok().unwrap(), "acme");
        assert!(matches!(Dev::at(&base, "OLD").unwrap().me().await, Err(Failure::Account(e)) if e.contains("connect DEV again")));

        let dev = Dev::at(&base, "GOOD").unwrap();
        let tags = ["email".to_string(), "sales".to_string()];
        assert_eq!(dev.publish("Warm up", "# Hi", &tags).await.ok().unwrap(), "https://dev.to/acme/warm-up-5a1");
        let article = sent.lock().unwrap()[0]["article"].clone();
        assert_eq!(article, json!({ "title": "Warm up", "body_markdown": "# Hi", "published": true, "tags": "email, sales" }));

        assert!(matches!(dev.publish("fast", "Hi", &[]).await, Err(Failure::Account(e)) if e.contains("rate limiting")));
        assert!(matches!(dev.publish("bad", "Hi", &[]).await, Err(Failure::Post(e)) if e.contains("non-alphanumeric")));
        assert!(matches!(dev.publish("down", "Hi", &[]).await, Err(Failure::Post(_))));
        assert!(matches!(Dev::at(&base, "OLD").unwrap().publish("Hi", "Hi", &[]).await, Err(Failure::Account(_))));
    }
}
