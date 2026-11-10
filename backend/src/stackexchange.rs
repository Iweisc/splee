//! Stack Exchange through API 2.3, as the user on the one site they picked, with the key of their own registered app and an
//! access token for it with the write_access scope. Splee only answers questions there, it never asks them. The token goes in
//! the query string or form, as the docs show it, so errors drop the URL.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;
use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::Value;

use crate::social::Failure;

const API: &str = "https://api.stackexchange.com/2.3";
/// Stack Exchange's limits on an answer's body, in characters.
pub const CHARS: usize = 30_000;
pub const MIN_CHARS: usize = 30;
/// The longest a call waits out a backoff from the call before it. Past that it gives up, so a run doesn't stall.
const MAX_WAIT: Duration = Duration::from_secs(60);

pub struct StackExchange {
    client: reqwest::Client,
    api: String,
    key: String,
    token: String,
    /// The site's host, like stackoverflow.com, or before connecting whatever the user typed that the API takes as a site.
    site: String,
    /// When each method may be called again, from the API's backoff field.
    waits: Mutex<HashMap<&'static str, Instant>>,
}

/// Who the token belongs to on the site.
pub struct User {
    /// {site}/users/{user_id}, which the daily cap counts by. User ids differ per site, so the site is part of it.
    pub account: String,
    pub name: String,
    pub site: String,
}

/// A question on a Stack Exchange site.
#[derive(Debug, PartialEq)]
pub struct Question {
    pub site: String,
    pub id: u64,
}

/// Stack Overflow, its other languages, *.stackexchange.com and the sites with their own domains, with their metas.
fn is_site(host: &str) -> bool {
    const OWN: [&str; 5] = ["superuser.com", "serverfault.com", "askubuntu.com", "mathoverflow.net", "stackapps.com"];
    let main = host.strip_prefix("meta.").unwrap_or(host);
    main == "stackoverflow.com" || main.ends_with(".stackoverflow.com") || host.ends_with(".stackexchange.com") || OWN.contains(&main)
}

impl Question {
    /// Reads /questions/{id}/{slug} and /q/{id} links.
    pub fn parse(link: &str) -> Option<Self> {
        let url = Url::parse(link.trim()).ok()?;
        let host = url.host_str()?.to_ascii_lowercase();
        let host = host.strip_prefix("www.").unwrap_or(&host);
        let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        let id = match path.as_slice() {
            ["questions" | "q", id, ..] => id.parse::<u64>().ok().filter(|id| *id > 0)?,
            _ => return None,
        };
        (url.scheme() == "https" && is_site(host)).then(|| Self { site: host.into(), id })
    }

    pub fn url(&self) -> String {
        format!("https://{}/questions/{}", self.site, self.id)
    }
}

/// The site as the user typed it: a link or host on it, or the API's name for it, like "unix".
pub fn site_input(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_end_matches('/').to_ascii_lowercase();
    if !raw.contains('.') && !raw.contains('/') {
        return (!raw.is_empty() && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(raw);
    }
    let url = Url::parse(&if raw.contains("://") { raw.clone() } else { format!("https://{raw}") }).ok()?;
    let host = url.host_str()?;
    let host = host.strip_prefix("www.").unwrap_or(host);
    (url.scheme() == "https" && is_site(host)).then(|| host.to_string())
}

/// Write failures that come from the user's own posting limits, like a new account's wait between answers, rather than from
/// the question. The API puts them all under write_failed, so the message is all there is to go on.
fn rate_limited(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    ["once every", "rate limit", "too many", "too fast", "wait"].iter().any(|w| m.contains(w))
}

impl StackExchange {
    pub fn new(key: &str, token: &str, site: &str) -> Result<Self, Failure> {
        Self::at(API, key, token, site)
    }

    fn at(api: &str, key: &str, token: &str, site: &str) -> Result<Self, Failure> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().map_err(|e| Failure::Account(e.to_string()))?;
        Ok(Self { client, api: api.into(), key: key.into(), token: token.into(), site: site.into(), waits: Mutex::default() })
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url, Failure> {
        let mut url = Url::parse_with_params(&format!("{}{path}", self.api), query).map_err(|e| Failure::Post(e.to_string()))?;
        url.query_pairs_mut().append_pair("site", &self.site).append_pair("key", &self.key).append_pair("access_token", &self.token);
        Ok(url)
    }

    /// Waits out the backoff the API last gave for `method`, or gives up on a long one, which holds the account.
    async fn wait(&self, method: &'static str) -> Result<(), Failure> {
        let until = self.waits.lock().unwrap().get(method).copied();
        let Some(left) = until.map(|u| u.saturating_duration_since(Instant::now())).filter(|d| !d.is_zero()) else { return Ok(()) };
        if left > MAX_WAIT {
            return Err(Failure::Account(format!("Stack Exchange asked Splee to wait {} seconds before trying again", left.as_secs())));
        }
        tokio::time::sleep(left).await;
        Ok(())
    }

    /// The backoff the API gave on the last call to `method`, in seconds, 0 for none.
    pub fn backoff(&self, method: &'static str) -> u64 {
        self.waits.lock().unwrap().get(method).map_or(0, |u| u.saturating_duration_since(Instant::now()).as_secs())
    }

    /// A bad token, a missing scope and throttling are about the account. Anything else, like a closed question or a request
    /// that timed out and may have gone through, is about the post. Errors come back as HTTP 400 with an error_id that
    /// mirrors an HTTP status.
    async fn call(&self, method: &'static str, request: RequestBuilder) -> Result<Value, Failure> {
        self.wait(method).await?;
        let res = request.send().await.map_err(|e| Failure::Post(format!("Stack Exchange didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        if let Some(secs) = body["backoff"].as_u64().filter(|s| *s > 0) {
            self.waits.lock().unwrap().insert(method, Instant::now() + Duration::from_secs(secs));
        }
        let message = body["error_message"].as_str().unwrap_or_default();
        let said = if message.is_empty() { String::new() } else { format!(" ({message})") };
        Err(match (status, body["error_id"].as_i64()) {
            (s, None) if s.is_success() => return Ok(body),
            // A server error may come after the answer was saved, so it never goes back in the queue.
            (s, _) if s.is_server_error() => Failure::Post(format!("Stack Exchange answered {s}{said}")),
            (_, Some(500 | 503)) => Failure::Post(format!("Stack Exchange had an error{said}")),
            (StatusCode::TOO_MANY_REQUESTS, _) | (_, Some(502)) => Failure::Account(format!("Stack Exchange is throttling the account{said}")),
            (_, Some(401 | 402 | 406)) => {
                Failure::Account(format!("Stack Exchange didn't accept the access token{said}. Make a new one and connect Stack Exchange again"))
            }
            (_, Some(403)) => Failure::Account("The access token can't post. Make one with the write_access scope and connect Stack Exchange again".into()),
            (_, Some(405)) => Failure::Account("Stack Exchange wants the app's key. Connect Stack Exchange again".into()),
            (_, Some(407)) if rate_limited(message) => Failure::Account(format!("Stack Exchange is holding the account's answers{said}")),
            (_, Some(404)) => Failure::Post("Stack Exchange couldn't find it. It may have been deleted".into()),
            _ => Failure::Post(format!("Stack Exchange turned it down{said}")),
        })
    }

    /// Whether the token may write, which an answer needs.
    pub async fn can_write(&self) -> Result<bool, Failure> {
        let mut url = Url::parse(&format!("{}/access-tokens", self.api)).map_err(|e| Failure::Post(e.to_string()))?;
        url.path_segments_mut().map_err(|_| Failure::Post("bad API address".into()))?.push(&self.token);
        url.query_pairs_mut().append_pair("key", &self.key);
        let found = self.call("access-tokens", self.client.get(url)).await?;
        let scope = found["items"][0]["scope"].as_array().cloned().unwrap_or_default();
        Ok(scope.iter().any(|s| s == "write_access"))
    }

    /// The token's user on the site, which checks the token and the site.
    pub async fn me(&self) -> Result<User, Failure> {
        let me = self.call("me", self.client.get(self.url("/me", &[])?)).await?;
        let user = &me["items"][0];
        let link = user["link"].as_str().and_then(|l| Url::parse(l).ok());
        let site = link.as_ref().and_then(|l| l.host_str()).map(|h| h.strip_prefix("www.").unwrap_or(h).to_string()).filter(|h| is_site(h));
        match (user["user_id"].as_u64(), site) {
            (Some(id), Some(site)) => {
                Ok(User { account: format!("{site}/users/{id}"), name: user["display_name"].as_str().unwrap_or_default().into(), site })
            }
            _ => Err(Failure::Account(format!("You don't have an account on {} yet. Join the site, then connect again", self.site))),
        }
    }

    /// Open questions from the last month matching `query` without an accepted answer, most recently active first.
    pub async fn search(&self, query: &str, tagged: &str, limit: u64) -> Result<Vec<Value>, Failure> {
        let from = (Utc::now() - chrono::Duration::days(30)).timestamp().to_string();
        let limit = limit.to_string();
        let mut params = vec![("q", query), ("sort", "activity"), ("order", "desc"), ("fromdate", &from), ("accepted", "False"), ("closed", "False")];
        params.push(("pagesize", &limit));
        if !tagged.is_empty() {
            params.push(("tagged", tagged));
        }
        let found = self.call("search", self.client.get(self.url("/search/advanced", &params)?)).await?;
        Ok(found["items"].as_array().cloned().unwrap_or_default())
    }

    /// The question with its body, as HTML.
    pub async fn question(&self, id: u64) -> Result<Value, Failure> {
        let found = self.call("questions", self.client.get(self.url(&format!("/questions/{id}"), &[("filter", "withbody")])?)).await?;
        Some(found["items"][0].clone()).filter(Value::is_object).ok_or_else(|| Failure::Post("Stack Exchange couldn't find that question. It may be deleted".into()))
    }

    /// The question's answers, best first, with their bodies as HTML.
    pub async fn answers(&self, id: u64) -> Result<Vec<Value>, Failure> {
        let query = [("filter", "withbody"), ("sort", "votes"), ("order", "desc"), ("pagesize", "5")];
        let found = self.call("answers", self.client.get(self.url(&format!("/questions/{id}/answers"), &query)?)).await?;
        Ok(found["items"].as_array().cloned().unwrap_or_default())
    }

    /// Answers the question with `body`, in markdown, and returns the answer's link.
    pub async fn answer(&self, question: &Question, body: &str) -> Result<String, Failure> {
        if question.site != self.site {
            return Err(Failure::Post(format!("That question is on {}, and the account is on {}", question.site, self.site)));
        }
        let form = [("body", body), ("site", &self.site), ("key", &self.key), ("access_token", &self.token)];
        let request = self.client.post(format!("{}/questions/{}/answers/add", self.api, question.id)).form(&form);
        let made = self.call("answer", request).await?;
        // It's out now, so not getting its id back doesn't fail it.
        Ok(made["items"][0]["answer_id"].as_u64().map(|id| format!("https://{}/a/{id}", self.site)).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::extract::{Path, Query, State};
    use axum::routing::{get, post};
    use axum::{Form, Json, Router};
    use serde_json::json;

    use super::*;

    #[test]
    fn question_links_and_sites_get_one_form() {
        let q = Question { site: "stackoverflow.com".into(), id: 79724206 };
        for link in ["https://stackoverflow.com/questions/79724206/firebase-auth?noredirect=1", "https://www.stackoverflow.com/q/79724206/123"] {
            assert_eq!(Question::parse(link).as_ref(), Some(&q), "{link}");
        }
        assert_eq!(q.url(), "https://stackoverflow.com/questions/79724206");
        assert_eq!(Question::parse("https://unix.stackexchange.com/questions/42/x").unwrap().site, "unix.stackexchange.com");
        for link in ["https://stackoverflow.com/users/42", "http://superuser.com/questions/1", "https://example.com/questions/1", "https://stackexchange.com/questions/1"] {
            assert_eq!(Question::parse(link), None, "{link}");
        }
        assert_eq!(site_input("https://superuser.com/questions/1/x").as_deref(), Some("superuser.com"));
        assert_eq!(site_input(" Unix ").as_deref(), Some("unix"));
        assert_eq!(site_input("www.stackoverflow.com/").as_deref(), Some("stackoverflow.com"));
        assert_eq!(site_input("example.com"), None);
    }

    type Sent = Arc<Mutex<Vec<HashMap<String, String>>>>;

    /// The API as its docs describe it: errors are HTTP 400 with an error_id. The token picks whether the account works, and
    /// the answer's body how posting goes.
    async fn fake_api() -> (String, Sent) {
        fn error(id: i64, name: &str, message: &str) -> (StatusCode, Json<Value>) {
            (StatusCode::BAD_REQUEST, Json(json!({ "error_id": id, "error_name": name, "error_message": message })))
        }
        async fn tokens(Path(token): Path<String>) -> Json<Value> {
            let scope = if token == "GOOD))" { json!(["write_access"]) } else { json!([]) };
            Json(json!({ "items": [{ "access_token": token, "scope": scope }] }))
        }
        async fn me(Query(q): Query<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            match (q["access_token"].as_str(), q["site"].as_str()) {
                ("OLD", _) => error(402, "invalid_access_token", "access_token is invalid"),
                (_, "superuser.com") => (StatusCode::OK, Json(json!({ "items": [] }))),
                _ => (StatusCode::OK, Json(json!({ "items": [{ "user_id": 31, "display_name": "Ana", "link": "https://stackoverflow.com/users/31/ana" }] }))),
            }
        }
        async fn search(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            assert_eq!((q["sort"].as_str(), q["accepted"].as_str(), q["site"].as_str()), ("activity", "False", "stackoverflow.com"));
            Json(json!({ "items": [{ "question_id": 7, "title": q["q"], "answer_count": 0 }], "backoff": 1 }))
        }
        async fn add(State(sent): State<Sent>, Path(id): Path<u64>, Form(form): Form<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            assert_eq!(id, 7);
            match form["body"].as_str() {
                "throttled" => error(502, "throttle_violation", "too many requests from this IP, more requests available in 120 seconds"),
                "slow down" => error(407, "write_failed", "You can only post once every 3 minutes"),
                "closed" => error(407, "write_failed", "This question is closed"),
                "scope" => error(403, "access_denied", "write_access scope required"),
                "down" => error(500, "internal_error", "An unexpected error occurred"),
                _ => {
                    sent.lock().unwrap().push(form);
                    (StatusCode::OK, Json(json!({ "items": [{ "answer_id": 99, "question_id": 7 }], "backoff": 90 })))
                }
            }
        }
        let sent = Sent::default();
        let app = Router::new()
            .route("/access-tokens/{token}", get(tokens))
            .route("/me", get(me))
            .route("/search/advanced", get(search))
            .route("/questions/{id}/answers/add", post(add))
            .with_state(sent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, sent)
    }

    #[tokio::test]
    async fn connecting_checks_the_scope_and_the_site() {
        let (base, _) = fake_api().await;
        let se = StackExchange::at(&base, "KEY", "GOOD))", "stackoverflow").unwrap();
        assert!(se.can_write().await.ok().unwrap());
        let me = se.me().await.ok().unwrap();
        assert_eq!((me.account.as_str(), me.name.as_str(), me.site.as_str()), ("stackoverflow.com/users/31", "Ana", "stackoverflow.com"));
        assert!(!StackExchange::at(&base, "KEY", "READ))", "stackoverflow").unwrap().can_write().await.ok().unwrap());
        let other = StackExchange::at(&base, "KEY", "GOOD))", "superuser.com").unwrap();
        assert!(matches!(other.me().await, Err(Failure::Account(e)) if e.contains("account on superuser.com")));
        let old = StackExchange::at(&base, "KEY", "OLD", "stackoverflow").unwrap();
        assert!(matches!(old.me().await, Err(Failure::Account(e)) if e.contains("connect Stack Exchange again")));
    }

    #[tokio::test]
    async fn answers_go_out_and_failures_say_whose_problem_it_is() {
        let (base, sent) = fake_api().await;
        let se = StackExchange::at(&base, "KEY", "GOOD))", "stackoverflow.com").unwrap();
        let q = Question::parse("https://stackoverflow.com/questions/7/how").unwrap();

        // A one-second backoff on search is waited out, not an error.
        assert_eq!(se.search("cold email", "", 10).await.ok().unwrap().len(), 1);
        assert_eq!(se.search("cold email", "", 10).await.ok().unwrap()[0]["title"], "cold email");

        assert_eq!(se.answer(&q, "Try a warmup schedule first").await.ok().unwrap(), "https://stackoverflow.com/a/99");
        let form = sent.lock().unwrap().pop().unwrap();
        assert_eq!((form["site"].as_str(), form["key"].as_str(), form["access_token"].as_str()), ("stackoverflow.com", "KEY", "GOOD))"));
        // The backoff it gave holds the next answer, which the poster turns into a hold on the account.
        assert!((89..=90).contains(&se.backoff("answer")));
        assert!(matches!(se.answer(&q, "Hi").await, Err(Failure::Account(e)) if e.contains("seconds before trying again")));

        let se = StackExchange::at(&base, "KEY", "GOOD))", "stackoverflow.com").unwrap();
        assert!(matches!(se.answer(&q, "throttled").await, Err(Failure::Account(e)) if e.contains("throttling")));
        assert!(matches!(se.answer(&q, "slow down").await, Err(Failure::Account(e)) if e.contains("once every 3 minutes")));
        assert!(matches!(se.answer(&q, "scope").await, Err(Failure::Account(e)) if e.contains("write_access")));
        assert!(matches!(se.answer(&q, "closed").await, Err(Failure::Post(e)) if e.contains("is closed")));
        assert!(matches!(se.answer(&q, "down").await, Err(Failure::Post(_))));
        let elsewhere = Question::parse("https://superuser.com/questions/7").unwrap();
        assert!(matches!(se.answer(&elsewhere, "Hi").await, Err(Failure::Post(e)) if e.contains("superuser.com")));
    }
}
