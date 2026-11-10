//! Reddit through its official API, as the account behind a "script" app the user made at reddit.com/prefs/apps. Each use
//! signs in with the app's id and secret and the account's password. The token lasts an hour, longer than any one use.

use std::time::Duration;

use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::Value;

use crate::social::Failure;

const WWW: &str = "https://www.reddit.com";
const API: &str = "https://oauth.reddit.com";

pub struct Login {
    pub client_id: String,
    pub secret: String,
    pub username: String,
    pub password: String,
}

pub struct Reddit {
    client: reqwest::Client,
    token: String,
    api: String,
}

/// What a link points at: a post, or a comment in it.
#[derive(Debug, PartialEq)]
pub struct Target {
    pub post: String,
    pub comment: Option<String>,
}

impl Target {
    /// Reads www, old and new Reddit links, with or without the subreddit and title, and redd.it short links.
    pub fn parse(link: &str) -> Option<Self> {
        let link = link.trim();
        let rest = ["https://", "http://"].iter().fold(link, |s, p| s.strip_prefix(p).unwrap_or(s));
        let (host, path) = rest.split_once('/')?;
        let path: Vec<&str> = path.split(['?', '#']).next()?.split('/').filter(|s| !s.is_empty()).collect();
        let host = host.to_ascii_lowercase();
        let (post, comment) = if host == "redd.it" {
            (*path.first()?, None)
        } else if host == "reddit.com" || host.ends_with(".reddit.com") {
            let at = path.iter().position(|s| *s == "comments")?;
            // After the post id comes its title slug, or "comment" in new links, then the comment id.
            (*path.get(at + 1)?, path.get(at + 3).copied())
        } else {
            return None;
        };
        let id = |s: &str| (1..=12).contains(&s.len()) && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if !id(post) || comment.is_some_and(|c| !id(c)) {
            return None;
        }
        Some(Self { post: post.into(), comment: comment.map(Into::into) })
    }

    /// Reddit's name for it, which comments are posted under.
    pub fn thing(&self) -> String {
        match &self.comment {
            Some(c) => format!("t1_{c}"),
            None => format!("t3_{}", self.post),
        }
    }

    /// The one link Splee keeps for it. Reddit takes any title slug, so "_" stands in for one.
    pub fn url(&self) -> String {
        match &self.comment {
            Some(c) => format!("{WWW}/comments/{}/_/{c}/", self.post),
            None => format!("{WWW}/comments/{}/", self.post),
        }
    }
}

/// A subreddit name as people write it ("r/sales", "/r/sales/", "sales"), without the r/.
pub fn subreddit(raw: &str) -> Option<String> {
    let name = raw.trim().trim_matches('/');
    let name = name.strip_prefix("r/").unwrap_or(name);
    ((2..=21).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then(|| name.to_string())
}

impl Reddit {
    pub async fn sign_in(login: &Login) -> Result<Self, Failure> {
        Self::sign_in_at(WWW, API, login).await
    }

    async fn sign_in_at(www: &str, api: &str, login: &Login) -> Result<Self, Failure> {
        // Reddit turns away clients without a descriptive User-Agent.
        let agent = format!("splee/1.0 (by /u/{})", login.username);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(agent)
            .build()
            .map_err(|e| Failure::Account(e.to_string()))?;
        let res = client
            .post(format!("{www}/api/v1/access_token"))
            .basic_auth(&login.client_id, Some(&login.secret))
            .form(&[("grant_type", "password"), ("username", &login.username), ("password", &login.password)])
            .send()
            .await
            .map_err(|e| Failure::Account(format!("Reddit didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        match body["access_token"].as_str() {
            Some(token) if status.is_success() => Ok(Self { client, token: token.into(), api: api.into() }),
            _ => Err(Failure::Account(sign_in_error(status, body["error"].as_str()))),
        }
    }

    /// 401s and rate limits are about the account. Anything else, like a locked thread, a ban from the subreddit or a
    /// request that timed out and may have gone through, is about the post.
    async fn call(&self, request: RequestBuilder) -> Result<Value, Failure> {
        let res = request.bearer_auth(&self.token).send().await.map_err(|e| Failure::Post(format!("Reddit didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        let said = body["message"].as_str().or(body["reason"].as_str()).map(|m| format!(" ({m})")).unwrap_or_default();
        match status {
            s if s.is_success() => {}
            StatusCode::UNAUTHORIZED => return Err(Failure::Account(format!("Reddit signed the account out{said}. Connect Reddit again"))),
            StatusCode::TOO_MANY_REQUESTS => return Err(Failure::Account("Reddit is rate limiting the account".into())),
            StatusCode::FORBIDDEN => {
                return Err(Failure::Post(format!("Reddit refused it{said}. The account may be banned there, or the subreddit is private")));
            }
            StatusCode::NOT_FOUND => return Err(Failure::Post("Reddit couldn't find it. It may have been deleted".into())),
            s => return Err(Failure::Post(format!("Reddit answered {s}{said}"))),
        }
        match api_error(&body) {
            Some(failure) => Err(failure),
            None => Ok(body),
        }
    }

    /// The account's name as Reddit spells it, which checks the sign-in works for the API too.
    pub async fn me(&self) -> Result<String, Failure> {
        let me = self.call(self.client.get(format!("{}/api/v1/me", self.api))).await?;
        me["name"].as_str().map(str::to_string).ok_or_else(|| Failure::Account("Reddit didn't say which account this is".into()))
    }

    /// Posts from the last month, newest first, across Reddit or in one subreddit.
    pub async fn search(&self, query: &str, subreddit: Option<&str>, limit: u64) -> Result<Vec<Value>, Failure> {
        let (path, restrict) = match subreddit {
            Some(sr) => (format!("/r/{sr}/search"), "on"),
            None => ("/search".to_string(), "off"),
        };
        let limit = limit.to_string();
        let query = [("q", query), ("sort", "new"), ("t", "month"), ("limit", &limit), ("type", "link"), ("restrict_sr", restrict), ("raw_json", "1")];
        let url = Url::parse_with_params(&format!("{}{path}", self.api), query).map_err(|e| Failure::Post(e.to_string()))?;
        let list = self.call(self.client.get(url)).await?;
        Ok(children(&list))
    }

    /// A post and up to 20 of its top-level comments.
    pub async fn thread(&self, post: &str) -> Result<(Value, Vec<Value>), Failure> {
        let url = Url::parse_with_params(&format!("{}/comments/{post}", self.api), [("limit", "20"), ("depth", "1"), ("raw_json", "1")])
            .map_err(|e| Failure::Post(e.to_string()))?;
        let both = self.call(self.client.get(url)).await?;
        let post = children(&both[0]).into_iter().next().ok_or_else(|| Failure::Post("Reddit couldn't find that post".into()))?;
        Ok((post, children(&both[1])))
    }

    /// Replies to a post or comment and returns the comment's link.
    pub async fn comment(&self, target: &Target, text: &str) -> Result<String, Failure> {
        let thing = target.thing();
        let form = [("thing_id", thing.as_str()), ("text", text), ("api_type", "json")];
        let made = self.call(self.client.post(format!("{}/api/comment", self.api)).form(&form)).await?;
        let permalink = made["json"]["data"]["things"][0]["data"]["permalink"].as_str().unwrap_or_default();
        Ok(if permalink.is_empty() { String::new() } else { format!("{WWW}{permalink}") })
    }

    /// Submits a text post and returns its link.
    pub async fn submit(&self, subreddit: &str, title: &str, text: &str) -> Result<String, Failure> {
        let form = [("sr", subreddit), ("kind", "self"), ("title", title), ("text", text), ("api_type", "json")];
        let made = self.call(self.client.post(format!("{}/api/submit", self.api)).form(&form)).await?;
        Ok(made["json"]["data"]["url"].as_str().unwrap_or_default().to_string())
    }
}

/// The `data` of each child of a listing.
fn children(listing: &Value) -> Vec<Value> {
    listing["data"]["children"].as_array().into_iter().flatten().map(|c| c["data"].clone()).collect()
}

fn sign_in_error(status: StatusCode, error: Option<&str>) -> String {
    match (status, error) {
        (StatusCode::UNAUTHORIZED, _) => "Reddit didn't accept the app's client id and secret. Copy them again from reddit.com/prefs/apps".into(),
        (_, Some("invalid_grant")) => {
            "Wrong Reddit username or password. Accounts with two-factor sign-in can't connect this way, and the account has to be a developer of the app".into()
        }
        (_, Some("unauthorized_client" | "unsupported_grant_type")) => {
            "That app isn't a script app. Create one with the type set to script at reddit.com/prefs/apps".into()
        }
        (StatusCode::TOO_MANY_REQUESTS, _) => "Reddit is rate limiting sign-ins from this app. Try again in a few minutes".into(),
        (_, Some(error)) => format!("Reddit didn't let Splee sign in ({error})"),
        (status, None) => format!("Reddit didn't let Splee sign in ({status})"),
    }
}

/// With api_type=json Reddit answers 200 and lists what went wrong in json.errors, as [code, message, field].
fn api_error(body: &Value) -> Option<Failure> {
    let first = body["json"]["errors"].as_array()?.first()?;
    let code = first[0].as_str().unwrap_or_default();
    let message = first[1].as_str().filter(|m| !m.is_empty()).unwrap_or(code);
    Some(if code == "RATELIMIT" { Failure::Account(format!("Reddit asked the account to slow down: {message}")) } else { Failure::Post(format!("{message} ({code})")) })
}

#[cfg(test)]
mod tests {
    use axum::extract::Form;
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::json;
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn links_become_posts_and_comments() {
        let post = Target { post: "1abc2d".into(), comment: None };
        for link in [
            "https://www.reddit.com/r/sales/comments/1abc2d/how_do_you_find_leads/",
            "old.reddit.com/r/sales/comments/1abc2d/?sort=new",
            "https://reddit.com/comments/1abc2d",
            "https://redd.it/1abc2d",
        ] {
            assert_eq!(Target::parse(link).as_ref(), Some(&post), "{link}");
        }
        let comment = Target { post: "1abc2d".into(), comment: Some("kx9z0q1".into()) };
        for link in ["https://www.reddit.com/r/sales/comments/1abc2d/how_do_you_find_leads/kx9z0q1/", "https://www.reddit.com/r/sales/comments/1abc2d/comment/kx9z0q1/?context=3"] {
            assert_eq!(Target::parse(link).as_ref(), Some(&comment), "{link}");
        }
        for link in ["https://www.reddit.com/r/sales/", "https://example.com/r/sales/comments/1abc2d/", "https://www.reddit.com/r/sales/s/AbCdEf", "", "https://www.reddit.com/r/x/comments/AB_C/"] {
            assert_eq!(Target::parse(link), None, "{link}");
        }
        assert_eq!((comment.thing(), comment.url()), ("t1_kx9z0q1".into(), "https://www.reddit.com/comments/1abc2d/_/kx9z0q1/".into()));
        assert_eq!(Target::parse(&comment.url()), Some(comment));
        assert_eq!(post.thing(), "t3_1abc2d");
    }

    #[test]
    fn subreddit_names_lose_their_prefix() {
        for raw in ["sales", "r/sales", "/r/sales/", " sales "] {
            assert_eq!(subreddit(raw).as_deref(), Some("sales"), "{raw}");
        }
        for raw in ["", "s", "r/has space", "r/way_too_long_for_a_subreddit"] {
            assert_eq!(subreddit(raw), None, "{raw}");
        }
    }

    fn login(password: &str) -> Login {
        Login { client_id: "ID".into(), secret: "SECRET".into(), username: "ana".into(), password: password.into() }
    }

    /// Reddit as its docs describe it. The password picks how the sign-in goes, and the comment text how the comment does.
    async fn fake_reddit() -> String {
        async fn token(headers: HeaderMap, Form(form): Form<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            assert_eq!(headers["user-agent"], "splee/1.0 (by /u/ana)");
            if headers["authorization"] != "Basic SUQ6U0VDUkVU" {
                return (StatusCode::UNAUTHORIZED, Json(json!({ "message": "Unauthorized", "error": 401 })));
            }
            match form["password"].as_str() {
                "right" => (StatusCode::OK, Json(json!({ "access_token": "TOKEN", "token_type": "bearer", "expires_in": 3600 }))),
                "web-app" => (StatusCode::OK, Json(json!({ "error": "unauthorized_client" }))),
                _ => (StatusCode::OK, Json(json!({ "error": "invalid_grant" }))),
            }
        }
        async fn comment(headers: HeaderMap, Form(form): Form<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            assert_eq!(headers["authorization"], "Bearer TOKEN");
            match form["text"].as_str() {
                "locked" => (StatusCode::OK, Json(json!({ "json": { "errors": [["THREAD_LOCKED", "that thread is locked", "parent"]] } }))),
                "fast" => (StatusCode::OK, Json(json!({ "json": { "errors": [["RATELIMIT", "take a break for 5 minutes", "ratelimit"]] } }))),
                "banned" => (StatusCode::FORBIDDEN, Json(json!({ "message": "Forbidden", "error": 403 }))),
                _ => {
                    assert_eq!(form["thing_id"], "t3_1abc2d");
                    let thing = json!({ "kind": "t1", "data": { "id": "c1", "permalink": "/r/sales/comments/1abc2d/x/c1/" } });
                    (StatusCode::OK, Json(json!({ "json": { "errors": [], "data": { "things": [thing] } } })))
                }
            }
        }
        async fn submit(Form(form): Form<HashMap<String, String>>) -> Json<Value> {
            assert_eq!((form["sr"].as_str(), form["kind"].as_str(), form["title"].as_str()), ("sales", "self", "A question"));
            Json(json!({ "json": { "errors": [], "data": { "url": "https://www.reddit.com/r/sales/comments/2xyz/a_question/" } } }))
        }
        let app = Router::new()
            .route("/api/v1/access_token", post(token))
            .route("/api/v1/me", get(|| async { Json(json!({ "name": "Ana" })) }))
            .route("/api/comment", post(comment))
            .route("/api/submit", post(submit));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    #[tokio::test]
    async fn sign_in_failures_say_what_to_fix() {
        let base = fake_reddit().await;
        let fails = |login: Login| {
            let base = base.clone();
            async move {
                match Reddit::sign_in_at(&base, &base, &login).await {
                    Err(Failure::Account(message)) => message,
                    _ => panic!("signed in"),
                }
            }
        };
        assert!(fails(login("wrong")).await.starts_with("Wrong Reddit username or password"));
        assert!(fails(login("web-app")).await.starts_with("That app isn't a script app"));
        assert!(fails(Login { secret: "OTHER".into(), ..login("right") }).await.contains("client id and secret"));
        let reddit = Reddit::sign_in_at(&base, &base, &login("right")).await.ok().unwrap();
        assert_eq!(reddit.me().await.ok().unwrap(), "Ana");
    }

    #[tokio::test]
    async fn posting_returns_the_link_or_whose_problem_it_is() {
        let base = fake_reddit().await;
        let reddit = Reddit::sign_in_at(&base, &base, &login("right")).await.ok().unwrap();
        let post = Target { post: "1abc2d".into(), comment: None };
        assert_eq!(reddit.comment(&post, "Helpful answer").await.ok().unwrap(), "https://www.reddit.com/r/sales/comments/1abc2d/x/c1/");
        assert!(matches!(reddit.comment(&post, "locked").await, Err(Failure::Post(m)) if m == "that thread is locked (THREAD_LOCKED)"));
        assert!(matches!(reddit.comment(&post, "fast").await, Err(Failure::Account(_))));
        assert!(matches!(reddit.comment(&post, "banned").await, Err(Failure::Post(m)) if m.contains("banned")));
        assert_eq!(reddit.submit("sales", "A question", "Body").await.ok().unwrap(), "https://www.reddit.com/r/sales/comments/2xyz/a_question/");
    }
}
