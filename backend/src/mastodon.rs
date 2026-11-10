//! Mastodon through its REST API, with an access token the user made on their instance under Preferences → Development, with
//! the read and write:statuses scopes. The instance is user input, so requests only go to a public address (see
//! hooks::public_client).

use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::{Value, json};

use crate::hooks;
use crate::social::Failure;

pub struct Mastodon {
    client: reqwest::Client,
    base: String,
    token: String,
}

/// Who the token belongs to, and how long a post can be on their instance.
pub struct Account {
    /// user@instance
    pub handle: String,
    pub max_chars: i32,
}

/// The instance's origin, like https://mastodon.social, from what the user typed: "mastodon.social", or any link on it.
pub fn instance_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    // A handle like @ana@mastodon.social names its instance after the last @.
    let raw = if raw.contains('/') { raw } else { raw.rsplit_once('@').map_or(raw, |(_, host)| host) };
    let url = Url::parse(&if raw.contains("://") { raw.to_string() } else { format!("https://{raw}") }).ok()?;
    let host = url.host_str()?;
    (url.scheme() == "https" && host.contains('.') && url.username().is_empty()).then(|| url.origin().ascii_serialization())
}

/// A status link in the form Mastodon shows it, https://<host>/@<user>/<id>, from that form or https://<host>/users/<user>/statuses/<id>.
pub fn status_url(link: &str) -> Option<String> {
    let url = Url::parse(link.trim()).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let (user, id) = match path.as_slice() {
        [user, id] if user.starts_with('@') => (&user[1..], *id),
        ["users", user, "statuses", id] => (*user, *id),
        _ => return None,
    };
    let user_ok = !user.is_empty() && user.chars().all(|c| c.is_ascii_alphanumeric() || "_.-@".contains(c));
    let id_ok = (1..=20).contains(&id.len()) && id.chars().all(|c| c.is_ascii_digit());
    (url.scheme() == "https" && host.contains('.') && user_ok && id_ok).then(|| format!("https://{host}/@{user}/{id}"))
}

/// Mastodon's text is HTML. Paragraphs and line breaks become new lines, and the rest of the markup goes.
pub fn plain(html: &str) -> String {
    let html = html.replace("</p><p>", "\n\n").replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n");
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ").replace("&amp;", "&").trim().to_string()
}

fn tag_ok(tag: &str) -> bool {
    !tag.is_empty() && tag.chars().all(|c| c.is_alphanumeric() || c == '_')
}

impl Mastodon {
    pub async fn new(base: &str, token: &str) -> Result<Self, Failure> {
        let url = Url::parse(base).map_err(|e| Failure::Account(e.to_string()))?;
        let client = hooks::public_client(&url).await.map_err(|e| Failure::Account(format!("Couldn't reach {base} ({e:#})")))?;
        Ok(Self { client, base: base.into(), token: token.into() })
    }

    /// 401s, missing scopes and rate limits are about the account. Anything else, like a deleted post or a request that timed
    /// out and may have gone through, is about the post.
    async fn call(&self, request: RequestBuilder) -> Result<Value, Failure> {
        let res = request.bearer_auth(&self.token).send().await.map_err(|e| Failure::Post(format!("Mastodon didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        let error = body["error"].as_str().filter(|m| !m.is_empty());
        let said = error.map(|m| format!(" ({m})")).unwrap_or_default();
        Err(match status {
            s if s.is_success() => return Ok(body),
            StatusCode::UNAUTHORIZED => Failure::Account(format!("Mastodon didn't accept the access token{said}. Make a new one and connect Mastodon again")),
            StatusCode::FORBIDDEN => Failure::Account(format!("The access token can't do that{said}. Make one with the read and write:statuses scopes")),
            StatusCode::TOO_MANY_REQUESTS => Failure::Account("Mastodon is rate limiting the account".into()),
            StatusCode::NOT_FOUND => Failure::Post("Mastodon couldn't find it. It may have been deleted".into()),
            StatusCode::UNPROCESSABLE_ENTITY if error.is_some() => Failure::Post(error.unwrap_or_default().into()),
            s => Failure::Post(format!("Mastodon answered {s}{said}")),
        })
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<RequestBuilder, Failure> {
        let url = Url::parse_with_params(&format!("{}{path}", self.base), query).map_err(|e| Failure::Post(e.to_string()))?;
        Ok(self.client.get(url))
    }

    /// The token's account, which checks it, and the instance's post limit.
    pub async fn me(&self) -> Result<Account, Failure> {
        let me = self.call(self.get("/api/v1/accounts/verify_credentials", &[])?).await?;
        let username = me["username"].as_str().ok_or_else(|| Failure::Account("Mastodon didn't say which account this is".into()))?;
        let instance = self.call(self.get("/api/v2/instance", &[])?).await.unwrap_or_default();
        let domain = instance["domain"].as_str().map(str::to_string).unwrap_or_else(|| self.base.trim_start_matches("https://").to_string());
        let max_chars = instance["configuration"]["statuses"]["max_characters"].as_i64().unwrap_or(500).clamp(1, 100_000) as i32;
        Ok(Account { handle: format!("{username}@{domain}"), max_chars })
    }

    /// Posts matching `query`, newest first. Full-text search only works on instances that turned it on, so a #hashtag, or a
    /// one-word query that finds nothing, reads that hashtag's timeline instead.
    pub async fn search(&self, query: &str, limit: u64) -> Result<Vec<Value>, Failure> {
        let query = query.trim();
        let limit = limit.to_string();
        let hashtag = query.strip_prefix('#').filter(|t| tag_ok(t));
        if hashtag.is_none() {
            let found = self.call(self.get("/api/v2/search", &[("q", query), ("type", "statuses"), ("resolve", "false"), ("limit", &limit)])?).await?;
            let mut statuses = found["statuses"].as_array().cloned().unwrap_or_default();
            if !statuses.is_empty() || !tag_ok(query) {
                statuses.sort_by(|a, b| b["created_at"].as_str().cmp(&a["created_at"].as_str()));
                return Ok(statuses);
            }
        }
        let tag = hashtag.unwrap_or(query);
        let found = self.call(self.get(&format!("/api/v1/timelines/tag/{tag}"), &[("limit", &limit)])?).await?;
        Ok(found.as_array().cloned().unwrap_or_default())
    }

    /// The post at a link, from any instance, as this instance knows it, which is what replies need.
    pub async fn status(&self, link: &str) -> Result<Value, Failure> {
        let found = self.call(self.get("/api/v2/search", &[("q", link), ("type", "statuses"), ("resolve", "true"), ("limit", "1")])?).await?;
        let status = found["statuses"][0].clone();
        // The id goes into a path, so it has to be the number Mastodon uses.
        match status["id"].as_str() {
            Some(id) if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()) => Ok(status),
            _ => Err(Failure::Post("Mastodon couldn't find that post. It may be deleted or private".into())),
        }
    }

    /// The replies under a post.
    pub async fn replies(&self, status: &Value) -> Result<Vec<Value>, Failure> {
        let id = status["id"].as_str().unwrap_or_default();
        let context = self.call(self.get(&format!("/api/v1/statuses/{id}/context"), &[])?).await?;
        Ok(context["descendants"].as_array().cloned().unwrap_or_default())
    }

    /// Posts `text`, as a reply to the post at `reply_to` when there is one, and returns its link. `key` makes a retried
    /// request post once.
    pub async fn post(&self, reply_to: Option<&str>, text: &str, key: &str) -> Result<String, Failure> {
        let mut status = json!({ "status": text });
        if let Some(link) = reply_to {
            status["in_reply_to_id"] = self.status(link).await?["id"].clone();
        }
        let request = self.client.post(format!("{}/api/v1/statuses", self.base)).header("Idempotency-Key", key).json(&status);
        let made = self.call(request).await?;
        Ok(made["url"].as_str().unwrap_or_default().to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, Query, State};
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    use axum::{Json, Router};

    use super::*;

    #[test]
    fn instances_and_status_links_get_one_form() {
        for raw in ["mastodon.social", "https://mastodon.social/@ana", " @ana@mastodon.social ", "https://mastodon.social/"] {
            assert_eq!(instance_url(raw).as_deref(), Some("https://mastodon.social"), "{raw}");
        }
        for raw in ["http://mastodon.social", "localhost", "", "https://user:pw@mastodon.social"] {
            assert_eq!(instance_url(raw), None, "{raw}");
        }
        let canonical = Some("https://fosstodon.org/@ana/112233445566".to_string());
        assert_eq!(status_url("https://fosstodon.org/@ana/112233445566?x=1"), canonical);
        assert_eq!(status_url("https://Fosstodon.org/users/ana/statuses/112233445566"), canonical);
        assert_eq!(status_url("https://mastodon.social/@ana@fosstodon.org/112233445566").as_deref(), Some("https://mastodon.social/@ana@fosstodon.org/112233445566"));
        for link in ["https://fosstodon.org/@ana", "https://medium.com/@ana/some-title-1a2b3c", "http://fosstodon.org/@ana/1", "https://x.com/i/status/1"] {
            assert_eq!(status_url(link), None, "{link}");
        }
    }

    #[test]
    fn html_becomes_text() {
        assert_eq!(plain("<p>Hi <span class=\"h-card\"><a href=\"x\">@<span>ana</span></a></span> &amp; all</p><p>Line<br />two</p>"), "Hi @ana & all\n\nLine\ntwo");
    }

    type Sent = Arc<Mutex<Vec<String>>>;

    /// Mastodon as its docs describe it. The token picks whether the account works, and the text how posting goes.
    async fn fake_mastodon() -> (Mastodon, Sent) {
        async fn me(headers: HeaderMap) -> (StatusCode, Json<Value>) {
            match headers["authorization"].to_str().unwrap() {
                "Bearer GOOD" => (StatusCode::OK, Json(json!({ "id": "1", "username": "ana", "acct": "ana" }))),
                _ => (StatusCode::UNAUTHORIZED, Json(json!({ "error": "The access token is invalid" }))),
            }
        }
        async fn search(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            match (q["q"].as_str(), q["resolve"].as_str()) {
                ("https://other.social/@bo/42", "true") => Json(json!({ "statuses": [{ "id": "9001", "url": "https://other.social/@bo/42" }] })),
                // Full-text search is off on this instance.
                _ => Json(json!({ "statuses": [] })),
            }
        }
        async fn tag(Path(tag): Path<String>) -> Json<Value> {
            Json(json!([{ "id": "7", "content": format!("<p>#{tag}</p>"), "created_at": "2026-11-01T00:00:00.000Z" }]))
        }
        async fn statuses(State(sent): State<Sent>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            match body["status"].as_str().unwrap() {
                "long" => (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": "Validation failed: Text character limit of 500 exceeded" }))),
                "fast" => (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": "Too many requests" }))),
                "scope" => (StatusCode::FORBIDDEN, Json(json!({ "error": "This action is outside the authorized scopes" }))),
                text => {
                    sent.lock().unwrap().push(format!("{text} reply_to={} key={}", body["in_reply_to_id"], headers["idempotency-key"].to_str().unwrap()));
                    (StatusCode::OK, Json(json!({ "id": "5", "url": "https://acme.social/@ana/5" })))
                }
            }
        }
        let sent = Sent::default();
        let app = Router::new()
            .route("/api/v1/accounts/verify_credentials", get(me))
            .route("/api/v2/instance", get(|| async { Json(json!({ "domain": "acme.social", "configuration": { "statuses": { "max_characters": 1000 } } })) }))
            .route("/api/v2/search", get(search))
            .route("/api/v1/timelines/tag/{tag}", get(tag))
            .route("/api/v1/statuses", post(statuses))
            .with_state(sent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        // public_client turns away 127.0.0.1, so the fake gets a plain client.
        (Mastodon { client: reqwest::Client::new(), base, token: "GOOD".into() }, sent)
    }

    #[tokio::test]
    async fn the_account_search_and_posting_work_and_failures_say_whose_problem_it_is() {
        let (m, _) = fake_mastodon().await;
        let me = m.me().await.ok().unwrap();
        assert_eq!((me.handle.as_str(), me.max_chars), ("ana@acme.social", 1000));
        assert!(matches!(Mastodon { token: "BAD".into(), ..m }.me().await, Err(Failure::Account(e)) if e.contains("connect Mastodon again")));
        let (m, sent) = fake_mastodon().await;

        // Full-text search finds nothing here, so one word reads its hashtag instead. Several words don't.
        assert_eq!(m.search("coldemail", 10).await.ok().unwrap()[0]["content"], "<p>#coldemail</p>");
        assert_eq!(m.search("#sales", 10).await.ok().unwrap()[0]["content"], "<p>#sales</p>");
        assert!(m.search("cold email tools", 10).await.ok().unwrap().is_empty());

        assert_eq!(m.post(Some("https://other.social/@bo/42"), "@bo@other.social Try this", "k1").await.ok().unwrap(), "https://acme.social/@ana/5");
        m.post(None, "A lesson", "k2").await.ok().unwrap();
        assert_eq!(*sent.lock().unwrap(), ["@bo@other.social Try this reply_to=\"9001\" key=k1", "A lesson reply_to=null key=k2"]);

        assert!(matches!(m.post(Some("https://other.social/@bo/404"), "Hi", "k3").await, Err(Failure::Post(e)) if e.contains("couldn't find")));
        assert!(matches!(m.post(None, "long", "k4").await, Err(Failure::Post(e)) if e == "Validation failed: Text character limit of 500 exceeded"));
        assert!(matches!(m.post(None, "fast", "k5").await, Err(Failure::Account(_))));
        assert!(matches!(m.post(None, "scope", "k6").await, Err(Failure::Account(e)) if e.contains("write:statuses")));
    }
}
