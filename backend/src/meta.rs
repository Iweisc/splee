//! Facebook Pages and Instagram business accounts through Meta's Graph API, signed in with Facebook through the operator's Meta
//! app (routes/meta.rs). Each picked Page posts with its own Page access token, and an Instagram account with the token of the
//! Page it's linked to. Meta says how much of its rate limits the app and each Page or account have used in the X-App-Usage and
//! X-Business-Use-Case-Usage headers, and a client stops calling once one is over 90%. `settle` saves that wait for the poster
//! and the next social run, and signs the project out when Meta turned a token down. Tokens go in the query string or the form,
//! as Meta's docs show them, so errors drop the URL.

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::OAuthApp;
use crate::crypto::encrypt;
use crate::social::Failure;

pub const API: &str = "https://graph.facebook.com/v21.0";
pub const AUTHORIZE_URL: &str = "https://www.facebook.com/v21.0/dialog/oauth";
/// Listing the Pages and their Instagram accounts, reading the comments on their posts, and posting and replying on both.
pub const SCOPE: &str = "pages_show_list,pages_read_engagement,pages_read_user_content,pages_manage_posts,pages_manage_engagement,business_management,\
                         instagram_basic,instagram_content_publish,instagram_manage_comments";
/// Facebook's limits on a Page post and a comment, and Instagram's on a caption and a comment, in characters.
pub const POST_CHARS: usize = 63_206;
pub const COMMENT_CHARS: usize = 8_000;
pub const CAPTION_CHARS: usize = 2_200;
/// Above this percentage of any limit in Meta's usage headers, calls stop until the wait is over.
const MAX_USAGE: f64 = 90.0;
/// The wait when Meta doesn't say how long, since its limits are counted over a rolling hour.
const DEFAULT_WAIT_SECS: f64 = 15.0 * 60.0;
/// What became of an Instagram container.
pub enum Published {
    /// Out, with its link, or "" when an earlier try published it.
    Live(String),
    /// Still being processed, for a later try.
    Processing,
    /// Instagram turned it down, so the post fails and a new try needs a new container.
    Rejected(String),
}

/// How often and how long an Instagram container is checked before publishing. Images are usually ready within seconds.
const CONTAINER_CHECKS: usize = 5;
const CONTAINER_WAIT: Duration = if cfg!(test) { Duration::from_millis(20) } else { Duration::from_secs(3) };
const SIGNED_OUT: &str = "Facebook signed Splee out, so nothing goes out on Facebook or Instagram. Sign in with Facebook again";
const RATE_LIMITED: &str = "Meta is rate limiting Splee's app or this account, so posting waits a while";

/// Graph ids, which go into paths: digits, or a post or comment id's two parts joined by an underscore.
fn is_id(id: &str) -> bool {
    let part = |p: &str| (1..=30).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit());
    match id.split_once('_') {
        Some((a, b)) => part(a) && part(b),
        None => part(id),
    }
}

/// The link Splee keeps for a comment on a Page's post: the post, which Facebook opens, with the comment's Graph id, which a
/// reply needs. A post's id starts with its Page's.
pub fn facebook_comment_link(post_id: &str, comment_id: &str) -> Option<String> {
    let (page, _) = post_id.split_once('_')?;
    (is_id(post_id) && is_id(page) && is_id(comment_id)).then(|| format!("https://www.facebook.com/{post_id}?comment_id={comment_id}"))
}

/// The Page and the comment's Graph id in a link in the form facebook_comment_link gives.
pub fn facebook_comment(link: &str) -> Option<(String, String)> {
    let url = Url::parse(link.trim()).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if url.scheme() != "https" || !matches!(host.as_str(), "www.facebook.com" | "facebook.com" | "m.facebook.com") {
        return None;
    }
    let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let [post] = path.as_slice() else { return None };
    let comment = url.query_pairs().find(|(k, _)| k == "comment_id")?.1.into_owned();
    facebook_comment_link(post, &comment)?;
    Some((post.split_once('_')?.0.to_string(), comment))
}

/// The link Splee keeps for a comment on an Instagram post: the comment's own link, which Instagram opens, plus the account's id,
/// which says whose token answers it. Instagram ignores the extra parameter.
pub fn instagram_comment_link(permalink: &str, comment_id: &str, account: &str) -> Option<String> {
    let url = Url::parse(permalink.trim()).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if url.scheme() != "https" || !matches!(host.as_str(), "www.instagram.com" | "instagram.com") {
        return None;
    }
    let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let code = match path.as_slice() {
        ["p" | "reel", code] | ["p" | "reel", code, "c", _] => *code,
        _ => return None,
    };
    let code_ok = (1..=40).contains(&code.len()) && code.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
    let ids_ok = is_id(comment_id) && !comment_id.contains('_') && is_id(account) && !account.contains('_');
    (code_ok && ids_ok).then(|| format!("https://www.instagram.com/p/{code}/c/{comment_id}/?account={account}"))
}

/// The account and the comment's id in a link in the form instagram_comment_link gives.
pub fn instagram_comment(link: &str) -> Option<(String, String)> {
    let url = Url::parse(link.trim()).ok()?;
    let account = url.query_pairs().find(|(k, _)| k == "account")?.1.into_owned();
    let comment = url.path_segments()?.filter(|s| !s.is_empty()).nth(3)?.to_string();
    instagram_comment_link(link, &comment, &account)?;
    Some((account, comment))
}

/// The one link Splee keeps for a Facebook or Instagram comment, for agent::social::canonical.
pub fn comment_url(link: &str) -> Option<String> {
    if let Some((_, comment)) = facebook_comment(link) {
        let url = Url::parse(link.trim()).ok()?;
        return facebook_comment_link(url.path().trim_matches('/'), &comment);
    }
    let (account, comment) = instagram_comment(link)?;
    instagram_comment_link(link, &comment, &account)
}

/// A link anyone can open, which a Facebook post's link and an Instagram post's picture have to be.
pub fn public_link(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some_and(|h| h.contains('.')))
}

/// A Page the user manages, with its token and the Instagram business account linked to it, if any.
pub struct Page {
    pub id: String,
    pub name: String,
    pub username: String,
    pub token: String,
    /// Whether the user's role on the Page lets them post (CREATE_CONTENT) and answer comments (MODERATE).
    pub can_post: bool,
    pub can_moderate: bool,
    pub instagram: Option<Instagram>,
}

pub struct Instagram {
    pub id: String,
    pub username: String,
    pub name: String,
}

/// How long Meta's usage headers say to wait, when a limit is over MAX_USAGE, and whether the limit is the app's own, which
/// every project shares.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hold {
    pub secs: f64,
    pub app_wide: bool,
}

/// Reads X-App-Usage, like {"call_count":28,"total_time":25,"total_cputime":25}, and X-Business-Use-Case-Usage, which has the
/// same per Page or account plus estimated_time_to_regain_access in minutes.
pub fn usage(app: Option<&str>, business: Option<&str>) -> Option<Hold> {
    let most = |v: &Value| ["call_count", "total_time", "total_cputime"].iter().filter_map(|k| v[*k].as_f64()).fold(0.0, f64::max);
    let app: Value = app.and_then(|h| serde_json::from_str(h).ok()).unwrap_or_default();
    let business: Value = business.and_then(|h| serde_json::from_str(h).ok()).unwrap_or_default();
    let entries: Vec<&Value> = business.as_object().into_iter().flat_map(|o| o.values()).flat_map(|v| v.as_array().into_iter().flatten()).collect();
    let over: Vec<&&Value> = entries.iter().filter(|e| most(e) > MAX_USAGE || e["estimated_time_to_regain_access"].as_f64().unwrap_or_default() > 0.0).collect();
    if !over.is_empty() {
        let minutes = over.iter().filter_map(|e| e["estimated_time_to_regain_access"].as_f64()).fold(0.0, f64::max);
        return Some(Hold { secs: if minutes > 0.0 { minutes * 60.0 } else { DEFAULT_WAIT_SECS }, app_wide: false });
    }
    (most(&app) > MAX_USAGE).then_some(Hold { secs: DEFAULT_WAIT_SECS, app_wide: true })
}

pub struct Graph {
    client: reqwest::Client,
    api: String,
    token: String,
    hold: Mutex<Option<Hold>>,
    signed_out: Mutex<bool>,
}

impl Graph {
    /// `api` is Config::graph_api_url.
    pub fn new(api: &str, token: &str) -> Result<Self, Failure> {
        let client = reqwest::Client::builder().user_agent("Splee").timeout(Duration::from_secs(30)).build().map_err(|e| Failure::Account(e.to_string()))?;
        Ok(Self { client, api: api.trim_end_matches('/').into(), token: token.into(), hold: Mutex::new(None), signed_out: Mutex::new(false) })
    }

    pub fn hold(&self) -> Option<Hold> {
        *self.hold.lock().unwrap()
    }

    pub fn signed_out(&self) -> bool {
        *self.signed_out.lock().unwrap()
    }

    fn held(&self, hold: Hold) {
        let mut held = self.hold.lock().unwrap();
        if held.is_none_or(|h| h.secs < hold.secs || hold.app_wide && !h.app_wide) {
            *held = Some(hold);
        }
    }

    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, Failure> {
        let mut url = Url::parse_with_params(&format!("{}{path}", self.api), query).map_err(|e| Failure::Post(e.to_string()))?;
        if !self.token.is_empty() {
            url.query_pairs_mut().append_pair("access_token", &self.token);
        }
        self.call(self.client.get(url)).await
    }

    /// Posts the form, so long text stays out of the URL.
    async fn post(&self, path: &str, form: &[(&str, &str)]) -> Result<Value, Failure> {
        let mut form = form.to_vec();
        form.push(("access_token", &self.token));
        self.call(self.client.post(format!("{}{path}", self.api)).form(&form)).await
    }

    /// A token Meta turned down and rate limits are about the account. Anything else, like a deleted comment, a missing
    /// permission on one Page or a request that may have gone through, is about the post.
    async fn call(&self, request: RequestBuilder) -> Result<Value, Failure> {
        if self.hold().is_some() {
            return Err(Failure::Account(RATE_LIMITED.into()));
        }
        let res = request.send().await.map_err(|e| Failure::Post(format!("Meta didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let header = |name: &str| res.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        if let Some(hold) = usage(header("x-app-usage").as_deref(), header("x-business-use-case-usage").as_deref()) {
            self.held(hold);
        }
        let body: Value = res.json().await.unwrap_or_default();
        let error = &body["error"];
        let code = error["code"].as_i64().unwrap_or_default();
        let said = error["error_user_msg"].as_str().or(error["message"].as_str()).filter(|m| !m.is_empty());
        let paren = said.map(|m| format!(" ({m})")).unwrap_or_default();
        Err(match status {
            s if s.is_success() => return Ok(body),
            // A server error may come after the post was saved, whatever the error says, so it never goes back in the queue.
            s if s.is_server_error() => Failure::Post(format!("Meta answered {s}{paren}")),
            StatusCode::UNAUTHORIZED => self.sign_out(),
            _ if code == 190 || code == 102 => self.sign_out(),
            // 4 is the app's limit, 17 the user's, 32 and 80001 a Page's, 80002 an Instagram account's and 613 a call's.
            _ if status == StatusCode::TOO_MANY_REQUESTS || matches!(code, 4 | 17 | 32 | 613 | 80001 | 80002) => {
                self.held(Hold { secs: DEFAULT_WAIT_SECS, app_wide: code == 4 });
                Failure::Account(RATE_LIMITED.into())
            }
            s => Failure::Post(match said {
                Some(m) => format!("Meta said: {m}"),
                None => format!("Meta answered {s}"),
            }),
        })
    }

    fn sign_out(&self) -> Failure {
        *self.signed_out.lock().unwrap() = true;
        Failure::Account(SIGNED_OUT.into())
    }

    /// Trades the code from the sign-in for the user's token, and that for a long-lived one, whose Pages' tokens don't expire.
    pub async fn exchange_code(&self, app: &OAuthApp, code: &str, redirect_uri: &str) -> Result<String, Failure> {
        let short = self.get("/oauth/access_token", &[("client_id", &app.client_id), ("client_secret", &app.client_secret), ("redirect_uri", redirect_uri), ("code", code)]).await?;
        let short = short["access_token"].as_str().ok_or_else(|| Failure::Account("Facebook didn't sign you in".into()))?;
        let long = [("grant_type", "fb_exchange_token"), ("client_id", &app.client_id), ("client_secret", &app.client_secret), ("fb_exchange_token", short)];
        let long = self.get("/oauth/access_token", &long).await?;
        long["access_token"].as_str().map(str::to_string).ok_or_else(|| Failure::Account("Facebook didn't let Splee stay signed in".into()))
    }

    /// Who the user's token belongs to: their id and name.
    pub async fn me(&self) -> Result<(String, String), Failure> {
        let me = self.get("/me", &[("fields", "id,name")]).await?;
        match (me["id"].as_str().filter(|id| is_id(id)), me["name"].as_str()) {
            (Some(id), Some(name)) => Ok((id.into(), name.into())),
            _ => Err(Failure::Account("Facebook didn't say who signed in".into())),
        }
    }

    /// The Pages the user's token manages. A company has a few, so the first 100 are plenty.
    pub async fn pages(&self) -> Result<Vec<Page>, Failure> {
        let fields = "id,name,username,access_token,tasks,instagram_business_account{id,username,name}";
        let found = self.get("/me/accounts", &[("fields", fields), ("limit", "100")]).await?;
        let mut pages = Vec::new();
        for p in found["data"].as_array().into_iter().flatten() {
            let (Some(id), Some(token)) = (p["id"].as_str().filter(|id| is_id(id)), p["access_token"].as_str()) else { continue };
            let tasks: Vec<&str> = p["tasks"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let ig = &p["instagram_business_account"];
            pages.push(Page {
                id: id.into(),
                name: p["name"].as_str().unwrap_or_default().into(),
                username: p["username"].as_str().unwrap_or_default().into(),
                token: token.into(),
                can_post: tasks.contains(&"CREATE_CONTENT"),
                can_moderate: tasks.contains(&"MODERATE"),
                instagram: ig["id"].as_str().filter(|id| is_id(id)).map(|id| Instagram {
                    id: id.into(),
                    username: ig["username"].as_str().unwrap_or_default().into(),
                    name: ig["name"].as_str().unwrap_or_default().into(),
                }),
            });
        }
        Ok(pages)
    }

    /// The link of a post or comment that just went out. It's out, so not getting the link back doesn't fail it.
    async fn permalink(&self, id: &str, field: &str) -> String {
        if !is_id(id) {
            return String::new();
        }
        self.get(&format!("/{id}"), &[("fields", field)]).await.ok().and_then(|v| v[field].as_str().map(str::to_string)).unwrap_or_default()
    }

    /// Posts on the Page, with a link preview when there's a link, and returns the post's link.
    pub async fn page_post(&self, page: &str, message: &str, link: &str) -> Result<String, Failure> {
        let mut form = vec![("message", message)];
        if !link.is_empty() {
            form.push(("link", link));
        }
        let made = self.post(&format!("/{page}/feed"), &form).await?;
        Ok(self.permalink(made["id"].as_str().unwrap_or_default(), "permalink_url").await)
    }

    /// Answers a comment on one of the Page's posts and returns the answer's link.
    pub async fn reply_facebook(&self, comment: &str, message: &str) -> Result<String, Failure> {
        let made = self.post(&format!("/{comment}/comments"), &[("message", message)]).await?;
        Ok(self.permalink(made["id"].as_str().unwrap_or_default(), "permalink_url").await)
    }

    /// Answers a comment on one of the account's posts. Instagram has no link for a reply, so this returns the comment's.
    pub async fn reply_instagram(&self, comment: &str, message: &str, link: &str) -> Result<String, Failure> {
        self.post(&format!("/{comment}/replies"), &[("message", message)]).await?;
        Ok(link.split_once('?').map_or(link, |(l, _)| l).to_string())
    }

    /// Makes a media container for the picture and caption, the first of Instagram's two steps.
    pub async fn container(&self, account: &str, image_url: &str, caption: &str) -> Result<String, Failure> {
        let made = self.post(&format!("/{account}/media"), &[("image_url", image_url), ("caption", caption)]).await?;
        made["id"].as_str().filter(|id| is_id(id)).map(str::to_string).ok_or_else(|| Failure::Post("Instagram didn't take the picture".into()))
    }

    /// Waits for the container to be ready, then publishes it. An error means it may or may not have gone out, so the
    /// container is kept for the next try to check.
    pub async fn publish(&self, account: &str, container: &str) -> Result<Published, Failure> {
        for check in 0..CONTAINER_CHECKS {
            let found = self.get(&format!("/{container}"), &[("fields", "status_code,status")]).await?;
            match found["status_code"].as_str().unwrap_or_default() {
                "FINISHED" => break,
                // Published by an earlier try that didn't hear back.
                "PUBLISHED" => return Ok(Published::Live(String::new())),
                "ERROR" | "EXPIRED" => {
                    let why = found["status"].as_str().filter(|s| !s.is_empty()).map(|s| format!(" ({s})")).unwrap_or_default();
                    return Ok(Published::Rejected(format!("Instagram couldn't use the picture{why}. Check that the image link opens a JPEG")));
                }
                _ if check + 1 == CONTAINER_CHECKS => return Ok(Published::Processing),
                _ => tokio::time::sleep(CONTAINER_WAIT).await,
            }
        }
        let published = self.post(&format!("/{account}/media_publish"), &[("creation_id", container)]).await?;
        Ok(Published::Live(self.permalink(published["id"].as_str().unwrap_or_default(), "permalink").await))
    }

    /// The Page's latest posts, each with its latest comments, and the Page's own answers under those.
    pub async fn page_comments(&self, page: &str) -> Result<Vec<Value>, Failure> {
        let fields = "id,message,permalink_url,created_time,comments.limit(25){id,message,from{id,name},created_time,can_comment,comments.limit(10){from{id}}}";
        Ok(self.get(&format!("/{page}/posts"), &[("fields", fields), ("limit", "10")]).await?["data"].as_array().cloned().unwrap_or_default())
    }

    /// The account's latest posts, each with its latest comments and who answered them.
    pub async fn media_comments(&self, account: &str) -> Result<Vec<Value>, Failure> {
        let fields = "id,caption,permalink,timestamp,comments.limit(25){id,text,username,timestamp,replies{username}}";
        Ok(self.get(&format!("/{account}/media"), &[("fields", fields), ("limit", "10")]).await?["data"].as_array().cloned().unwrap_or_default())
    }
}

/// Meta writes times like 2026-11-01T10:00:00+0000, which RFC 3339 doesn't allow.
pub fn time(t: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_str(t.as_str()?, "%Y-%m-%dT%H:%M:%S%z").ok().map(|t| t.with_timezone(&Utc))
}

/// Saves what a client learned from Meta: the wait its usage headers asked for, for every project when it's the app's, and that
/// Meta signed Splee out.
pub async fn settle(db: &PgPool, project_id: Uuid, graph: &Graph) {
    if let Some(hold) = graph.hold() {
        let held = sqlx::query("update project_settings set meta_held_until = now() + make_interval(secs => $2) where project_id = $1 or ($3 and meta_connected)")
            .bind(project_id)
            .bind(hold.secs)
            .bind(hold.app_wide)
            .execute(db)
            .await;
        if let Err(e) = held {
            tracing::warn!(error = %e, "couldn't save Meta's wait");
        }
    }
    if graph.signed_out()
        && let Err(e) = sign_out(db, project_id).await
    {
        tracing::warn!(error = %e, "couldn't sign the project out of Meta");
    }
}

/// Meta turned the token down, after the user changed their password, took Splee's access away or lost their role on the Page.
/// The Pages' tokens go and approved posts go back to drafts, like a Telegram chat the bot left, and the integrations page asks
/// to sign in again. A post being sent goes back too, which finish() then leaves alone.
pub async fn sign_out(db: &PgPool, project_id: Uuid) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query(
        "update project_settings set meta_user_id = '', facebook_error = $2, facebook_failed_at = now(), instagram_error = $2, instagram_failed_at = now()
         where project_id = $1",
    )
    .bind(project_id)
    .bind(SIGNED_OUT)
    .execute(&mut *tx)
    .await?;
    sqlx::query("update social_channels set token_enc = '' where project_id = $1 and platform in ('facebook', 'instagram')").bind(project_id).execute(&mut *tx).await?;
    sqlx::query(
        "update social_posts set status = 'draft', updated_at = now() where project_id = $1 and platform in ('facebook', 'instagram') and status in ('approved', 'posting')",
    )
    .bind(project_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Saves the Pages and Instagram accounts the user manages now, with their tokens, keeping which are picked. The ones they
/// don't manage anymore go, and their approved posts go back to drafts.
pub async fn save_pages(db: &PgPool, secret: &str, project_id: Uuid, pages: &[Page]) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    let mut kept: Vec<String> = Vec::new();
    for p in pages {
        let token = encrypt(secret, &p.token);
        let mut rows = vec![("facebook", p.id.as_str(), "", "", p.name.as_str(), p.username.as_str(), "page")];
        if let Some(ig) = &p.instagram {
            rows.push(("instagram", ig.id.as_str(), p.id.as_str(), p.name.as_str(), ig.name.as_str(), ig.username.as_str(), "instagram"));
        }
        for (platform, id, page, page_name, name, username, kind) in rows {
            sqlx::query(
                "insert into social_channels (project_id, platform, chat_id, guild_id, guild_name, name, kind, username, can_read, can_send, token_enc)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                 on conflict (project_id, platform, chat_id) do update set guild_id = excluded.guild_id, guild_name = excluded.guild_name,
                   name = excluded.name, kind = excluded.kind, username = excluded.username, can_read = excluded.can_read, can_send = excluded.can_send,
                   token_enc = excluded.token_enc",
            )
            .bind(project_id)
            .bind(platform)
            .bind(id)
            .bind(page)
            .bind(page_name)
            .bind(name)
            .bind(kind)
            .bind(username)
            .bind(p.can_moderate)
            .bind(p.can_post)
            .bind(&token)
            .execute(&mut *tx)
            .await?;
            kept.push(format!("{platform}:{id}"));
        }
    }
    let gone = "project_id = $1 and platform in ('facebook', 'instagram') and platform || ':' || chat_id <> all($2)";
    sqlx::query(&format!(
        "update social_posts set status = 'draft', updated_at = now() where status = 'approved' and channel_id in (select id from social_channels where {gone})"
    ))
    .bind(project_id)
    .bind(&kept)
    .execute(&mut *tx)
    .await?;
    sqlx::query(&format!("delete from social_channels where {gone}")).bind(project_id).bind(&kept).execute(&mut *tx).await?;
    tx.commit().await
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, RawForm, State};
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    use super::*;

    #[test]
    fn comment_links_keep_what_a_reply_needs() {
        let fb = facebook_comment_link("111_222", "222_333").unwrap();
        assert_eq!(fb, "https://www.facebook.com/111_222?comment_id=222_333");
        assert_eq!(facebook_comment(&fb), Some(("111".into(), "222_333".into())));
        assert_eq!(comment_url("https://m.facebook.com/111_222/?comment_id=222_333"), Some(fb));
        let ig = instagram_comment_link("https://www.instagram.com/reel/DCx-9_q/", "1790", "1784").unwrap();
        assert_eq!(ig, "https://www.instagram.com/p/DCx-9_q/c/1790/?account=1784");
        assert_eq!(instagram_comment(&ig), Some(("1784".into(), "1790".into())));
        assert_eq!(comment_url(&ig), Some(ig.clone()));
        for link in [
            "https://www.facebook.com/111?comment_id=222_333",
            "https://www.facebook.com/111_222?comment_id=abc",
            "https://evil.example/111_222?comment_id=222_333",
            "https://www.instagram.com/p/DCx/c/1790/",
            "https://www.instagram.com/p/DCx/c/1790/?account=x1",
            "http://www.instagram.com/p/DCx/c/1790/?account=1784",
        ] {
            assert_eq!(comment_url(link), None, "{link}");
        }
    }

    #[test]
    fn usage_over_ninety_percent_holds_for_as_long_as_meta_says() {
        assert_eq!(usage(Some(r#"{"call_count":28,"total_time":25,"total_cputime":25}"#), None), None);
        assert_eq!(usage(Some(r#"{"call_count":95,"total_time":25,"total_cputime":25}"#), None), Some(Hold { secs: DEFAULT_WAIT_SECS, app_wide: true }));
        let page = r#"{"111":[{"type":"pages","call_count":40,"total_cputime":92,"total_time":30,"estimated_time_to_regain_access":7}]}"#;
        assert_eq!(usage(None, Some(page)), Some(Hold { secs: 420.0, app_wide: false }));
        let calm = r#"{"111":[{"type":"pages","call_count":40,"total_cputime":12,"total_time":30,"estimated_time_to_regain_access":0}]}"#;
        assert_eq!(usage(Some("not json"), Some(calm)), None);
    }

    /// What the fake Graph API was asked to do, as "METHOD /path key=value ...", without the tokens.
    pub type Calls = Arc<Mutex<Vec<String>>>;

    #[derive(Clone)]
    struct Fake {
        calls: Calls,
        /// How often each container's status was read.
        checks: Arc<Mutex<HashMap<String, usize>>>,
    }

    fn params(raw: &[u8]) -> Vec<(String, String)> {
        Url::parse(&format!("http://fake/?{}", String::from_utf8_lossy(raw))).unwrap().query_pairs().into_owned().collect()
    }

    fn error(status: u16, code: i64, message: &str) -> Response {
        (axum::http::StatusCode::from_u16(status).unwrap(), Json(json!({ "error": { "message": message, "type": "OAuthException", "code": code } }))).into_response()
    }

    /// Meta's Graph API as its docs describe it. The token says how the account does: GOOD works, EXPIRED was revoked, BUSY is
    /// at 95% of a Page's limit. A post's text says how posting goes. RawForm is the query on a GET and the form on a POST.
    async fn graph(State(fake): State<Fake>, method: axum::http::Method, Path(path): Path<String>, RawForm(raw): RawForm) -> Response {
        let all = params(&raw);
        let get = |k: &str| all.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone()).unwrap_or_default();
        let token = get("access_token");
        let shown: Vec<String> = all.iter().filter(|(k, _)| k != "access_token" && k != "client_secret").map(|(k, v)| format!("{k}={v}")).collect();
        fake.calls.lock().unwrap().push(format!("{method} /{path} {}", shown.join(" ")).trim().to_string());
        let mut headers = HeaderMap::new();
        if token == "BUSY" {
            let usage = r#"{"111":[{"type":"pages","call_count":95,"total_cputime":10,"total_time":10,"estimated_time_to_regain_access":12}]}"#;
            headers.insert("x-business-use-case-usage", usage.parse().unwrap());
        }
        if path != "oauth/access_token" && token == "EXPIRED" {
            return error(400, 190, "Error validating access token: The session has been invalidated because the user changed their password.");
        }
        let ok = |v: Value| (headers.clone(), Json(v)).into_response();
        let segments: Vec<&str> = path.split('/').collect();
        match (method.as_str(), segments.as_slice()) {
            ("GET", ["oauth", "access_token"]) if get("code") == "the-code" => ok(json!({ "access_token": "SHORT", "token_type": "bearer", "expires_in": 3600 })),
            ("GET", ["oauth", "access_token"]) if get("fb_exchange_token") == "SHORT" => ok(json!({ "access_token": "USER-LONG", "token_type": "bearer" })),
            ("GET", ["oauth", "access_token"]) => error(400, 100, "Invalid verification code format."),
            ("GET", ["me"]) => ok(json!({ "id": "9001", "name": "Ana Lee" })),
            ("GET", ["me", "accounts"]) => ok(json!({ "data": [
                { "id": "111", "name": "Acme", "username": "acmehq", "access_token": "GOOD", "tasks": ["ANALYZE", "ADVERTISE", "MODERATE", "CREATE_CONTENT", "MANAGE"],
                  "instagram_business_account": { "id": "1784", "username": "acme.hq", "name": "Acme" } },
                { "id": "112", "name": "Acme Jobs", "access_token": "GOOD-2", "tasks": ["ANALYZE"] },
            ] })),
            ("POST", [page, "feed"]) => match get("message").as_str() {
                "fast" => error(400, 32, "Page request limit reached"),
                "down" => error(500, 1, "An unknown error has occurred."),
                _ => ok(json!({ "id": format!("{page}_5001") })),
            },
            ("POST", [comment, "comments"]) if *comment == "222_404" => error(400, 100, "Unsupported post request. Object with ID '222_404' does not exist"),
            ("POST", [_, "comments"]) => ok(json!({ "id": "222_6001" })),
            ("POST", [_, "replies"]) => ok(json!({ "id": "17999" })),
            ("POST", [account, "media"]) => match get("image_url").as_str() {
                "https://acme.test/slow.jpg" => ok(json!({ "id": "4002" })),
                "https://acme.test/broken.jpg" => ok(json!({ "id": "4003" })),
                "https://acme.test/lost.jpg" => ok(json!({ "id": "4004" })),
                _ => ok(json!({ "id": if *account == "1784" { "4001" } else { "0" } })),
            },
            ("POST", [_, "media_publish"]) if get("creation_id") == "4004" => error(500, 1, "An unknown error has occurred."),
            ("POST", [_, "media_publish"]) => ok(json!({ "id": format!("90{}", get("creation_id")) })),
            ("GET", [container]) if container.starts_with("400") => {
                let n = {
                    let mut checks = fake.checks.lock().unwrap();
                    let n = checks.entry(container.to_string()).or_default();
                    *n += 1;
                    *n
                };
                // 4001 is ready on the second check, 4002 never is, and 4003 fails.
                let status = match (*container, n) {
                    ("4001", 1) | ("4002", _) => "IN_PROGRESS",
                    ("4003", _) => "ERROR",
                    _ => "FINISHED",
                };
                ok(json!({ "id": container, "status_code": status, "status": if status == "ERROR" { "Error: Media download failed" } else { "" } }))
            }
            ("GET", [id]) if id.starts_with("90") => ok(json!({ "id": id, "permalink": format!("https://www.instagram.com/p/P{id}/") })),
            ("GET", [id]) => ok(json!({ "id": id, "permalink_url": format!("https://www.facebook.com/acmehq/posts/{id}") })),
            ("GET", [page, "posts"]) => ok(json!({ "data": [{
                "id": format!("{page}_222"), "message": "We shipped warmup reports", "permalink_url": "https://www.facebook.com/acmehq/posts/222",
                "created_time": "2026-11-10T10:00:00+0000",
                "comments": { "data": [
                    { "id": "222_333", "message": "Does it work with Gmail?", "from": { "id": "77", "name": "Bo Diaz" }, "created_time": "2026-11-11T09:00:00+0000", "can_comment": true },
                    { "id": "222_334", "message": "Thanks!", "from": { "id": "78", "name": "Cy Ng" }, "created_time": "2026-11-11T09:30:00+0000", "can_comment": true,
                      "comments": { "data": [{ "from": { "id": page } }] } },
                ] },
            }] })),
            ("GET", [_, "media"]) => ok(json!({ "data": [{
                "id": "9500", "caption": "Our new dashboard", "permalink": "https://www.instagram.com/p/DCx9q/", "timestamp": "2026-11-10T10:00:00+0000",
                "comments": { "data": [
                    { "id": "1790", "text": "How much is it?", "username": "bo.diaz", "timestamp": "2026-11-11T09:00:00+0000" },
                    { "id": "1791", "text": "Love it", "username": "cy", "timestamp": "2026-11-11T09:00:00+0000", "replies": { "data": [{ "username": "acme.hq" }] } },
                ] },
            }] })),
            _ => error(400, 100, "Unsupported request"),
        }
    }

    /// A fake Graph API on a local port, and what it was asked.
    pub async fn fake_graph() -> (String, Calls) {
        let fake = Fake { calls: Calls::default(), checks: Arc::default() };
        let app = Router::new().route("/{*path}", get(graph).post(graph)).with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, fake.calls)
    }

    #[tokio::test]
    async fn posting_replying_and_publishing_on_instagram_go_through_the_graph_api() {
        let (base, calls) = fake_graph().await;
        let g = Graph::new(&base, "GOOD").unwrap();
        assert_eq!(g.page_post("111", "Hello", "https://acme.test/blog").await.ok().unwrap(), "https://www.facebook.com/acmehq/posts/111_5001");
        assert_eq!(g.reply_facebook("222_333", "It does").await.ok().unwrap(), "https://www.facebook.com/acmehq/posts/222_6001");
        let link = "https://www.instagram.com/p/DCx9q/c/1790/?account=1784";
        assert_eq!(g.reply_instagram("1790", "From $9", link).await.ok().unwrap(), "https://www.instagram.com/p/DCx9q/c/1790/");

        // The container is still processing on the first check and ready on the second.
        let container = g.container("1784", "https://acme.test/team.jpg", "Our team").await.ok().unwrap();
        assert!(matches!(g.publish("1784", &container).await, Ok(Published::Live(url)) if url == "https://www.instagram.com/p/P904001/"));
        // One that stays in progress waits for a later try, and one Instagram couldn't use fails the post.
        let slow = g.container("1784", "https://acme.test/slow.jpg", "Slow").await.ok().unwrap();
        assert!(matches!(g.publish("1784", &slow).await, Ok(Published::Processing)));
        let broken = g.container("1784", "https://acme.test/broken.jpg", "Broken").await.ok().unwrap();
        assert!(matches!(g.publish("1784", &broken).await, Ok(Published::Rejected(e)) if e.contains("Media download failed")));

        let calls = calls.lock().unwrap().clone();
        assert!(calls.contains(&"POST /111/feed message=Hello link=https://acme.test/blog".to_string()));
        assert!(calls.contains(&"POST /222_333/comments message=It does".to_string()));
        assert!(calls.contains(&"POST /1790/replies message=From $9".to_string()));
        assert!(calls.contains(&"POST /1784/media image_url=https://acme.test/team.jpg caption=Our team".to_string()));
        assert_eq!(calls.iter().filter(|c| c.starts_with("POST /1784/media_publish")).collect::<Vec<_>>(), ["POST /1784/media_publish creation_id=4001"]);
        assert_eq!(calls.iter().filter(|c| c.starts_with("GET /4002")).count(), CONTAINER_CHECKS);
    }

    #[tokio::test]
    async fn failures_say_whose_problem_it_is_and_usage_stops_the_next_call() {
        let (base, calls) = fake_graph().await;
        let g = Graph::new(&base, "GOOD").unwrap();
        assert!(matches!(g.reply_facebook("222_404", "Hi").await, Err(Failure::Post(e)) if e.contains("does not exist")));
        assert!(matches!(g.page_post("111", "fast", "").await, Err(Failure::Account(e)) if e.contains("rate limiting")));
        assert_eq!(g.hold(), Some(Hold { secs: DEFAULT_WAIT_SECS, app_wide: false }));
        // It may have been saved before the error, so it fails the post.
        assert!(matches!(Graph::new(&base, "GOOD").unwrap().page_post("111", "down", "").await, Err(Failure::Post(_))));

        let expired = Graph::new(&base, "EXPIRED").unwrap();
        assert!(matches!(expired.page_post("111", "Hi", "").await, Err(Failure::Account(e)) if e.contains("Sign in with Facebook again")));
        assert!(expired.signed_out());

        // The answer goes through at 95%, and the next call waits without asking Meta.
        let busy = Graph::new(&base, "BUSY").unwrap();
        busy.page_post("111", "Hi", "").await.ok().unwrap();
        assert_eq!(busy.hold(), Some(Hold { secs: 720.0, app_wide: false }));
        let asked = calls.lock().unwrap().len();
        assert!(matches!(busy.reply_facebook("222_333", "Hi").await, Err(Failure::Account(e)) if e.contains("rate limiting")));
        assert_eq!(calls.lock().unwrap().len(), asked);
    }
}
