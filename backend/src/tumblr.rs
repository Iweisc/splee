//! Tumblr through its API v2, signed in with OAuth 2 through the operator's Tumblr app (routes/tumblr.rs), as one of the
//! account's blogs. Tumblr has no replies for apps, so Splee answers a post by reblogging it with a comment. Posts are in the
//! Neue Post Format. Access tokens last under an hour, and renewing one replaces the refresh token too.

use std::time::Duration;

use chrono::Utc;
use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::{Config, OAuthApp};
use crate::crypto::{decrypt, encrypt};
use crate::mail::oauth::{TokenError, Tokens, token_request};
use crate::models::Ts;
use crate::social::Failure;

pub const AUTHORIZE_URL: &str = "https://www.tumblr.com/oauth2/authorize";
pub const TOKEN_URL: &str = "https://api.tumblr.com/v2/oauth2/token";
/// offline_access is what gets a refresh token back.
pub const SCOPE: &str = "basic write offline_access";
/// Tumblr's limit on one text block. A post is split into a block per paragraph, so this bounds a paragraph, and Splee keeps
/// whole posts and comments under it too.
pub const CHARS: usize = 4_096;
pub const MAX_TAGS: usize = 10;
const NOT_SET_UP: &str = "Tumblr sign-in isn't set up on this Splee anymore. Set TUMBLR_CLIENT_ID and TUMBLR_CLIENT_SECRET on the server";

/// Tags as Tumblr takes them: without the #, commas or repeats, at most 140 characters each and 10 in all.
pub fn tags(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in raw {
        let tag = tag.replace(',', " ").trim().trim_start_matches('#').trim().to_string();
        if !tag.is_empty() && tag.chars().count() <= 140 && !out.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
            out.push(tag);
        }
    }
    out.truncate(MAX_TAGS);
    out
}

/// The API's base, which the token endpoint sits under. Tests point the app at a fake, which serves both.
fn api(app: &OAuthApp) -> &str {
    app.token_url.trim_end_matches("/oauth2/token")
}

/// A post on a blog.
#[derive(Debug, PartialEq)]
pub struct PostRef {
    pub blog: String,
    pub id: String,
}

impl PostRef {
    /// Reads https://{blog}.tumblr.com/post/{id}, https://www.tumblr.com/{blog}/{id} and https://tumblr.com/{blog}/{id}, each
    /// with an optional slug after it.
    pub fn parse(link: &str) -> Option<Self> {
        let url = Url::parse(link.trim()).ok()?;
        let host = url.host_str()?.to_ascii_lowercase();
        let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        let (blog, id) = match (host.as_str(), path.as_slice()) {
            ("www.tumblr.com" | "tumblr.com", [blog, id, ..]) => (blog.to_ascii_lowercase(), *id),
            (h, ["post", id, ..]) => (h.strip_suffix(".tumblr.com")?.to_string(), *id),
            _ => return None,
        };
        let blog_ok = (1..=32).contains(&blog.len()) && blog.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        let id_ok = (1..=30).contains(&id.len()) && id.chars().all(|c| c.is_ascii_digit());
        (url.scheme() == "https" && blog_ok && id_ok && blog != "www").then(|| Self { blog, id: id.into() })
    }

    pub fn url(&self) -> String {
        format!("https://www.tumblr.com/{}/{}", self.blog, self.id)
    }
}

/// The post's text, from its NPF content blocks or, for an older post, its summary.
pub fn text(post: &Value) -> String {
    let blocks: Vec<&str> = post["content"].as_array().map(|c| c.iter().filter_map(|b| b["text"].as_str()).collect()).unwrap_or_default();
    if blocks.is_empty() { post["summary"].as_str().unwrap_or_default().to_string() } else { blocks.join("\n\n") }
}

/// Text as NPF blocks, one per paragraph.
fn blocks(text: &str) -> Vec<Value> {
    text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).map(|p| json!({ "type": "text", "text": p })).collect()
}

/// The account a token belongs to and the blogs it can post to.
pub struct Account {
    pub name: String,
    /// [{name, title, url, uuid}], as tumblr_blogs keeps them.
    pub blogs: Vec<Value>,
}

pub struct Tumblr {
    client: reqwest::Client,
    api: String,
    token: String,
    /// The blog it posts as, by name.
    blog: String,
}

impl Tumblr {
    pub fn new(app: &OAuthApp, token: &str, blog: &str) -> Result<Self, Failure> {
        let client = reqwest::Client::builder().user_agent("Splee").timeout(Duration::from_secs(30)).build().map_err(|e| Failure::Account(e.to_string()))?;
        Ok(Self { client, api: api(app).into(), token: token.into(), blog: blog.into() })
    }

    /// A revoked token and rate limits are about the account. Anything else, like a deleted post or a request that timed out
    /// and may have gone through, is about the post. Tumblr says why in errors[].detail and meta.msg.
    async fn call(&self, request: RequestBuilder) -> Result<Value, Failure> {
        let res = request.bearer_auth(&self.token).send().await.map_err(|e| Failure::Post(format!("Tumblr didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        let said = body["errors"][0]["detail"].as_str().or(body["meta"]["msg"].as_str()).filter(|m| !m.is_empty());
        let paren = said.map(|m| format!(" ({m})")).unwrap_or_default();
        Err(match status {
            s if s.is_success() => return Ok(body["response"].clone()),
            // A server error may come after the post was saved, so it never goes back in the queue.
            s if s.is_server_error() => Failure::Post(format!("Tumblr answered {s}{paren}")),
            StatusCode::UNAUTHORIZED => Failure::Account("Tumblr signed Splee out. Connect Tumblr again".into()),
            StatusCode::TOO_MANY_REQUESTS => Failure::Account("Tumblr is rate limiting the account".into()),
            StatusCode::NOT_FOUND => Failure::Post("Tumblr couldn't find it. It may have been deleted".into()),
            s => Failure::Post(said.map_or_else(|| format!("Tumblr answered {s}"), String::from)),
        })
    }

    /// A read before anything is posted. A timeout or server error there leaves the post to try again later, while a post
    /// that's gone still fails it.
    async fn lookup(&self, request: RequestBuilder) -> Result<Value, Failure> {
        self.call(request).await.map_err(|f| match f {
            Failure::Post(m) if m.starts_with("Tumblr answered 5") || m.starts_with("Tumblr didn't answer") => Failure::Account(m),
            f => f,
        })
    }

    pub async fn me(&self) -> Result<Account, Failure> {
        let me = self.call(self.client.get(format!("{}/user/info", self.api))).await?;
        let name = me["user"]["name"].as_str().filter(|n| !n.is_empty()).ok_or_else(|| Failure::Account("Tumblr didn't say which account this is".into()))?;
        let blogs = me["user"]["blogs"].as_array().cloned().unwrap_or_default();
        let blogs = blogs.iter().map(|b| json!({ "name": b["name"], "title": b["title"], "url": b["url"], "uuid": b["uuid"] })).collect();
        Ok(Account { name: name.into(), blogs })
    }

    /// The newest posts with the tag, in NPF.
    pub async fn tagged(&self, tag: &str, limit: u64) -> Result<Vec<Value>, Failure> {
        let url = Url::parse_with_params(&format!("{}/tagged", self.api), [("tag", tag), ("npf", "true"), ("limit", &limit.to_string())]);
        let found = self.call(self.client.get(url.map_err(|e| Failure::Post(e.to_string()))?)).await?;
        Ok(found.as_array().cloned().unwrap_or_default())
    }

    /// One post, in NPF.
    pub async fn post_at(&self, post: &PostRef) -> Result<Value, Failure> {
        self.call(self.client.get(format!("{}/blog/{}/posts/{}", self.api, post.blog, post.id))).await
    }

    fn link(&self, made: &Value) -> String {
        // It's out now, so not getting its id back doesn't fail it.
        let id = made["id_string"].as_str().or(made["id"].as_str()).map(String::from).or_else(|| made["id"].as_u64().map(|id| id.to_string()));
        id.map(|id| PostRef { blog: self.blog.clone(), id }.url()).unwrap_or_default()
    }

    /// Reblogs the post to the blog with `comment` above it, and returns the reblog's link. Tumblr wants the parent's blog
    /// uuid and reblog key, so the post is read first.
    pub async fn reblog(&self, post: &PostRef, comment: &str) -> Result<String, Failure> {
        let parent = self.lookup(self.client.get(format!("{}/blog/{}/posts/{}", self.api, post.blog, post.id))).await?;
        if parent["can_reblog"] == false {
            return Err(Failure::Post("That post can't be reblogged".into()));
        }
        let (Some(uuid), Some(key)) = (parent["blog"]["uuid"].as_str(), parent["reblog_key"].as_str()) else {
            return Err(Failure::Post("Tumblr didn't give what a reblog needs".into()));
        };
        let body = json!({ "content": blocks(comment), "parent_tumblelog_uuid": uuid, "parent_post_id": post.id, "reblog_key": key });
        let made = self.call(self.client.post(format!("{}/blog/{}/posts", self.api, self.blog)).json(&body)).await?;
        Ok(self.link(&made))
    }

    /// A text post on the blog, and its link.
    pub async fn create(&self, text: &str, tags: &[String]) -> Result<String, Failure> {
        let mut body = json!({ "content": blocks(text) });
        if !tags.is_empty() {
            body["tags"] = json!(tags.join(","));
        }
        let made = self.call(self.client.post(format!("{}/blog/{}/posts", self.api, self.blog)).json(&body)).await?;
        Ok(self.link(&made))
    }
}

pub fn app(cfg: &Config) -> Result<&OAuthApp, Failure> {
    cfg.tumblr.as_ref().ok_or_else(|| Failure::Account(NOT_SET_UP.into()))
}

pub async fn exchange_code(app: &OAuthApp, code: &str, redirect_uri: &str) -> Result<Tokens, TokenError> {
    token_request("Tumblr", app, &[("grant_type", "authorization_code"), ("code", code), ("redirect_uri", redirect_uri)]).await
}

/// The project's Tumblr access token: the stored one while it has a minute left, else a new one. Under a lock on the
/// project, since the poster and a social run can both ask, and the first renewal makes the old refresh token stop working.
pub async fn access_token(db: &PgPool, cfg: &Config, project_id: Uuid) -> Result<String, Failure> {
    let failed = |e: sqlx::Error| Failure::Account(format!("Couldn't read the Tumblr sign-in ({e})"));
    let mut tx = db.begin().await.map_err(failed)?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('tumblr-token:' || $1::text, 0))").bind(project_id).execute(&mut *tx).await.map_err(failed)?;
    let (access_enc, refresh_enc, expires_at): (String, String, Option<Ts>) =
        sqlx::query_as("select tumblr_token_enc, tumblr_refresh_enc, tumblr_expires_at from project_settings where project_id = $1")
            .bind(project_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(failed)?;
    let secret = &cfg.app_secret;
    let unreadable = |_| Failure::Account("Couldn't read the saved Tumblr sign-in. Connect Tumblr again".into());
    if !access_enc.is_empty() && expires_at.is_some_and(|at| at > Utc::now() + chrono::Duration::seconds(60)) {
        return decrypt(secret, &access_enc).map_err(unreadable);
    }
    let refresh = decrypt(secret, &refresh_enc).map_err(unreadable)?;
    if refresh.is_empty() {
        return Err(Failure::Account("Connect Tumblr first".into()));
    }
    let tokens = token_request("Tumblr", app(cfg)?, &[("grant_type", "refresh_token"), ("refresh_token", &refresh)]).await.map_err(|e| match e {
        TokenError::Revoked(_) => Failure::Account("Tumblr signed Splee out. Connect Tumblr again".into()),
        e => Failure::Account(e.to_string()),
    })?;
    // Matched on the old refresh token, so a reconnect since keeps its own.
    sqlx::query(
        "update project_settings set tumblr_token_enc = $3, tumblr_expires_at = $4, tumblr_refresh_enc = coalesce($5, tumblr_refresh_enc)
         where project_id = $1 and tumblr_refresh_enc = $2",
    )
    .bind(project_id)
    .bind(&refresh_enc)
    .bind(encrypt(secret, &tokens.access_token))
    .bind(tokens.expires_at())
    .bind(tokens.refresh_token.as_deref().filter(|t| !t.is_empty()).map(|t| encrypt(secret, t)))
    .execute(&mut *tx)
    .await
    .map_err(failed)?;
    tx.commit().await.map_err(failed)?;
    Ok(tokens.access_token)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, Query, State};
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    use axum::{Form, Json, Router};

    use super::*;

    #[test]
    fn post_links_and_tags_get_one_form() {
        let post = PostRef { blog: "acme".into(), id: "7712345".into() };
        for link in ["https://acme.tumblr.com/post/7712345/warming-up", "https://www.tumblr.com/Acme/7712345", "https://tumblr.com/acme/7712345/x?source=share"] {
            assert_eq!(PostRef::parse(link).as_ref(), Some(&post), "{link}");
        }
        assert_eq!(post.url(), "https://www.tumblr.com/acme/7712345");
        for link in ["http://acme.tumblr.com/post/1", "https://www.tumblr.com/acme", "https://acme.tumblr.com/tagged/sales", "https://example.com/post/1"] {
            assert_eq!(PostRef::parse(link), None, "{link}");
        }
        let raw = ["#Cold Email", "cold email", "sales, b2b", ""].map(String::from);
        assert_eq!(tags(&raw), ["Cold Email", "sales  b2b"]);
        let npf = json!({ "content": [{ "type": "text", "text": "One" }, { "type": "image" }, { "type": "text", "text": "Two" }], "summary": "x" });
        assert_eq!(text(&npf), "One\n\nTwo");
        assert_eq!(blocks("One\n\n\nTwo  \n\n"), [json!({ "type": "text", "text": "One" }), json!({ "type": "text", "text": "Two" })]);
    }

    pub(crate) type Sent = Arc<Mutex<Vec<Value>>>;

    /// Tumblr's API and token endpoint as their docs describe them. The access token picks whether calls work, the refresh
    /// token whether renewing does, and the text how posting goes.
    pub(crate) async fn fake_tumblr() -> (OAuthApp, Sent) {
        fn refused(headers: &HeaderMap) -> Option<(StatusCode, Json<Value>)> {
            let ok = headers.get("authorization").and_then(|a| a.to_str().ok()).is_some_and(|a| a.starts_with("Bearer access-"));
            (!ok).then(|| (StatusCode::UNAUTHORIZED, Json(json!({ "meta": { "status": 401, "msg": "Unauthorized" }, "response": [] }))))
        }
        fn ok(response: Value) -> (StatusCode, Json<Value>) {
            (StatusCode::OK, Json(json!({ "meta": { "status": 200, "msg": "OK" }, "response": response })))
        }
        async fn token(State(sent): State<Sent>, Form(form): Form<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            sent.lock().unwrap().push(json!(form));
            let n = sent.lock().unwrap().len();
            match (form["grant_type"].as_str(), form.get("refresh_token").map(String::as_str)) {
                ("refresh_token", Some("revoked")) => (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_grant" }))),
                (_, _) => (StatusCode::OK, Json(json!({ "access_token": format!("access-{n}"), "expires_in": 2520, "refresh_token": format!("refresh-{n}") }))),
            }
        }
        async fn me(headers: HeaderMap) -> (StatusCode, Json<Value>) {
            refused(&headers).unwrap_or_else(|| {
                let blog = |name: &str| json!({ "name": name, "title": format!("{name} notes"), "url": format!("https://{name}.tumblr.com/"), "uuid": format!("t:{name}"), "followers": 3 });
                ok(json!({ "user": { "name": "acme", "blogs": [blog("acme"), blog("acme-labs")] } }))
            })
        }
        async fn tagged(Query(q): Query<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            assert_eq!(q["npf"], "true");
            ok(json!([{ "id_string": "42", "blog_name": "bo", "post_url": "https://bo.tumblr.com/post/42", "tags": [q["tag"]] }]))
        }
        async fn one(Path((blog, id)): Path<(String, String)>) -> (StatusCode, Json<Value>) {
            match (blog.as_str(), id.as_str()) {
                ("bo", "42") => ok(json!({ "id_string": "42", "blog": { "name": "bo", "uuid": "t:bo-uuid" }, "reblog_key": "rk42", "can_reblog": true })),
                ("bo", "43") => ok(json!({ "id_string": "43", "blog": { "name": "bo", "uuid": "t:bo-uuid" }, "reblog_key": "rk43", "can_reblog": false })),
                ("bo", "44") => (StatusCode::BAD_GATEWAY, Json(json!({}))),
                _ => (StatusCode::NOT_FOUND, Json(json!({ "meta": { "status": 404, "msg": "Not Found" }, "response": [] }))),
            }
        }
        async fn create(State(sent): State<Sent>, Path(blog): Path<String>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            if let Some(refused) = refused(&headers) {
                return refused;
            }
            assert_eq!(blog, "acme-labs");
            match body["content"][0]["text"].as_str().unwrap_or_default() {
                "fast" => (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "meta": { "status": 429, "msg": "Limit Exceeded" } }))),
                "bad" => (StatusCode::BAD_REQUEST, Json(json!({ "errors": [{ "title": "Bad Request", "code": 8001, "detail": "Post content is invalid" }] }))),
                "down" => (StatusCode::BAD_GATEWAY, Json(json!({}))),
                _ => {
                    sent.lock().unwrap().push(body);
                    (StatusCode::CREATED, Json(json!({ "meta": { "status": 201, "msg": "Created" }, "response": { "id": "900", "state": "published" } })))
                }
            }
        }
        let sent = Sent::default();
        let app = Router::new()
            .route("/v2/oauth2/token", post(token))
            .route("/v2/user/info", get(me))
            .route("/v2/tagged", get(tagged))
            .route("/v2/blog/{blog}/posts/{id}", get(one))
            .route("/v2/blog/{blog}/posts", post(create))
            .with_state(sent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let app = OAuthApp { client_id: "tumblr-client".into(), client_secret: "shh".into(), authorize_url: format!("{base}/oauth2/authorize"), token_url: format!("{base}/v2/oauth2/token") };
        (app, sent)
    }

    #[tokio::test]
    async fn reblogs_carry_the_comment_and_failures_say_whose_problem_it_is() {
        let (app, sent) = fake_tumblr().await;
        let me = Tumblr::new(&app, "access-1", "").unwrap().me().await.ok().unwrap();
        assert_eq!((me.name.as_str(), me.blogs.len()), ("acme", 2));
        assert_eq!(me.blogs[1], json!({ "name": "acme-labs", "title": "acme-labs notes", "url": "https://acme-labs.tumblr.com/", "uuid": "t:acme-labs" }));
        assert!(matches!(Tumblr::new(&app, "old", "").unwrap().me().await, Err(Failure::Account(e)) if e.contains("Connect Tumblr again")));

        let t = Tumblr::new(&app, "access-1", "acme-labs").unwrap();
        assert_eq!(t.tagged("cold email", 20).await.ok().unwrap()[0]["tags"], json!(["cold email"]));
        let bo = |id: &str| PostRef { blog: "bo".into(), id: id.into() };
        assert_eq!(t.reblog(&bo("42"), "Warm it up first.\n\nFour weeks, 20 a day.").await.ok().unwrap(), "https://www.tumblr.com/acme-labs/900");
        assert_eq!(t.create("How we warm up", &["cold email".into(), "sales".into()]).await.ok().unwrap(), "https://www.tumblr.com/acme-labs/900");
        assert_eq!(*sent.lock().unwrap(), [
            json!({ "content": [{ "type": "text", "text": "Warm it up first." }, { "type": "text", "text": "Four weeks, 20 a day." }],
                    "parent_tumblelog_uuid": "t:bo-uuid", "parent_post_id": "42", "reblog_key": "rk42" }),
            json!({ "content": [{ "type": "text", "text": "How we warm up" }], "tags": "cold email,sales" }),
        ]);

        assert!(matches!(t.reblog(&bo("43"), "Hi").await, Err(Failure::Post(e)) if e.contains("can't be reblogged")));
        assert!(matches!(t.reblog(&bo("41"), "Hi").await, Err(Failure::Post(e)) if e.contains("deleted")));
        // The read before the reblog posted nothing, so a server error there leaves the reblog to try again.
        assert!(matches!(t.reblog(&bo("44"), "Hi").await, Err(Failure::Account(_))));
        assert!(matches!(t.create("fast", &[]).await, Err(Failure::Account(e)) if e.contains("rate limiting")));
        assert!(matches!(t.create("bad", &[]).await, Err(Failure::Post(e)) if e == "Post content is invalid"));
        assert!(matches!(t.create("down", &[]).await, Err(Failure::Post(_))));
        assert!(matches!(Tumblr::new(&app, "old", "acme-labs").unwrap().create("Hi", &[]).await, Err(Failure::Account(_))));
    }

    async fn cfg(app: OAuthApp) -> Config {
        let other = crate::mail::oauth::tests::FakeProvider::start().await;
        Config { tumblr: Some(app), ..crate::mail::oauth::tests::config(&other) }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_access_token_is_renewed_once_and_keeps_the_new_refresh_token(db: PgPool) {
        let (app, sent) = fake_tumblr().await;
        let cfg = cfg(app).await;
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into project_settings (project_id, tumblr_token_enc, tumblr_refresh_enc, tumblr_expires_at) values ($1, $2, $3, now() + interval '20 seconds')")
            .bind(project)
            .bind(encrypt(&cfg.app_secret, "access-old"))
            .bind(encrypt(&cfg.app_secret, "refresh-0"))
            .execute(&db)
            .await
            .unwrap();
        // Under a minute left, so both callers get one new token between them.
        let (a, b) = tokio::join!(access_token(&db, &cfg, project), access_token(&db, &cfg, project));
        assert_eq!((a.ok().unwrap(), b.ok().unwrap()), ("access-1".to_string(), "access-1".to_string()));
        assert_eq!(sent.lock().unwrap()[0]["refresh_token"], "refresh-0");
        let refresh: String = sqlx::query_scalar("select tumblr_refresh_enc from project_settings where project_id = $1").bind(project).fetch_one(&db).await.unwrap();
        assert_eq!(decrypt(&cfg.app_secret, &refresh).unwrap(), "refresh-1");
        assert_eq!(sent.lock().unwrap().len(), 1);

        sqlx::query("update project_settings set tumblr_expires_at = now(), tumblr_refresh_enc = $2 where project_id = $1")
            .bind(project)
            .bind(encrypt(&cfg.app_secret, "revoked"))
            .execute(&db)
            .await
            .unwrap();
        assert!(matches!(access_token(&db, &cfg, project).await, Err(Failure::Account(e)) if e.contains("Connect Tumblr again")));
        assert!(matches!(access_token(&db, &Config { tumblr: None, ..cfg }, project).await, Err(Failure::Account(e)) if e.contains("TUMBLR_CLIENT_ID")));
    }
}
