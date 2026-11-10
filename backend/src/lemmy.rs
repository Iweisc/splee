//! Lemmy through its HTTP API v3, signed in with the account's username and password. Login gives a JWT, which is kept and
//! used until the instance stops taking it, then traded for a new one. The instance is user input, so requests only go to a
//! public address (see hooks::public_client).

use std::sync::Mutex;

use chrono::{DateTime, NaiveDateTime, Utc};
use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::{Value, json};

use crate::hooks;
use crate::social::Failure;

/// Lemmy's limits, in characters, on a post's or comment's text and a post's title.
pub const CHARS: usize = 10_000;
pub const TITLE_CHARS: usize = 200;

pub struct Lemmy {
    client: reqwest::Client,
    base: String,
    username: String,
    password: String,
    /// Empty until the first login.
    jwt: Mutex<String>,
}

/// A post or comment on the account's instance, by the id the instance gives it. Other instances number them differently,
/// so only links on the account's own instance can be answered.
#[derive(Debug, PartialEq)]
pub struct Link {
    pub instance: String,
    pub comment: bool,
    pub id: u64,
}

impl Link {
    /// Reads https://{instance}/post/{id} and https://{instance}/comment/{id}.
    pub fn parse(link: &str) -> Option<Self> {
        let url = Url::parse(link.trim()).ok()?;
        let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        let (comment, id) = match path.as_slice() {
            ["post", id] => (false, id),
            ["comment", id] => (true, id),
            _ => return None,
        };
        let id = id.parse::<u64>().ok().filter(|id| *id > 0)?;
        (url.scheme() == "https" && url.host_str()?.contains('.')).then(|| Self { instance: url.origin().ascii_serialization(), comment, id })
    }

    pub fn url(&self) -> String {
        format!("{}/{}/{}", self.instance, if self.comment { "comment" } else { "post" }, self.id)
    }
}

/// A community as name@instance, from "!sales@lemmy.world", "sales@lemmy.world", a link to it, or a bare name on `home`, the
/// account's instance's host.
pub fn community(raw: &str, home: &str) -> Option<String> {
    let raw = raw.trim();
    let (name, host) = match Url::parse(raw) {
        Ok(url) if url.scheme() == "https" => match url.path_segments()?.filter(|s| !s.is_empty()).collect::<Vec<_>>().as_slice() {
            ["c", name] => match name.split_once('@') {
                Some((name, host)) => (name.to_string(), host.to_string()),
                None => (name.to_string(), url.host_str()?.to_string()),
            },
            _ => return None,
        },
        _ => {
            let raw = raw.trim_start_matches('!');
            match raw.split_once('@') {
                Some((name, host)) => (name.to_string(), host.to_string()),
                None => (raw.to_string(), home.to_string()),
            }
        }
    };
    let name_ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let host = host.to_ascii_lowercase();
    let host_ok = host.contains('.') && host.chars().all(|c| c.is_ascii_alphanumeric() || ".-".contains(c));
    (name_ok && host_ok).then(|| format!("{}@{host}", name.to_ascii_lowercase()))
}

/// Lemmy 0.19 writes times with a Z, older versions without one.
pub fn time(t: &Value) -> Option<DateTime<Utc>> {
    let t = t.as_str()?;
    DateTime::parse_from_rfc3339(t).map(|t| t.with_timezone(&Utc)).ok().or_else(|| NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%.f").ok().map(|t| t.and_utc()))
}

impl Lemmy {
    pub async fn new(instance: &str, username: &str, password: &str, jwt: &str) -> Result<Self, Failure> {
        let url = Url::parse(instance).map_err(|e| Failure::Account(e.to_string()))?;
        let client = hooks::public_client(&url).await.map_err(|e| Failure::Account(format!("Couldn't reach {instance} ({e:#})")))?;
        Ok(Self::with(client, instance, username, password, jwt))
    }

    fn with(client: reqwest::Client, instance: &str, username: &str, password: &str, jwt: &str) -> Self {
        Self { client, base: instance.into(), username: username.into(), password: password.into(), jwt: Mutex::new(jwt.into()) }
    }

    /// The JWT in use, which may be newer than the one it started with.
    pub fn jwt(&self) -> String {
        self.jwt.lock().unwrap().clone()
    }

    /// Bad sign-ins, bans and rate limits are about the account. Anything else, like a locked post or a request that timed out
    /// and may have gone through, is about the post. Lemmy names its errors in `error`, mostly with HTTP 400.
    fn failure(status: StatusCode, body: &Value) -> Failure {
        let error = body["error"].as_str().unwrap_or_default();
        match (status, error) {
            // A server error may come after the comment was saved, so it never goes back in the queue.
            (s, _) if s.is_server_error() => Failure::Post(format!("Lemmy answered {s}")),
            (StatusCode::TOO_MANY_REQUESTS, _) | (_, "rate_limit_error") => Failure::Account("Lemmy is rate limiting the account".into()),
            (_, "incorrect_login") => Failure::Account("Lemmy didn't accept the username and password. Connect Lemmy again".into()),
            (_, "missing_totp_token" | "incorrect_totp_token") => {
                Failure::Account("The account signs in with two-factor codes, which Splee can't type. Use an account without them".into())
            }
            (_, "email_not_verified") => Failure::Account("Confirm the account's email address, then connect Lemmy again".into()),
            (_, "registration_application_is_pending" | "registration_denied") => Failure::Account("The instance hasn't approved the account".into()),
            (_, "site_ban" | "person_is_banned_from_site") => Failure::Account("The instance banned the account".into()),
            (_, "banned_from_community") => Failure::Account("A community banned the account".into()),
            (StatusCode::UNAUTHORIZED, _) | (_, "not_logged_in") => Failure::Account("Lemmy signed the account out. Connect Lemmy again".into()),
            (StatusCode::NOT_FOUND, _) | (_, "couldnt_find_post" | "couldnt_find_comment" | "couldnt_find_community" | "not_found") => {
                Failure::Post("Lemmy couldn't find it. It may have been deleted".into())
            }
            (_, e) if !e.is_empty() => Failure::Post(format!("Lemmy turned it down ({})", e.replace('_', " "))),
            (s, _) => Failure::Post(format!("Lemmy answered {s}")),
        }
    }

    async fn send(&self, request: RequestBuilder) -> Result<(StatusCode, Value), Failure> {
        let res = request.send().await.map_err(|e| Failure::Post(format!("Lemmy didn't answer: {}", e.without_url())))?;
        let status = res.status();
        Ok((status, res.json().await.unwrap_or_default()))
    }

    async fn login(&self) -> Result<String, Failure> {
        let form = json!({ "username_or_email": self.username, "password": self.password });
        let (status, body) = self.send(self.client.post(format!("{}/api/v3/user/login", self.base)).json(&form)).await?;
        let jwt = body["jwt"].as_str().filter(|j| status.is_success() && !j.is_empty());
        let Some(jwt) = jwt else {
            // A failed login posted nothing, so even a timeout or a server error is about the account here.
            return Err(Failure::Account(Self::failure(status, &body).message()));
        };
        *self.jwt.lock().unwrap() = jwt.to_string();
        Ok(jwt.to_string())
    }

    /// Sends the request with the JWT, signing in first when there's none and again once when the instance stops taking it,
    /// which is safe to retry since nothing was posted.
    async fn call(&self, build: impl Fn() -> RequestBuilder) -> Result<Value, Failure> {
        let mut jwt = self.jwt();
        if jwt.is_empty() {
            jwt = self.login().await?;
        }
        let (mut status, mut body) = self.send(build().bearer_auth(&jwt)).await?;
        if status == StatusCode::UNAUTHORIZED || body["error"] == "not_logged_in" {
            jwt = self.login().await?;
            (status, body) = self.send(build().bearer_auth(&jwt)).await?;
        }
        if status.is_success() { Ok(body) } else { Err(Self::failure(status, &body)) }
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<impl Fn() -> RequestBuilder, Failure> {
        let url = Url::parse_with_params(&format!("{}/api/v3{path}", self.base), query).map_err(|e| Failure::Post(e.to_string()))?;
        let client = self.client.clone();
        Ok(move || client.get(url.clone()))
    }

    fn post(&self, path: &str, body: Value) -> impl Fn() -> RequestBuilder {
        let (client, url) = (self.client.clone(), format!("{}/api/v3{path}", self.base));
        move || client.post(&url).json(&body)
    }

    /// The account as name@instance, which checks the sign-in.
    pub async fn me(&self) -> Result<String, Failure> {
        let site = self.call(self.get("/site", &[])?).await?;
        let name = site["my_user"]["local_user_view"]["person"]["name"].as_str().ok_or_else(|| Failure::Account("Lemmy didn't say which account this is".into()))?;
        let host = Url::parse(&self.base).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
        Ok(format!("{name}@{host}"))
    }

    /// The newest posts or comments matching `query` across every community the instance knows, or in one.
    pub async fn search(&self, query: &str, comments: bool, community: Option<&str>, limit: u64) -> Result<Vec<Value>, Failure> {
        let limit = limit.to_string();
        let kind = if comments { "Comments" } else { "Posts" };
        let mut params = vec![("q", query), ("type_", kind), ("sort", "New"), ("listing_type", "All"), ("limit", &limit)];
        if let Some(community) = community {
            params.push(("community_name", community));
        }
        let found = self.call(self.get("/search", &params)?).await?;
        Ok(found[if comments { "comments" } else { "posts" }].as_array().cloned().unwrap_or_default())
    }

    /// A post, as a PostView.
    pub async fn post_view(&self, id: u64) -> Result<Value, Failure> {
        Ok(self.call(self.get("/post", &[("id", &id.to_string())])?).await?["post_view"].clone())
    }

    /// A comment, as a CommentView.
    pub async fn comment_view(&self, id: u64) -> Result<Value, Failure> {
        Ok(self.call(self.get("/comment", &[("id", &id.to_string())])?).await?["comment_view"].clone())
    }

    /// The post's top comments, as CommentViews.
    pub async fn comments(&self, post: u64, limit: u64) -> Result<Vec<Value>, Failure> {
        let (post, limit) = (post.to_string(), limit.to_string());
        let found = self.call(self.get("/comment/list", &[("post_id", &post), ("sort", "Top"), ("type_", "All"), ("limit", &limit)])?).await?;
        Ok(found["comments"].as_array().cloned().unwrap_or_default())
    }

    fn own(&self, link: &Link) -> Result<(), Failure> {
        if link.instance != self.base {
            return Err(Failure::Post(format!("That's on {}, and the account is on {}", link.instance, self.base)));
        }
        Ok(())
    }

    /// Comments on the post or comment at `link` and returns the comment's link.
    pub async fn comment(&self, link: &Link, content: &str) -> Result<String, Failure> {
        self.own(link)?;
        let mut body = json!({ "content": content, "post_id": link.id });
        if link.comment {
            body["parent_id"] = json!(link.id);
            body["post_id"] = self.comment_view(link.id).await?["comment"]["post_id"].clone();
        }
        let made = self.call(self.post("/comment", body)).await?;
        Ok(made["comment_view"]["comment"]["id"].as_u64().map(|id| Link { instance: self.base.clone(), comment: true, id }.url()).unwrap_or_default())
    }

    /// Posts to the community, as name@instance, and returns the post's link.
    pub async fn create_post(&self, community: &str, title: &str, body: &str) -> Result<String, Failure> {
        let found = self.call(self.get("/community", &[("name", community)])?).await?;
        let id = found["community_view"]["community"]["id"].as_u64().ok_or_else(|| Failure::Post(format!("Lemmy couldn't find {community}")))?;
        let made = self.call(self.post("/post", json!({ "name": title, "body": body, "community_id": id }))).await?;
        Ok(made["post_view"]["post"]["id"].as_u64().map(|id| Link { instance: self.base.clone(), comment: false, id }.url()).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::extract::{Query, State};
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    use axum::{Json, Router};

    use super::*;

    #[test]
    fn links_communities_and_times_get_one_form() {
        let post = Link { instance: "https://lemmy.world".into(), comment: false, id: 42 };
        assert_eq!(Link::parse("https://lemmy.world/post/42?scrollToComments=true").as_ref(), Some(&post));
        assert_eq!(post.url(), "https://lemmy.world/post/42");
        assert_eq!(Link::parse("https://lemmy.world/comment/7").unwrap().url(), "https://lemmy.world/comment/7");
        for link in ["https://lemmy.world/c/sales", "http://lemmy.world/post/1", "https://lemmy.world/post/x"] {
            assert_eq!(Link::parse(link), None, "{link}");
        }
        for raw in ["!Sales@lemmy.world", "sales@lemmy.world", "https://lemmy.world/c/sales", "https://sh.itjust.works/c/sales@lemmy.world", "sales"] {
            assert_eq!(community(raw, "lemmy.world").as_deref(), Some("sales@lemmy.world"), "{raw}");
        }
        assert_eq!(community("r/sales", "lemmy.world"), None);
        assert_eq!(time(&json!("2026-11-01T10:00:00.123456Z")), time(&json!("2026-11-01T10:00:00.123456")));
        assert!(time(&json!("2026-11-01T10:00:00.123456")).is_some());
    }

    type Sent = Arc<Mutex<Vec<Value>>>;

    /// Lemmy 0.19 as its docs describe it. The password picks whether login works, the JWT whether calls do, and the text how
    /// posting goes.
    async fn fake_lemmy() -> (String, Sent) {
        fn signed_in(headers: &HeaderMap) -> bool {
            headers.get("authorization").is_some_and(|a| a == "Bearer FRESH")
        }
        fn error(e: &str) -> (StatusCode, Json<Value>) {
            (StatusCode::BAD_REQUEST, Json(json!({ "error": e })))
        }
        async fn login(Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            match body["password"].as_str().unwrap() {
                "right" => (StatusCode::OK, Json(json!({ "jwt": "FRESH", "registration_created": false, "verify_email_sent": false }))),
                _ => error("incorrect_login"),
            }
        }
        async fn site(headers: HeaderMap) -> (StatusCode, Json<Value>) {
            if !signed_in(&headers) {
                return error("not_logged_in");
            }
            (StatusCode::OK, Json(json!({ "my_user": { "local_user_view": { "person": { "name": "ana" } } } })))
        }
        async fn search(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            assert_eq!((q["type_"].as_str(), q["sort"].as_str(), q["listing_type"].as_str()), ("Posts", "New", "All"));
            Json(json!({ "type_": "Posts", "posts": [{ "post": { "id": 42, "name": q["q"] } }], "comments": [] }))
        }
        async fn comment_view(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            Json(json!({ "comment_view": { "comment": { "id": q["id"].parse::<u64>().unwrap(), "post_id": 42 } } }))
        }
        async fn community(Query(q): Query<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            match q["name"].as_str() {
                "sales@lemmy.world" => (StatusCode::OK, Json(json!({ "community_view": { "community": { "id": 5, "name": "sales" } } }))),
                _ => error("couldnt_find_community"),
            }
        }
        async fn comment(State(sent): State<Sent>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            if !signed_in(&headers) {
                return error("not_logged_in");
            }
            match body["content"].as_str().unwrap() {
                "banned" => error("banned_from_community"),
                "locked" => error("locked"),
                "fast" => (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": "rate_limit_error" }))),
                "down" => (StatusCode::BAD_GATEWAY, Json(json!({}))),
                _ => {
                    sent.lock().unwrap().push(body);
                    (StatusCode::OK, Json(json!({ "comment_view": { "comment": { "id": 77 } } })))
                }
            }
        }
        async fn create(State(sent): State<Sent>, Json(body): Json<Value>) -> Json<Value> {
            sent.lock().unwrap().push(body);
            Json(json!({ "post_view": { "post": { "id": 88 } } }))
        }
        let sent = Sent::default();
        let app = Router::new()
            .route("/api/v3/user/login", post(login))
            .route("/api/v3/site", get(site))
            .route("/api/v3/search", get(search))
            .route("/api/v3/comment", get(comment_view).post(comment))
            .route("/api/v3/community", get(community))
            .route("/api/v3/post", post(create))
            .with_state(sent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, sent)
    }

    /// public_client turns away 127.0.0.1, so the fake gets a plain client.
    fn lemmy(base: &str, password: &str, jwt: &str) -> Lemmy {
        Lemmy::with(reqwest::Client::new(), base, "ana", password, jwt)
    }

    #[tokio::test]
    async fn an_expired_jwt_is_traded_for_a_new_one_and_failures_say_whose_problem_it_is() {
        let (base, sent) = fake_lemmy().await;
        let l = lemmy(&base, "right", "");
        assert_eq!(l.me().await.ok().unwrap(), "ana@127.0.0.1");
        assert_eq!(l.jwt(), "FRESH");
        // The saved JWT stopped working, so it signs in again and keeps the new one.
        let l = lemmy(&base, "right", "EXPIRED");
        assert_eq!(l.search("cold email", false, None, 10).await.ok().unwrap()[0]["post"]["name"], "cold email");
        assert!(l.me().await.is_ok());
        assert_eq!(l.jwt(), "FRESH");
        assert!(matches!(lemmy(&base, "wrong", "EXPIRED").me().await, Err(Failure::Account(e)) if e.contains("username and password")));

        let on = |comment, id| Link { instance: base.clone(), comment, id };
        assert_eq!(l.comment(&on(false, 42), "Warm it up first").await.ok().unwrap(), format!("{base}/comment/77"));
        assert_eq!(l.comment(&on(true, 9), "Same here").await.ok().unwrap(), format!("{base}/comment/77"));
        assert_eq!(l.create_post("sales@lemmy.world", "How we warm up", "Four weeks").await.ok().unwrap(), format!("{base}/post/88"));
        assert_eq!(*sent.lock().unwrap(), [
            json!({ "content": "Warm it up first", "post_id": 42 }),
            json!({ "content": "Same here", "post_id": 42, "parent_id": 9 }),
            json!({ "name": "How we warm up", "body": "Four weeks", "community_id": 5 }),
        ]);

        assert!(matches!(l.create_post("nope@lemmy.world", "Hi", "Hi").await, Err(Failure::Post(e)) if e.contains("couldn't find")));
        assert!(matches!(l.comment(&on(false, 42), "banned").await, Err(Failure::Account(e)) if e.contains("banned")));
        assert!(matches!(l.comment(&on(false, 42), "fast").await, Err(Failure::Account(e)) if e.contains("rate limiting")));
        assert!(matches!(l.comment(&on(false, 42), "locked").await, Err(Failure::Post(e)) if e == "Lemmy turned it down (locked)"));
        assert!(matches!(l.comment(&on(false, 42), "down").await, Err(Failure::Post(_))));
        let elsewhere = Link { instance: "https://lemmy.ml".into(), comment: false, id: 42 };
        assert!(matches!(l.comment(&elsewhere, "Hi").await, Err(Failure::Post(e)) if e.contains("lemmy.ml")));
    }
}
