//! Discourse forums through their REST API, as one user: with a User API Key, or with an API key an admin made, which acts as
//! the username sent with it. A project can join several forums. Forums are user input, so requests only go to a public
//! address (see hooks::public_client).

use std::sync::Mutex;

use chrono::Utc;
use reqwest::header::RETRY_AFTER;
use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::{Value, json};

use crate::hooks;
use crate::social::Failure;

/// Discourse's default limits, in characters, on a post's text and a topic's title.
pub const MIN_CHARS: usize = 20;
pub const CHARS: usize = 32_000;
pub const MIN_TITLE_CHARS: usize = 15;
pub const TITLE_CHARS: usize = 255;
/// How long a 429 without a Retry-After holds the forum.
const DEFAULT_WAIT: u64 = 60;

pub struct Discourse {
    client: reqwest::Client,
    base: String,
    key: String,
    /// Set for an admin's API key, which acts as this user. A User API Key knows its user.
    api_username: Option<String>,
    /// The seconds the forum last asked to wait after a 429, for the poster to hold the forum that long.
    retry_after: Mutex<Option<u64>>,
}

/// The first path segments of Discourse's own pages. What comes before one of them in a link is the forum's address.
const ROUTES: [&str; 14] = ["t", "c", "latest", "top", "new", "unread", "categories", "u", "g", "search", "tag", "tags", "about", "session"];

/// The forum's address, like https://forum.example.com or https://example.com/forum, from what the user typed: its host, its
/// address or a link to any page on it.
pub fn forum_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let url = Url::parse(&if raw.contains("://") { raw.to_string() } else { format!("https://{raw}") }).ok()?;
    let host = url.host_str()?;
    if url.scheme() != "https" || !host.contains('.') || !url.username().is_empty() {
        return None;
    }
    let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).take_while(|s| !ROUTES.contains(s)).collect();
    let path = if path.is_empty() { String::new() } else { format!("/{}", path.join("/")) };
    Some(format!("{}{path}", url.origin().ascii_serialization()))
}

/// A topic on a forum.
#[derive(Debug, PartialEq)]
pub struct Topic {
    /// The forum's address, as forum_url gives it.
    pub forum: String,
    pub id: u64,
}

impl Topic {
    /// Reads {forum}/t/{slug}/{id}, {forum}/t/{id} and either with a post number after it.
    pub fn parse(link: &str) -> Option<Self> {
        let url = Url::parse(link.trim()).ok()?;
        let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        let at = path.iter().position(|s| *s == "t")?;
        let number = |s: &&str| s.parse::<u64>().ok().filter(|id| *id > 0);
        // A slug is never only digits, so a number right after /t/ is the id.
        let id = path.get(at + 1).and_then(number).or_else(|| path.get(at + 2).and_then(number))?;
        let forum = forum_url(link)?;
        let prefix = &path[..at];
        // forum_url stops at the first route, which has to be this /t/.
        (forum.ends_with(&if prefix.is_empty() { String::new() } else { format!("/{}", prefix.join("/")) })).then_some(Self { forum, id })
    }

    pub fn url(&self) -> String {
        format!("{}/t/{}", self.forum, self.id)
    }
}

/// The account a forum's user counts as for the daily cap, in any project.
pub fn account(forum: &str, username: &str) -> String {
    format!("{}/u/{}", forum.trim_start_matches("https://"), username).to_lowercase()
}

/// The category the user named, by its name, its slug or a link to it, from the forum's categories.
pub fn find_category<'a>(categories: &'a [Value], wanted: &str) -> Option<&'a Value> {
    let wanted = wanted.trim();
    if let Ok(url) = Url::parse(wanted) {
        // /c/{slug}/{id}, or /c/{parent}/{slug}/{id} for a subcategory.
        let id = url.path_segments()?.filter(|s| !s.is_empty()).skip_while(|s| *s != "c").filter_map(|s| s.parse::<i64>().ok()).last()?;
        return categories.iter().find(|c| c["id"].as_i64() == Some(id));
    }
    let named = |key: &str, c: &Value| c[key].as_str().is_some_and(|n| n.eq_ignore_ascii_case(wanted));
    categories.iter().find(|c| named("name", c)).or_else(|| categories.iter().find(|c| named("slug", c)))
}

impl Discourse {
    pub async fn new(forum: &str, key: &str, api_username: Option<&str>) -> Result<Self, Failure> {
        let url = Url::parse(forum).map_err(|e| Failure::Account(e.to_string()))?;
        let client = hooks::public_client(&url).await.map_err(|e| Failure::Account(format!("Couldn't reach {forum} ({e:#})")))?;
        Ok(Self::with(client, forum, key, api_username))
    }

    fn with(client: reqwest::Client, forum: &str, key: &str, api_username: Option<&str>) -> Self {
        Self { client, base: forum.into(), key: key.into(), api_username: api_username.map(String::from), retry_after: Mutex::default() }
    }

    fn name(&self) -> &str {
        self.base.trim_start_matches("https://")
    }

    /// The wait the forum gave with its last 429, in seconds, once.
    pub fn retry_after(&self) -> Option<u64> {
        self.retry_after.lock().unwrap().take()
    }

    /// A bad key, a key without the write scope and rate limits are about the account. Anything else, like a closed topic or a
    /// request that timed out and may have gone through, is about the post.
    async fn call(&self, request: RequestBuilder) -> Result<Value, Failure> {
        let request = match &self.api_username {
            Some(user) => request.header("Api-Key", &self.key).header("Api-Username", user),
            None => request.header("User-Api-Key", &self.key),
        };
        let name = self.name();
        let res = request.header("Accept", "application/json").send().await.map_err(|e| Failure::Post(format!("{name} didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let retry_after = res.headers().get(RETRY_AFTER).and_then(|v| v.to_str().ok()?.trim().parse::<u64>().ok());
        let body: Value = res.json().await.unwrap_or_default();
        let errors: Vec<&str> = body["errors"].as_array().map(|e| e.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        let said = if errors.is_empty() { String::new() } else { format!(" ({})", errors.join(" ")) };
        Err(match status {
            s if s.is_success() => return Ok(body),
            // A server error may come after the post was saved, so it never goes back in the queue.
            s if s.is_server_error() => Failure::Post(format!("{name} answered {s}{said}")),
            StatusCode::UNAUTHORIZED => Failure::Account(format!("{name} didn't accept the key. Make a new one and connect the forum again")),
            StatusCode::FORBIDDEN if body["error_type"] == "not_logged_in" => {
                Failure::Account(format!("{name} didn't accept the key. Make a new one and connect the forum again"))
            }
            StatusCode::FORBIDDEN => Failure::Account(format!("{name} won't let the account do that{said}. Check that the key can write")),
            StatusCode::TOO_MANY_REQUESTS => {
                let wait = retry_after.or_else(|| body["extras"]["wait_seconds"].as_u64()).unwrap_or(DEFAULT_WAIT);
                *self.retry_after.lock().unwrap() = Some(wait);
                Failure::Account(format!("{name} asked Splee to wait {wait} seconds{said}"))
            }
            StatusCode::NOT_FOUND => Failure::Post(format!("{name} couldn't find it. It may have been deleted or private")),
            _ if !errors.is_empty() => Failure::Post(errors.join(" ")),
            s => Failure::Post(format!("{name} answered {s}")),
        })
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<RequestBuilder, Failure> {
        let url = Url::parse_with_params(&format!("{}{path}", self.base), query).map_err(|e| Failure::Post(e.to_string()))?;
        Ok(self.client.get(url))
    }

    /// The key's username, which checks the key.
    pub async fn me(&self) -> Result<String, Failure> {
        let refused = || Failure::Account(format!("{} didn't accept the key. Check the forum's address and the key", self.name()));
        let me = self.call(self.get("/session/current.json", &[])?).await.map_err(|e| if let Failure::Post(_) = e { refused() } else { e })?;
        me["current_user"]["username"].as_str().map(String::from).ok_or_else(refused)
    }

    /// The categories the user can see, subcategories included.
    pub async fn categories(&self) -> Result<Vec<Value>, Failure> {
        let site = self.call(self.get("/site.json", &[])?).await?;
        Ok(site["categories"].as_array().cloned().unwrap_or_default())
    }

    /// Open topics from the last month matching `query`, newest first, each with the blurb of the post that matched.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Value>, Failure> {
        let after = (Utc::now() - chrono::Duration::days(30)).format("%Y-%m-%d");
        let q = format!("{} status:open after:{after} order:latest", query.trim());
        let found = self.call(self.get("/search.json", &[("q", &q)])?).await?;
        let posts = found["posts"].as_array().cloned().unwrap_or_default();
        let mut topics = found["topics"].as_array().cloned().unwrap_or_default();
        topics.truncate(limit);
        for topic in &mut topics {
            let post = posts.iter().find(|p| p["topic_id"] == topic["id"]);
            topic["blurb"] = post.map_or(Value::Null, |p| p["blurb"].clone());
        }
        Ok(topics)
    }

    /// The topic with its first posts, up to 20, their text as HTML.
    pub async fn topic(&self, id: u64) -> Result<Value, Failure> {
        self.call(self.get(&format!("/t/{id}.json"), &[])?).await
    }

    /// Replies to the topic with `raw`, in Markdown, and returns the reply's link.
    pub async fn reply(&self, topic: &Topic, raw: &str) -> Result<String, Failure> {
        if topic.forum != self.base {
            return Err(Failure::Post(format!("That topic is on {}, not {}", topic.forum, self.base)));
        }
        let made = self.call(self.client.post(format!("{}/posts.json", self.base)).json(&json!({ "topic_id": topic.id, "raw": raw }))).await?;
        Ok(self.link(&made))
    }

    /// Starts a topic in the category and returns its link.
    pub async fn create_topic(&self, category: i64, title: &str, raw: &str) -> Result<String, Failure> {
        let made = self.call(self.client.post(format!("{}/posts.json", self.base)).json(&json!({ "title": title, "raw": raw, "category": category }))).await?;
        Ok(self.link(&made))
    }

    /// It's out now, so not getting its link back doesn't fail it.
    fn link(&self, post: &Value) -> String {
        match (post["topic_slug"].as_str(), post["topic_id"].as_u64(), post["post_number"].as_u64()) {
            (Some(slug), Some(topic), Some(1)) => format!("{}/t/{slug}/{topic}", self.base),
            (Some(slug), Some(topic), Some(n)) => format!("{}/t/{slug}/{topic}/{n}", self.base),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::extract::{Path, Query, State};
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    use axum::{Json, Router};

    use super::*;

    #[test]
    fn forums_and_topic_links_get_one_form() {
        for raw in ["forum.acme.com", "https://forum.acme.com/", "https://Forum.acme.com/latest?x=1", "https://forum.acme.com/t/hi/42/3"] {
            assert_eq!(forum_url(raw).as_deref(), Some("https://forum.acme.com"), "{raw}");
        }
        assert_eq!(forum_url("https://acme.com/forum/c/sales/7").as_deref(), Some("https://acme.com/forum"));
        for raw in ["http://forum.acme.com", "localhost", "", "https://a:b@forum.acme.com"] {
            assert_eq!(forum_url(raw), None, "{raw}");
        }
        let topic = Topic { forum: "https://forum.acme.com".into(), id: 42 };
        for link in ["https://forum.acme.com/t/how-do-you-warm-up/42", "https://forum.acme.com/t/42", "https://forum.acme.com/t/how/42/7?u=ana"] {
            assert_eq!(Topic::parse(link).as_ref(), Some(&topic), "{link}");
        }
        assert_eq!(topic.url(), "https://forum.acme.com/t/42");
        assert_eq!(Topic::parse("https://acme.com/forum/t/x/9").unwrap().url(), "https://acme.com/forum/t/9");
        for link in ["https://forum.acme.com/c/sales/7", "https://forum.acme.com/t/slug", "https://acme.com/latest/t/x/1"] {
            assert_eq!(Topic::parse(link), None, "{link}");
        }
        assert_eq!(account("https://Forum.acme.com", "Ana"), "forum.acme.com/u/ana");

        let categories = [json!({ "id": 7, "name": "Sales Ops", "slug": "sales-ops" }), json!({ "id": 9, "name": "Help", "slug": "help" })];
        for wanted in ["sales ops", "SALES-OPS", "https://forum.acme.com/c/sales-ops/7", "https://forum.acme.com/c/team/sales-ops/7?x=1"] {
            assert_eq!(find_category(&categories, wanted).map(|c| c["id"].clone()), Some(json!(7)), "{wanted}");
        }
        assert!(find_category(&categories, "marketing").is_none());
    }

    type Sent = Arc<Mutex<Vec<Value>>>;

    /// Discourse as its docs describe it. The key picks whether the account works, and the text how posting goes.
    async fn fake_forum() -> (String, Sent) {
        fn refused(headers: &HeaderMap) -> Option<(StatusCode, HeaderMap, Json<Value>)> {
            let user_key = headers.get("user-api-key").is_some_and(|k| k == "GOOD");
            let admin_key = headers.get("api-key").is_some_and(|k| k == "ADMIN") && headers.get("api-username").is_some_and(|u| u == "ana");
            let no = json!({ "error_type": "not_logged_in", "errors": ["You need to be logged in to do that."] });
            (!user_key && !admin_key).then(|| (StatusCode::FORBIDDEN, HeaderMap::new(), Json(no)))
        }
        async fn me(headers: HeaderMap) -> (StatusCode, HeaderMap, Json<Value>) {
            refused(&headers).unwrap_or((StatusCode::OK, HeaderMap::new(), Json(json!({ "current_user": { "id": 3, "username": "Ana" } }))))
        }
        async fn search(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            assert!(q["q"].starts_with("cold email status:open after:") && q["q"].ends_with(" order:latest"), "{}", q["q"]);
            Json(json!({
                "posts": [{ "id": 1, "topic_id": 42, "blurb": "How do you warm up a domain?" }],
                "topics": [{ "id": 42, "title": "Warming up", "slug": "warming-up" }, { "id": 43, "title": "Other", "slug": "other" }]
            }))
        }
        async fn topic(Path(id): Path<String>) -> Json<Value> {
            Json(json!({ "id": id.trim_end_matches(".json").parse::<u64>().unwrap(), "title": "Warming up", "post_stream": { "posts": [] } }))
        }
        async fn posts(State(sent): State<Sent>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, HeaderMap, Json<Value>) {
            if let Some(refused) = refused(&headers) {
                return refused;
            }
            let mut wait = HeaderMap::new();
            match body["raw"].as_str().unwrap() {
                "fast" => {
                    wait.insert("retry-after", "120".parse().unwrap());
                    (StatusCode::TOO_MANY_REQUESTS, wait, Json(json!({ "errors": ["You've performed this action too many times."], "error_type": "rate_limit" })))
                }
                "short" => (StatusCode::UNPROCESSABLE_ENTITY, wait, Json(json!({ "errors": ["Body is too short (minimum is 20 characters)"] }))),
                "readonly" => (StatusCode::FORBIDDEN, wait, Json(json!({ "errors": ["You are not permitted to view the requested resource."], "error_type": "invalid_access" }))),
                "down" => (StatusCode::BAD_GATEWAY, wait, Json(json!({}))),
                _ => {
                    let new_topic = body["title"].is_string();
                    sent.lock().unwrap().push(body);
                    let (topic, number) = if new_topic { (50, 1) } else { (42, 6) };
                    (StatusCode::OK, wait, Json(json!({ "id": 9, "topic_id": topic, "topic_slug": "warming-up", "post_number": number })))
                }
            }
        }
        let sent = Sent::default();
        let app = Router::new()
            .route("/forum/session/current.json", get(me))
            .route("/forum/search.json", get(search))
            .route("/forum/t/{id}", get(topic))
            .route("/forum/posts.json", post(posts))
            .with_state(sent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/forum", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, sent)
    }

    /// public_client turns away 127.0.0.1, so the fake gets a plain client.
    fn forum(base: &str, key: &str, api_username: Option<&str>) -> Discourse {
        Discourse::with(reqwest::Client::new(), base, key, api_username)
    }

    #[tokio::test]
    async fn both_kinds_of_key_work_and_failures_say_whose_problem_it_is() {
        let (base, sent) = fake_forum().await;
        assert_eq!(forum(&base, "GOOD", None).me().await.ok().unwrap(), "Ana");
        assert_eq!(forum(&base, "ADMIN", Some("ana")).me().await.ok().unwrap(), "Ana");
        assert!(matches!(forum(&base, "ADMIN", Some("bo")).me().await, Err(Failure::Account(e)) if e.contains("didn't accept the key")));

        let d = forum(&base, "GOOD", None);
        let found = d.search("cold email", 1).await.ok().unwrap();
        assert_eq!(found, [json!({ "id": 42, "title": "Warming up", "slug": "warming-up", "blurb": "How do you warm up a domain?" })]);
        assert_eq!(d.topic(42).await.ok().unwrap()["id"], 42);

        let topic = Topic { forum: base.clone(), id: 42 };
        assert_eq!(d.reply(&topic, "Warm it up over four weeks").await.ok().unwrap(), format!("{base}/t/warming-up/42/6"));
        assert_eq!(d.create_topic(7, "How we warm up domains", "Four weeks, 20 a day").await.ok().unwrap(), format!("{base}/t/warming-up/50"));
        assert_eq!(*sent.lock().unwrap(), [
            json!({ "topic_id": 42, "raw": "Warm it up over four weeks" }),
            json!({ "title": "How we warm up domains", "raw": "Four weeks, 20 a day", "category": 7 }),
        ]);

        // A 429 holds the forum for as long as it asked, once.
        assert!(matches!(d.reply(&topic, "fast").await, Err(Failure::Account(e)) if e.contains("wait 120 seconds")));
        assert_eq!((d.retry_after(), d.retry_after()), (Some(120), None));
        assert!(matches!(d.reply(&topic, "short").await, Err(Failure::Post(e)) if e == "Body is too short (minimum is 20 characters)"));
        assert!(matches!(d.reply(&topic, "readonly").await, Err(Failure::Account(e)) if e.contains("can write")));
        assert!(matches!(d.reply(&topic, "down").await, Err(Failure::Post(_))));
        assert!(matches!(forum(&base, "OLD", None).reply(&topic, "Hi there").await, Err(Failure::Account(_))));
        let elsewhere = Topic { forum: "https://other.example".into(), id: 42 };
        assert!(matches!(d.reply(&elsewhere, "Hi there").await, Err(Failure::Post(e)) if e.contains("other.example")));
    }
}
