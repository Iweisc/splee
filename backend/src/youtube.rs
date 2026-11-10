//! YouTube through the Data API v3, as the channel of the Google account the user authorized for their own Google Cloud OAuth
//! client, with the youtube.force-ssl scope. Each use trades the refresh token for an access token, like Bluesky signs in.

use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::{Value, json};

use crate::social::Failure;

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const API: &str = "https://www.googleapis.com/youtube/v3";
/// YouTube's limit on a comment's text, in characters.
pub const CHARS: usize = 10_000;
/// The post failure for a video with comments off, which the read tool shows instead of an error.
pub const COMMENTS_OFF: &str = "The video has comments turned off";

pub struct Login {
    pub client_id: String,
    pub secret: String,
    pub refresh_token: String,
}

pub struct YouTube {
    client: reqwest::Client,
    api: String,
    token: String,
}

/// The channel comments go out as. `name` is its @handle, or its title when it has none.
pub struct Channel {
    pub id: String,
    pub name: String,
}

/// A video, or a comment under it, which a reply answers in the comment's thread.
#[derive(Debug, PartialEq)]
pub struct Target {
    pub video: String,
    pub comment: Option<String>,
}

impl Target {
    /// Reads watch, youtu.be, Shorts and live links. `lc` is how YouTube links to a comment.
    pub fn parse(link: &str) -> Option<Self> {
        let url = Url::parse(link.trim()).ok()?;
        let host = url.host_str()?.to_ascii_lowercase();
        let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        let param = |name: &str| url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
        let video = match (host.as_str(), path.as_slice()) {
            ("youtu.be", [id]) => id.to_string(),
            ("youtube.com" | "www.youtube.com" | "m.youtube.com", ["watch"]) => param("v")?,
            ("youtube.com" | "www.youtube.com" | "m.youtube.com", ["shorts" | "live", id]) => id.to_string(),
            _ => return None,
        };
        let video_ok = video.len() == 11 && video.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
        let comment = param("lc").filter(|c| !c.is_empty());
        let comment_ok = comment.as_deref().is_none_or(|c| c.len() <= 100 && c.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)));
        (url.scheme() == "https" && video_ok && comment_ok).then_some(Self { video, comment })
    }

    pub fn url(&self) -> String {
        match &self.comment {
            Some(c) => format!("https://www.youtube.com/watch?v={}&lc={c}", self.video),
            None => format!("https://www.youtube.com/watch?v={}", self.video),
        }
    }
}

impl YouTube {
    pub async fn sign_in(login: &Login) -> Result<Self, Failure> {
        Self::sign_in_at(TOKEN_URL, API, login).await
    }

    async fn sign_in_at(token_url: &str, api: &str, login: &Login) -> Result<Self, Failure> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().map_err(|e| Failure::Account(e.to_string()))?;
        let form = [("client_id", login.client_id.as_str()), ("client_secret", &login.secret), ("refresh_token", &login.refresh_token), ("grant_type", "refresh_token")];
        let res = client.post(token_url).form(&form).send().await.map_err(|e| Failure::Account(format!("Google didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        if let (true, Some(token)) = (status.is_success(), body["access_token"].as_str()) {
            return Ok(Self { client, api: api.into(), token: token.into() });
        }
        Err(Failure::Account(match body["error"].as_str().unwrap_or_default() {
            "invalid_grant" => "Google turned down the refresh token. It may have expired or been revoked. Make a new one and connect YouTube again".into(),
            "invalid_client" | "unauthorized_client" => "Google didn't accept the client id and secret. Copy them again from the OAuth client".into(),
            _ => format!("Google didn't let Splee sign in ({})", body["error_description"].as_str().filter(|m| !m.is_empty()).unwrap_or(status.as_str())),
        }))
    }

    /// 401s, a used-up quota and a token without the scope are about the account. Anything else, like a deleted video or a
    /// request that timed out and may have gone through, is about the post.
    async fn call(&self, request: RequestBuilder) -> Result<Value, Failure> {
        let res = request.bearer_auth(&self.token).send().await.map_err(|e| Failure::Post(format!("YouTube didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        let reason = body["error"]["errors"][0]["reason"].as_str().unwrap_or_default();
        let said = body["error"]["message"].as_str().filter(|m| !m.is_empty()).map(|m| format!(" ({m})")).unwrap_or_default();
        Err(match status {
            s if s.is_success() => return Ok(body),
            // A server error may come after the comment was saved, so it never goes back in the queue.
            s if s.is_server_error() => Failure::Post(format!("YouTube answered {s}{said}")),
            StatusCode::UNAUTHORIZED => Failure::Account(format!("YouTube signed the account out{said}. Connect YouTube again")),
            StatusCode::TOO_MANY_REQUESTS => Failure::Account("YouTube is rate limiting the account".into()),
            _ if matches!(reason, "quotaExceeded" | "dailyLimitExceeded" | "rateLimitExceeded" | "userRateLimitExceeded") => {
                Failure::Account("The Google Cloud project used up its YouTube API quota for today".into())
            }
            _ if reason == "insufficientPermissions" => {
                Failure::Account("The refresh token can't post comments. Make one with the youtube.force-ssl scope and connect YouTube again".into())
            }
            _ if reason == "commentsDisabled" => Failure::Post(COMMENTS_OFF.into()),
            StatusCode::NOT_FOUND => Failure::Post("YouTube couldn't find it. It may have been deleted".into()),
            s => Failure::Post(format!("YouTube answered {s}{said}")),
        })
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<RequestBuilder, Failure> {
        let url = Url::parse_with_params(&format!("{}{path}", self.api), query).map_err(|e| Failure::Post(e.to_string()))?;
        Ok(self.client.get(url))
    }

    /// The account's channel, which checks the token.
    pub async fn me(&self) -> Result<Channel, Failure> {
        let found = self.call(self.get("/channels", &[("part", "snippet"), ("mine", "true")])?).await?;
        let channel = &found["items"][0];
        let id = channel["id"].as_str().ok_or_else(|| Failure::Account("The Google account has no YouTube channel. Make one on youtube.com first".into()))?;
        let name = channel["snippet"]["customUrl"].as_str().or(channel["snippet"]["title"].as_str()).unwrap_or(id);
        Ok(Channel { id: id.into(), name: name.into() })
    }

    /// Videos from the last month matching `query`, most relevant first. Each search costs 100 of the project's 10,000 daily
    /// quota units.
    pub async fn search(&self, query: &str, limit: u64) -> Result<Vec<Value>, Failure> {
        let after = (Utc::now() - chrono::Duration::days(30)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let limit = limit.to_string();
        let query = [("part", "snippet"), ("type", "video"), ("q", query), ("order", "relevance"), ("publishedAfter", &after), ("maxResults", &limit)];
        let found = self.call(self.get("/search", &query)?).await?;
        Ok(found["items"].as_array().cloned().unwrap_or_default())
    }

    pub async fn video(&self, id: &str) -> Result<Value, Failure> {
        let found = self.call(self.get("/videos", &[("part", "snippet,statistics"), ("id", id)])?).await?;
        Some(found["items"][0].clone()).filter(Value::is_object).ok_or_else(|| Failure::Post("YouTube couldn't find that video. It may be deleted or private".into()))
    }

    /// The video's top comments as comment threads, most relevant first.
    pub async fn comments(&self, video: &str) -> Result<Vec<Value>, Failure> {
        let query = [("part", "snippet"), ("videoId", video), ("order", "relevance"), ("maxResults", "20"), ("textFormat", "plainText")];
        let found = self.call(self.get("/commentThreads", &query)?).await?;
        Ok(found["items"].as_array().cloned().unwrap_or_default())
    }

    /// Comments `text` on the video, or replies in the thread of the comment, and returns the new comment's link.
    pub async fn comment(&self, target: &Target, text: &str) -> Result<String, Failure> {
        let made = match &target.comment {
            // A reply's id is its thread's id, a dot and its own, and YouTube only takes the thread's.
            Some(comment) => {
                let parent = comment.split('.').next().unwrap_or(comment);
                let body = json!({ "snippet": { "parentId": parent, "textOriginal": text } });
                self.call(self.client.post(format!("{}/comments?part=snippet", self.api)).json(&body)).await?
            }
            None => {
                let body = json!({ "snippet": { "videoId": target.video, "topLevelComment": { "snippet": { "textOriginal": text } } } });
                self.call(self.client.post(format!("{}/commentThreads?part=snippet", self.api)).json(&body)).await?
            }
        };
        let id = made["id"].as_str().unwrap_or_default();
        Ok(if id.is_empty() { String::new() } else { Target { video: target.video.clone(), comment: Some(id.into()) }.url() })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    use axum::{Form, Json, Router};
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn video_and_comment_links_get_one_form() {
        let video = Target { video: "dQw4w9WgXcQ".into(), comment: None };
        for link in ["https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42s", "https://youtu.be/dQw4w9WgXcQ?si=abc", "https://m.youtube.com/shorts/dQw4w9WgXcQ"] {
            assert_eq!(Target::parse(link).as_ref(), Some(&video), "{link}");
        }
        assert_eq!(video.url(), "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        let comment = Target::parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ&lc=UgzAbC123.9xYz").unwrap();
        assert_eq!(comment.url(), "https://www.youtube.com/watch?v=dQw4w9WgXcQ&lc=UgzAbC123.9xYz");
        for link in ["https://www.youtube.com/watch?v=short", "https://www.youtube.com/@acme", "http://youtu.be/dQw4w9WgXcQ", "https://vimeo.com/dQw4w9WgXcQ"] {
            assert_eq!(Target::parse(link), None, "{link}");
        }
    }

    type Sent = Arc<Mutex<Vec<Value>>>;

    /// Google's token endpoint and the Data API as their docs describe them. The refresh token picks how signing in goes,
    /// and the comment text how posting does.
    async fn fake_youtube() -> (String, Sent) {
        async fn token(Form(form): Form<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            match (form["grant_type"].as_str(), form["client_secret"].as_str(), form["refresh_token"].as_str()) {
                (_, "wrong", _) => (StatusCode::UNAUTHORIZED, Json(json!({ "error": "invalid_client", "error_description": "Unauthorized" }))),
                ("refresh_token", _, "GOOD") => (StatusCode::OK, Json(json!({ "access_token": "ya29", "expires_in": 3599 }))),
                _ => (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_grant", "error_description": "Token has been expired or revoked." }))),
            }
        }
        async fn channels(headers: HeaderMap) -> Json<Value> {
            assert_eq!(headers["authorization"], "Bearer ya29");
            Json(json!({ "items": [{ "id": "UCacme", "snippet": { "title": "Acme", "customUrl": "@acme" } }] }))
        }
        fn google_error(status: StatusCode, reason: &str, message: &str) -> (StatusCode, Json<Value>) {
            (status, Json(json!({ "error": { "code": status.as_u16(), "message": message, "errors": [{ "reason": reason, "message": message }] } })))
        }
        async fn insert(State(sent): State<Sent>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            let text = body["snippet"]["textOriginal"].as_str().or(body["snippet"]["topLevelComment"]["snippet"]["textOriginal"].as_str()).unwrap();
            match text {
                "quota" => google_error(StatusCode::FORBIDDEN, "quotaExceeded", "The request cannot be completed because you have exceeded your quota."),
                "closed" => google_error(StatusCode::FORBIDDEN, "commentsDisabled", "The video has disabled comments."),
                "scope" => google_error(StatusCode::FORBIDDEN, "insufficientPermissions", "Request had insufficient authentication scopes."),
                "down" => google_error(StatusCode::SERVICE_UNAVAILABLE, "backendError", "Backend Error"),
                _ => {
                    let reply = body["snippet"]["parentId"].is_string();
                    sent.lock().unwrap().push(body);
                    (StatusCode::OK, Json(json!({ "id": if reply { "UgzP.new1" } else { "UgzNew1" } })))
                }
            }
        }
        let sent = Sent::default();
        let app = Router::new()
            .route("/token", post(token))
            .route("/channels", get(channels))
            .route("/commentThreads", post(insert))
            .route("/comments", post(insert))
            .with_state(sent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, sent)
    }

    fn login(secret: &str, refresh_token: &str) -> Login {
        Login { client_id: "123.apps.googleusercontent.com".into(), secret: secret.into(), refresh_token: refresh_token.into() }
    }

    #[tokio::test]
    async fn sign_in_failures_say_what_to_fix() {
        let (base, _) = fake_youtube().await;
        let fails = async |secret, refresh_token| match YouTube::sign_in_at(&format!("{base}/token"), &base, &login(secret, refresh_token)).await {
            Err(Failure::Account(message)) => message,
            _ => panic!("signed in"),
        };
        assert!(fails("s", "OLD").await.contains("connect YouTube again"));
        assert!(fails("wrong", "GOOD").await.contains("client id and secret"));
        let yt = YouTube::sign_in_at(&format!("{base}/token"), &base, &login("s", "GOOD")).await.ok().unwrap();
        let me = yt.me().await.ok().unwrap();
        assert_eq!((me.id.as_str(), me.name.as_str()), ("UCacme", "@acme"));
    }

    #[tokio::test]
    async fn comments_and_replies_go_out_and_failures_say_whose_problem_it_is() {
        let (base, sent) = fake_youtube().await;
        let yt = YouTube::sign_in_at(&format!("{base}/token"), &base, &login("s", "GOOD")).await.ok().unwrap();
        let video = Target::parse("https://youtu.be/dQw4w9WgXcQ").unwrap();

        assert_eq!(yt.comment(&video, "Try this").await.ok().unwrap(), "https://www.youtube.com/watch?v=dQw4w9WgXcQ&lc=UgzNew1");
        let reply = Target::parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ&lc=UgzP.child2").unwrap();
        assert_eq!(yt.comment(&reply, "Me too").await.ok().unwrap(), "https://www.youtube.com/watch?v=dQw4w9WgXcQ&lc=UgzP.new1");
        let sent = std::mem::take(&mut *sent.lock().unwrap());
        assert_eq!(sent[0]["snippet"]["videoId"], "dQw4w9WgXcQ");
        assert_eq!(sent[1]["snippet"], json!({ "parentId": "UgzP", "textOriginal": "Me too" }));

        assert!(matches!(yt.comment(&video, "closed").await, Err(Failure::Post(e)) if e.contains("comments turned off")));
        assert!(matches!(yt.comment(&video, "quota").await, Err(Failure::Account(e)) if e.contains("quota")));
        assert!(matches!(yt.comment(&video, "scope").await, Err(Failure::Account(e)) if e.contains("youtube.force-ssl")));
        assert!(matches!(yt.comment(&video, "down").await, Err(Failure::Post(_))));
    }
}
