//! The Social page: replies and posts for X, Reddit, Bluesky, Mastodon, Threads, Discourse forums, Lemmy, Nostr, Discord,
//! Telegram, Facebook Pages, Instagram and LinkedIn, reblogs with a comment and posts for Tumblr, comments for YouTube, answers for Stack Exchange, articles
//! for DEV and the company's own blogs, and comments and articles for Hashnode, that a social run drafted or the user wrote,
//! which crate::social posts once approved, and connecting the accounts they go out from. Blogs connect in routes::blogs. LinkedIn goes out through the project's Unipile connection, see
//! routes::linkedin, Tumblr connects by signing in, see routes::tumblr, Facebook and Instagram too, see routes::meta, and the
//! Discord and Telegram bots in routes::bots.

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::settings::load_or_create;
use super::{util_input, util_scope};
use crate::agent::x::{self, Cookies};
use crate::agent::{self, kind};
use crate::auth::Auth;
use crate::blog;
use crate::bluesky::{self, Bluesky};
use crate::crypto;
use crate::dev::{self, Dev};
use crate::discourse::{self, Discourse};
use crate::error::{AppError, AppResult};
use crate::hashnode::{self, Hashnode};
use crate::lemmy::{self, Lemmy};
use crate::models::{Blog, DiscourseForum, ProjectSettings, SocialChannel, SocialPost};
use crate::mastodon::{self, Mastodon};
use crate::nostr;
use crate::reddit::{self, Login, Reddit};
use crate::social::{self, Failure, PLATFORMS, length, max_length, min_length, takes_posts};
use crate::stackexchange::{self, StackExchange};
use crate::state::AppState;
use crate::threads::Threads;
use crate::youtube::{self, YouTube};
use crate::{hooks, tumblr};

const STATUSES: [&str; 6] = ["draft", "approved", "posting", "posted", "failed", "skipped"];
/// Plenty for the few communities a company's buyers gather in, and it bounds a social run's searches.
const MAX_FORUMS: i64 = 10;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/social", get(list).post(create))
        .route("/projects/{id}/social/runs", post(start_run))
        .route("/social/{id}", patch(update).delete(remove))
        .route("/projects/{id}/integrations/x", put(connect_x).delete(disconnect_x))
        .route("/projects/{id}/integrations/reddit", put(connect_reddit).delete(disconnect_reddit))
        .route("/projects/{id}/integrations/bluesky", put(connect_bluesky).delete(disconnect_bluesky))
        .route("/projects/{id}/integrations/mastodon", put(connect_mastodon).delete(disconnect_mastodon))
        .route("/projects/{id}/integrations/threads", put(connect_threads).delete(disconnect_threads))
        .route("/projects/{id}/integrations/youtube", put(connect_youtube).delete(disconnect_youtube))
        .route("/projects/{id}/integrations/stackexchange", put(connect_stackexchange).delete(disconnect_stackexchange))
        .route("/projects/{id}/integrations/dev", put(connect_dev).delete(disconnect_dev))
        .route("/projects/{id}/integrations/lemmy", put(connect_lemmy).delete(disconnect_lemmy))
        .route("/projects/{id}/integrations/nostr", put(connect_nostr).delete(disconnect_nostr))
        .route("/projects/{id}/integrations/hashnode", put(connect_hashnode).delete(disconnect_hashnode))
        .route("/projects/{id}/integrations/hashnode/publication", put(pick_publication))
        .route("/projects/{id}/discourse-forums", get(list_forums).post(add_forum))
        .route("/discourse-forums/{id}", delete(remove_forum))
}

#[derive(Deserialize)]
struct ListQuery {
    /// One status or several, comma-separated.
    status: Option<String>,
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(q): Query<ListQuery>) -> AppResult<Json<Vec<SocialPost>>> {
    auth.project(&state.db, id).await?;
    let statuses: Vec<&str> = q.status.as_deref().unwrap_or_default().split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    for s in &statuses {
        util_input::one_of("status", s, &STATUSES)?;
    }
    let posts = sqlx::query_as::<_, SocialPost>(
        "select * from social_posts where project_id = $1 and (cardinality($2::text[]) = 0 or status = any($2))
         order by coalesce(posted_at, created_at) desc limit 500",
    )
    .bind(id)
    .bind(&statuses)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(posts))
}

/// What a post needs before it can go out: text within the platform's limits, a subreddit and title for a new Reddit post, a
/// community and title for a new Lemmy post, a title for a DEV or Hashnode article and a Discourse topic, and something to
/// answer for YouTube and Stack Exchange. `subreddit` is the Lemmy community for Lemmy.
fn check(platform: &str, reply: bool, subreddit: &str, title: &str, body: &str, mastodon_chars: usize) -> AppResult<()> {
    if body.trim().is_empty() {
        return Err(AppError::BadRequest("Write something first".into()));
    }
    if !reply && !takes_posts(platform) {
        let what = if platform == "stackexchange" { "answers" } else { "comments" };
        return Err(AppError::BadRequest(format!("{} only takes {what}", social::name(platform))));
    }
    let what = match (platform, reply) {
        ("stackexchange", _) => "Answers",
        ("dev" | "hashnode" | "blog", false) => "Articles",
        ("discourse", false) => "Topics",
        ("nostr", _) => "Notes",
        ("discord" | "telegram", _) => "Messages",
        ("instagram", false) => "Captions",
        ("tumblr", true) => "Reblog comments",
        ("linkedin" | "youtube" | "lemmy" | "hashnode", true) => "Comments",
        (_, true) => "Replies",
        _ => "Posts",
    };
    let (min, max, n) = (min_length(platform), max_length(platform, reply, mastodon_chars), length(platform, body.trim()));
    if n > max {
        return Err(AppError::BadRequest(format!("{what} on {} are at most {max} characters", social::name(platform))));
    }
    if n < min {
        return Err(AppError::BadRequest(format!("{what} on {} are at least {min} characters", social::name(platform))));
    }
    if platform == "reddit" && !reply && (subreddit.is_empty() || title.trim().is_empty()) {
        return Err(AppError::BadRequest("A Reddit post needs a subreddit and a title".into()));
    }
    if platform == "lemmy" && !reply {
        if subreddit.is_empty() || title.trim().is_empty() {
            return Err(AppError::BadRequest("A Lemmy post needs a community and a title".into()));
        }
        if title.chars().count() > lemmy::TITLE_CHARS {
            return Err(AppError::BadRequest(format!("Lemmy titles are at most {} characters", lemmy::TITLE_CHARS)));
        }
    }
    if platform == "discourse" && !reply && !(discourse::MIN_TITLE_CHARS..=discourse::TITLE_CHARS).contains(&title.trim().chars().count()) {
        return Err(AppError::BadRequest(format!("Discourse titles are {} to {} characters", discourse::MIN_TITLE_CHARS, discourse::TITLE_CHARS)));
    }
    if let ("dev" | "blog", _) | ("hashnode", false) = (platform, reply) {
        let (max, needs, titles) = match platform {
            "dev" => (dev::TITLE_CHARS, "A DEV article needs a title", "DEV titles"),
            "hashnode" => (hashnode::TITLE_CHARS, "A Hashnode article needs a title", "Hashnode titles"),
            _ => (blog::TITLE_CHARS, "An article needs a title", "Blog titles"),
        };
        if title.trim().is_empty() {
            return Err(AppError::BadRequest(needs.into()));
        }
        if title.chars().count() > max {
            return Err(AppError::BadRequest(format!("{titles} are at most {max} characters")));
        }
    }
    if title.chars().count() > 300 {
        return Err(AppError::BadRequest("Reddit titles are at most 300 characters".into()));
    }
    Ok(())
}

/// A Facebook post's link and an Instagram post's picture, which have to be links anyone can open. The picture is required.
fn check_media(platform: &str, reply: bool, link: &str, image_url: &str) -> AppResult<()> {
    let public = crate::meta::public_link;
    match (platform, reply) {
        ("facebook", false) if !link.is_empty() && !public(link) => Err(AppError::BadRequest("The link has to start with https://".into())),
        ("instagram", false) if image_url.is_empty() => Err(AppError::BadRequest("An Instagram post needs a picture. Add a link to a JPEG".into())),
        ("instagram", false) if !public(image_url) => Err(AppError::BadRequest("The picture's link has to start with https://".into())),
        _ => Ok(()),
    }
}

/// The channel a post goes to: a bot's channel or group, a Page or an Instagram account.
fn has_channel(platform: &str) -> bool {
    matches!(platform, "discord" | "telegram" | "facebook" | "instagram")
}

/// Reddit, Lemmy and Discourse original posts and DEV, Hashnode and blog articles have a title.
fn titled(platform: &str, reply: bool) -> bool {
    matches!(platform, "reddit" | "dev" | "lemmy" | "discourse" | "hashnode" | "blog") && !reply
}

/// DEV, Hashnode and blog articles and Tumblr posts have tags, kept as each platform takes them.
fn tags(platform: &str, reply: bool, raw: &[String]) -> Option<Vec<String>> {
    match (platform, reply) {
        ("dev", _) => Some(dev::tags(raw)),
        ("blog", _) => Some(blog::tags(raw)),
        ("hashnode", false) => Some(hashnode::tags(raw)),
        ("tumblr", false) => Some(tumblr::tags(raw)),
        _ => None,
    }
}

/// The project's Mastodon instance's post limit, for `check`.
async fn mastodon_chars(state: &AppState, project_id: Uuid) -> AppResult<usize> {
    let max: Option<i32> = sqlx::query_scalar("select mastodon_max_chars from project_settings where project_id = $1").bind(project_id).fetch_optional(&state.db).await?;
    Ok(max.unwrap_or(500).max(1) as usize)
}

/// A subreddit as typed, like "r/sales", or empty.
fn subreddit_input(raw: &str) -> AppResult<String> {
    if raw.trim().is_empty() {
        return Ok(String::new());
    }
    reddit::subreddit(raw).ok_or_else(|| AppError::BadRequest(format!("{} isn't a subreddit name", raw.trim())))
}

/// Where a new Reddit or Lemmy post goes, as typed: a subreddit like "r/sales", or a Lemmy community like
/// "!sales@lemmy.world". Empty when nothing was typed.
async fn community_input(state: &AppState, project_id: Uuid, platform: &str, raw: &str) -> AppResult<String> {
    match platform {
        "reddit" => subreddit_input(raw),
        "lemmy" if !raw.trim().is_empty() => {
            let home: Option<String> = sqlx::query_scalar("select lemmy_url from project_settings where project_id = $1").bind(project_id).fetch_optional(&state.db).await?;
            let home = home.unwrap_or_default();
            lemmy::community(raw, home.trim_start_matches("https://"))
                .ok_or_else(|| AppError::BadRequest(format!("{} isn't a Lemmy community. Write it like sales@lemmy.world", raw.trim())))
        }
        _ => Ok(String::new()),
    }
}

/// A Discourse post goes to one of the project's forums, and a new topic needs the category picked for that forum.
async fn require_forum(state: &AppState, project_id: Uuid, forum_id: Option<Uuid>, reply: bool) -> AppResult<()> {
    let forum: Option<DiscourseForum> = match forum_id {
        Some(id) => sqlx::query_as("select * from discourse_forums where id = $1 and project_id = $2").bind(id).bind(project_id).fetch_optional(&state.db).await?,
        None => None,
    };
    match forum {
        None => Err(AppError::BadRequest("Connect the forum on the integrations page first".into())),
        Some(f) if !reply && f.category_id.is_none() => Err(AppError::BadRequest(format!("Pick a category for new topics on {} first", f.url))),
        Some(_) => Ok(()),
    }
}

/// A blog article goes to one of the project's blogs, and one that's approved to a blog that isn't paused.
async fn require_blog(state: &AppState, project_id: Uuid, blog_id: Option<Uuid>, approving: bool) -> AppResult<()> {
    let blog: Option<Blog> = match blog_id {
        Some(id) => sqlx::query_as("select * from blogs where id = $1 and project_id = $2").bind(id).bind(project_id).fetch_optional(&state.db).await?,
        None => None,
    };
    match blog {
        None => Err(AppError::BadRequest("Connect the blog on the integrations page first".into())),
        Some(b) if b.paused && approving => Err(AppError::BadRequest(format!("{} turned Splee down. Connect it again on the integrations page first", blog::name(&b.kind)))),
        Some(_) => Ok(()),
    }
}

/// Ghost turns down longer excerpts.
fn check_excerpt(excerpt: &str) -> AppResult<()> {
    if excerpt.chars().count() > blog::EXCERPT_CHARS {
        return Err(AppError::BadRequest(format!("Excerpts are at most {} characters", blog::EXCERPT_CHARS)));
    }
    Ok(())
}

/// A Discord or Telegram post goes to a picked channel the bot can post in, and a Facebook or Instagram one to a picked Page or
/// account the user who signed in may post on, or answer comments on for a reply.
async fn require_channel(state: &AppState, project_id: Uuid, platform: &str, channel_id: Option<Uuid>, reply: bool) -> AppResult<()> {
    let channel: Option<SocialChannel> = match channel_id {
        Some(id) => {
            let sql = "select * from social_channels where id = $1 and project_id = $2 and platform = $3";
            sqlx::query_as(sql).bind(id).bind(project_id).bind(platform).fetch_optional(&state.db).await?
        }
        None => None,
    };
    let meta = matches!(platform, "facebook" | "instagram");
    match channel {
        None if platform == "facebook" => Err(AppError::BadRequest("Pick a Facebook Page on the integrations page first".into())),
        None if platform == "instagram" => Err(AppError::BadRequest("Pick an Instagram account on the integrations page first".into())),
        None => Err(AppError::BadRequest(format!("Pick a {} channel on the integrations page first", social::name(platform)))),
        Some(c) if !c.picked => Err(AppError::BadRequest(format!("Pick {} on the integrations page first", c.label()))),
        Some(c) if meta && c.token_enc.is_empty() => Err(AppError::BadRequest("Facebook signed Splee out. Sign in with Facebook again on the integrations page".into())),
        Some(c) if meta && reply && !c.can_read => Err(AppError::BadRequest(format!("Your role on {} doesn't let you answer comments there", c.label()))),
        Some(c) if meta && !reply && !c.can_send => Err(AppError::BadRequest(format!("Your role on {} doesn't let you post there", c.label()))),
        Some(_) if meta => Ok(()),
        Some(c) if !c.can_send => {
            Err(AppError::BadRequest(format!("The bot can't post in {}. Check its permissions there, then refresh the list on the integrations page", c.label())))
        }
        Some(_) => Ok(()),
    }
}

/// Approving a post for an account that isn't connected, or a Hashnode article with no publication to go in, would leave it
/// waiting with no word on why.
async fn require_account(state: &AppState, project_id: Uuid, platform: &str, reply: bool) -> AppResult<()> {
    let accounts = social::accounts(&state.db, project_id).await?;
    let connected = accounts.as_ref().is_some_and(|a| a.connected().contains(&platform));
    match (connected, platform) {
        (true, "hashnode") if !reply && accounts.is_some_and(|a| a.hashnode_publication_id.is_empty()) => {
            Err(AppError::BadRequest("Pick a Hashnode publication for articles on the integrations page first".into()))
        }
        (true, _) => Ok(()),
        (false, "linkedin") => Err(AppError::BadRequest("Connect LinkedIn sending on the integrations page first".into())),
        (false, p) => Err(AppError::BadRequest(format!("Connect {} on the integrations page first", social::name(p)))),
    }
}

#[derive(Deserialize)]
struct CreateBody {
    platform: String,
    body: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    subreddit: String,
    #[serde(default)]
    tags: Vec<String>,
    /// Discourse only: the forum the topic goes to.
    forum_id: Option<Uuid>,
    /// Discord and Telegram only: the channel the post goes to. Facebook and Instagram: the Page or account.
    channel_id: Option<Uuid>,
    /// Facebook only, optional.
    #[serde(default)]
    link: String,
    /// Instagram only, required.
    #[serde(default)]
    image_url: String,
    /// Blog articles only: the blog, and an optional excerpt.
    blog_id: Option<Uuid>,
    #[serde(default)]
    excerpt: String,
    /// "draft" (the default) or "approved".
    status: Option<String>,
}

/// A post the user wrote by hand.
async fn create(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<CreateBody>) -> AppResult<Json<SocialPost>> {
    auth.project(&state.db, id).await?;
    util_input::one_of("platform", &b.platform, &PLATFORMS)?;
    let status = b.status.as_deref().unwrap_or("draft");
    util_input::one_of("status", status, &["draft", "approved"])?;
    let subreddit = community_input(&state, id, &b.platform, &b.subreddit).await?;
    let title = if titled(&b.platform, false) { b.title.trim() } else { "" };
    let tags = tags(&b.platform, false, &b.tags).unwrap_or_default();
    let forum_id = if b.platform == "discourse" { b.forum_id } else { None };
    let channel_id = if has_channel(&b.platform) { b.channel_id } else { None };
    let link = if b.platform == "facebook" { b.link.trim() } else { "" };
    let image_url = if b.platform == "instagram" { b.image_url.trim() } else { "" };
    let (blog_id, excerpt) = if b.platform == "blog" { (b.blog_id, b.excerpt.trim()) } else { (None, "") };
    check(&b.platform, false, &subreddit, title, &b.body, mastodon_chars(&state, id).await?)?;
    check_media(&b.platform, false, link, image_url)?;
    check_excerpt(excerpt)?;
    if b.platform == "discourse" {
        require_forum(&state, id, forum_id, false).await?;
    }
    if b.platform == "blog" {
        require_blog(&state, id, blog_id, status == "approved").await?;
    }
    if has_channel(&b.platform) {
        require_channel(&state, id, &b.platform, channel_id, false).await?;
    }
    // require_blog checked a blog's article already.
    if status == "approved" && b.platform != "blog" {
        require_account(&state, id, &b.platform, false).await?;
    }
    let post = sqlx::query_as::<_, SocialPost>(
        "insert into social_posts (project_id, platform, subreddit, title, tags, body, status, forum_id, channel_id, link, image_url, blog_id, excerpt)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) returning *",
    )
    .bind(id)
    .bind(&b.platform)
    .bind(subreddit)
    .bind(title)
    .bind(tags)
    .bind(b.body.trim())
    .bind(status)
    .bind(forum_id)
    .bind(channel_id)
    .bind(link)
    .bind(image_url)
    .bind(blog_id)
    .bind(excerpt)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(post))
}

#[derive(Deserialize)]
struct UpdateBody {
    body: Option<String>,
    title: Option<String>,
    subreddit: Option<String>,
    tags: Option<Vec<String>>,
    link: Option<String>,
    image_url: Option<String>,
    excerpt: Option<String>,
    status: Option<String>,
}

async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<UpdateBody>) -> AppResult<Json<SocialPost>> {
    let post: SocialPost = util_scope::load(&state.db, "social_posts", id, auth.org_id).await?;
    if post.status == "posted" || post.status == "posting" {
        return Err(AppError::BadRequest("This is already posted".into()));
    }
    let status = b.status.unwrap_or_else(|| post.status.clone());
    if status != post.status {
        util_input::one_of("status", &status, &["draft", "approved", "skipped"])?;
    }
    let reply = post.in_reply_to.is_some();
    let body = b.body.map(|s| s.trim().to_string()).unwrap_or(post.body);
    let subreddit = match (post.platform.as_str(), b.subreddit, reply) {
        ("reddit" | "lemmy", Some(s), false) => community_input(&state, post.project_id, &post.platform, &s).await?,
        _ => post.subreddit,
    };
    let title = if titled(&post.platform, reply) { b.title.map(|t| t.trim().to_string()).unwrap_or(post.title) } else { post.title };
    let tags = b.tags.and_then(|raw| tags(&post.platform, reply, &raw)).unwrap_or(post.tags);
    let link = match (post.platform.as_str(), b.link, reply) {
        ("facebook", Some(l), false) => l.trim().to_string(),
        _ => post.link,
    };
    let image_url = match (post.platform.as_str(), b.image_url, reply) {
        ("instagram", Some(l), false) => l.trim().to_string(),
        _ => post.image_url,
    };
    let excerpt = match (post.platform.as_str(), b.excerpt) {
        ("blog", Some(e)) => e.trim().to_string(),
        _ => post.excerpt,
    };
    check_excerpt(&excerpt)?;
    if status == "approved" {
        check(&post.platform, reply, &subreddit, &title, &body, mastodon_chars(&state, post.project_id).await?)?;
        check_media(&post.platform, reply, &link, &image_url)?;
        if post.platform == "discourse" {
            require_forum(&state, post.project_id, post.forum_id, reply).await?;
        }
        if has_channel(&post.platform) {
            require_channel(&state, post.project_id, &post.platform, post.channel_id, reply).await?;
        }
        if post.platform == "blog" {
            require_blog(&state, post.project_id, post.blog_id, true).await?;
        } else {
            require_account(&state, post.project_id, &post.platform, reply).await?;
        }
    }
    let updated = sqlx::query_as::<_, SocialPost>(
        "update social_posts set body = $2, title = $3, subreddit = $4, tags = $5, status = $6, link = $7, image_url = $8, excerpt = $9,
           error = case when $6 = status then error end, updated_at = now(),
           container_id = case when body = $2 and image_url = $8 then container_id else '' end
         where id = $1 and status not in ('posting', 'posted') returning *",
    )
    .bind(id)
    .bind(body)
    .bind(title)
    .bind(subreddit)
    .bind(tags)
    .bind(&status)
    .bind(link)
    .bind(image_url)
    .bind(excerpt)
    .fetch_optional(&state.db)
    .await?
    // The poster took it in the meantime.
    .ok_or_else(|| AppError::BadRequest("This is already posted".into()))?;
    Ok(Json(updated))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let post: SocialPost = util_scope::load(&state.db, "social_posts", id, auth.org_id).await?;
    if post.status != "draft" {
        return Err(AppError::BadRequest("Only drafts can be deleted. Skip it instead".into()));
    }
    sqlx::query("delete from social_posts where id = $1 and status = 'draft'").bind(id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn start_run(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    if social::accounts(&state.db, id).await?.is_none_or(|a| a.connected().is_empty()) {
        return Err(AppError::BadRequest(
            "Connect X, Reddit, Bluesky, Mastodon, Threads, YouTube, Stack Exchange, DEV, a Discourse forum, Lemmy, Nostr, Tumblr, Hashnode, Discord, \
             Telegram, Facebook, Instagram, your blog or LinkedIn sending on the integrations page first"
                .into(),
        ));
    }
    let busy: bool = sqlx::query_scalar("select exists (select 1 from agent_runs where project_id = $1 and kind = $2 and status in ('queued', 'running'))")
        .bind(id)
        .bind(kind::SOCIAL)
        .fetch_one(&state.db)
        .await?;
    if busy {
        return Err(AppError::Conflict("The agent is already looking for threads. Try again when it's done.".into()));
    }
    let run_id = agent::enqueue(&state.db, kind::SOCIAL, Some(id), Some(auth.org_id), json!({})).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

#[derive(Deserialize)]
struct XBody {
    auth_token: String,
    ct0: String,
}

/// Saves the cookies once bird signs in with them, with the handle they belong to.
async fn connect_x(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<XBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let bird = state.cfg.bird_bin.as_deref().ok_or_else(|| AppError::BadRequest("Posting to X needs the bird CLI on the server. Set BIRD_BIN".into()))?;
    let cookies = Cookies { auth_token: b.auth_token.trim().to_string(), ct0: b.ct0.trim().to_string() };
    if cookies.auth_token.is_empty() || cookies.ct0.is_empty() {
        return Err(AppError::BadRequest("Paste both auth_token and ct0".into()));
    }
    let handle = x::whoami(bird, &cookies)
        .await
        .map_err(|e| AppError::BadRequest(format!("X didn't accept those cookies ({e:#}). Copy them again from a browser that's signed in to x.com")))?;
    let secret = &state.cfg.app_secret;
    set_x(&state, id, &crypto::encrypt(secret, &cookies.auth_token), &crypto::encrypt(secret, &cookies.ct0), &handle).await
}

async fn disconnect_x(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_x(&state, id, "", "", "").await
}

async fn set_x(state: &AppState, id: Uuid, token: &str, ct0: &str, handle: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set x_auth_token_enc = $2, x_ct0_enc = $3, x_handle = $4, x_error = null, x_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(token)
    .bind(ct0)
    .bind(handle)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct RedditBody {
    client_id: String,
    secret: String,
    username: String,
    password: String,
}

/// Saves the login once Reddit signs it in, with the username as Reddit spells it.
async fn connect_reddit(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<RedditBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let username = b.username.trim().trim_start_matches('/');
    // Passwords can start or end with a space, so only the other fields are trimmed.
    let login = Login {
        client_id: b.client_id.trim().into(),
        secret: b.secret.trim().into(),
        username: username.strip_prefix("u/").unwrap_or(username).into(),
        password: b.password,
    };
    if login.client_id.is_empty() || login.secret.is_empty() || login.username.is_empty() || login.password.is_empty() {
        return Err(AppError::BadRequest("Fill in the client id, secret, username and password".into()));
    }
    let refused = |e: Failure| AppError::BadRequest(e.message());
    let name = Reddit::sign_in(&login).await.map_err(refused)?.me().await.map_err(refused)?;
    let secret = &state.cfg.app_secret;
    let enc = (crypto::encrypt(secret, &login.secret), crypto::encrypt(secret, &login.password));
    set_reddit(&state, id, &login.client_id, &enc.0, &name, &enc.1).await
}

async fn disconnect_reddit(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_reddit(&state, id, "", "", "", "").await
}

async fn set_reddit(state: &AppState, id: Uuid, client_id: &str, secret: &str, username: &str, password: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set reddit_client_id = $2, reddit_secret_enc = $3, reddit_username = $4, reddit_password_enc = $5,
           reddit_error = null, reddit_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(client_id)
    .bind(secret)
    .bind(username)
    .bind(password)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct BlueskyBody {
    handle: String,
    password: String,
}

/// Saves the app password once Bluesky signs in with it, with the handle as Bluesky spells it.
async fn connect_bluesky(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<BlueskyBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let login = bluesky::Login { handle: b.handle.trim().trim_start_matches('@').to_string(), password: b.password.trim().to_string() };
    if login.handle.is_empty() || login.password.is_empty() {
        return Err(AppError::BadRequest("Fill in the handle and app password".into()));
    }
    let session = Bluesky::sign_in(&login).await.map_err(|e| AppError::BadRequest(e.message()))?;
    set_bluesky(&state, id, &session.handle, &crypto::encrypt(&state.cfg.app_secret, &login.password)).await
}

async fn disconnect_bluesky(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_bluesky(&state, id, "", "").await
}

async fn set_bluesky(state: &AppState, id: Uuid, handle: &str, password: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set bluesky_handle = $2, bluesky_password_enc = $3, bluesky_error = null, bluesky_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(handle)
    .bind(password)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct MastodonBody {
    url: String,
    token: String,
}

/// Saves the token once the instance accepts it, with the account's handle and the instance's post limit.
async fn connect_mastodon(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<MastodonBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let base = mastodon::instance_url(&b.url).ok_or_else(|| AppError::BadRequest("Write the instance's address, like mastodon.social".into()))?;
    let token = b.token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("Paste the access token".into()));
    }
    let account = Mastodon::new(&base, token).await.map_err(|e| AppError::BadRequest(e.message()))?.me().await.map_err(|e| AppError::BadRequest(e.message()))?;
    let enc = crypto::encrypt(&state.cfg.app_secret, token);
    set_mastodon(&state, id, &base, &enc, &account.handle, account.max_chars).await
}

async fn disconnect_mastodon(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_mastodon(&state, id, "", "", "", 500).await
}

async fn set_mastodon(state: &AppState, id: Uuid, url: &str, token: &str, handle: &str, max_chars: i32) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set mastodon_url = $2, mastodon_token_enc = $3, mastodon_handle = $4, mastodon_max_chars = $5,
           mastodon_error = null, mastodon_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(url)
    .bind(token)
    .bind(handle)
    .bind(max_chars)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct ThreadsBody {
    token: String,
}

/// Saves the token once Threads accepts it, with the account's user id and username.
async fn connect_threads(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<ThreadsBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let token = b.token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("Paste the access token".into()));
    }
    let account = Threads::new(token, "").map_err(|e| AppError::BadRequest(e.message()))?.me().await.map_err(|e| AppError::BadRequest(e.message()))?;
    set_threads(&state, id, &crypto::encrypt(&state.cfg.app_secret, token), &account.id, &account.username).await
}

async fn disconnect_threads(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_threads(&state, id, "", "", "").await
}

async fn set_threads(state: &AppState, id: Uuid, token: &str, user_id: &str, username: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set threads_token_enc = $2, threads_user_id = $3, threads_username = $4, threads_error = null, threads_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(token)
    .bind(user_id)
    .bind(username)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct YouTubeBody {
    client_id: String,
    secret: String,
    refresh_token: String,
}

/// Saves the OAuth client and refresh token once Google trades it for an access token, with the channel it belongs to.
async fn connect_youtube(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<YouTubeBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let login = youtube::Login { client_id: b.client_id.trim().into(), secret: b.secret.trim().into(), refresh_token: b.refresh_token.trim().into() };
    if login.client_id.is_empty() || login.secret.is_empty() || login.refresh_token.is_empty() {
        return Err(AppError::BadRequest("Fill in the client id, client secret and refresh token".into()));
    }
    let refused = |e: Failure| AppError::BadRequest(e.message());
    let channel = YouTube::sign_in(&login).await.map_err(refused)?.me().await.map_err(refused)?;
    let secret = &state.cfg.app_secret;
    let enc = (crypto::encrypt(secret, &login.secret), crypto::encrypt(secret, &login.refresh_token));
    set_youtube(&state, id, &login.client_id, &enc.0, &enc.1, &channel.id, &channel.name).await
}

async fn disconnect_youtube(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_youtube(&state, id, "", "", "", "", "").await
}

async fn set_youtube(state: &AppState, id: Uuid, client_id: &str, secret: &str, token: &str, channel_id: &str, channel: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set youtube_client_id = $2, youtube_secret_enc = $3, youtube_token_enc = $4, youtube_channel_id = $5,
           youtube_channel = $6, youtube_error = null, youtube_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(client_id)
    .bind(secret)
    .bind(token)
    .bind(channel_id)
    .bind(channel)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct StackExchangeBody {
    key: String,
    token: String,
    site: String,
}

/// Saves the key and token once Stack Exchange says the token can write and has a user on the site, with that user.
async fn connect_stackexchange(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<StackExchangeBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let (key, token) = (b.key.trim(), b.token.trim());
    if key.is_empty() || token.is_empty() || b.site.trim().is_empty() {
        return Err(AppError::BadRequest("Fill in the key, access token and site".into()));
    }
    let site = stackexchange::site_input(&b.site).ok_or_else(|| AppError::BadRequest(format!("{} isn't a Stack Exchange site", b.site.trim())))?;
    let refused = |e: Failure| AppError::BadRequest(e.message());
    let se = StackExchange::new(key, token, &site).map_err(refused)?;
    if !se.can_write().await.map_err(refused)? {
        return Err(AppError::BadRequest("The access token can't post answers. Make one with the write_access scope".into()));
    }
    let user = se.me().await.map_err(refused)?;
    let enc = crypto::encrypt(&state.cfg.app_secret, token);
    set_stackexchange(&state, id, key, &enc, &user.site, &user.account, &user.name).await
}

async fn disconnect_stackexchange(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_stackexchange(&state, id, "", "", "", "", "").await
}

async fn set_stackexchange(state: &AppState, id: Uuid, key: &str, token: &str, site: &str, account: &str, user: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set stackexchange_key = $2, stackexchange_token_enc = $3, stackexchange_site = $4, stackexchange_account = $5,
           stackexchange_user = $6, stackexchange_error = null, stackexchange_failed_at = null, stackexchange_backoff_until = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(key)
    .bind(token)
    .bind(site)
    .bind(account)
    .bind(user)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct DevBody {
    key: String,
}

/// Saves the API key once DEV accepts it, with the account's username.
async fn connect_dev(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<DevBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let key = b.key.trim();
    if key.is_empty() {
        return Err(AppError::BadRequest("Paste the API key".into()));
    }
    let username = Dev::new(key).map_err(|e| AppError::BadRequest(e.message()))?.me().await.map_err(|e| AppError::BadRequest(e.message()))?;
    set_dev(&state, id, &crypto::encrypt(&state.cfg.app_secret, key), &username).await
}

async fn disconnect_dev(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_dev(&state, id, "", "").await
}

async fn set_dev(state: &AppState, id: Uuid, key: &str, username: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set dev_key_enc = $2, dev_username = $3, dev_error = null, dev_failed_at = null where project_id = $1 returning *",
    )
    .bind(id)
    .bind(key)
    .bind(username)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct LemmyBody {
    url: String,
    username: String,
    password: String,
}

/// Saves the login once the instance signs it in, with the JWT it gave and the account as name@instance.
async fn connect_lemmy(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<LemmyBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let base = mastodon::instance_url(&b.url).ok_or_else(|| AppError::BadRequest("Write the instance's address, like lemmy.world".into()))?;
    // Passwords can start or end with a space, so only the username is trimmed.
    let username = b.username.trim().trim_start_matches('@');
    if username.is_empty() || b.password.is_empty() {
        return Err(AppError::BadRequest("Fill in the username and password".into()));
    }
    let refused = |e: Failure| AppError::BadRequest(e.message());
    let lemmy = Lemmy::new(&base, username, &b.password, "").await.map_err(refused)?;
    let handle = lemmy.me().await.map_err(refused)?;
    let secret = &state.cfg.app_secret;
    let enc = (crypto::encrypt(secret, &b.password), crypto::encrypt(secret, &lemmy.jwt()));
    set_lemmy(&state, id, &base, username, &enc.0, &enc.1, &handle).await
}

async fn disconnect_lemmy(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_lemmy(&state, id, "", "", "", "", "").await
}

async fn set_lemmy(state: &AppState, id: Uuid, url: &str, username: &str, password: &str, jwt: &str, handle: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set lemmy_url = $2, lemmy_username = $3, lemmy_password_enc = $4, lemmy_jwt_enc = $5, lemmy_handle = $6,
           lemmy_error = null, lemmy_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(url)
    .bind(username)
    .bind(password)
    .bind(jwt)
    .bind(handle)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct NostrBody {
    key: String,
    /// Empty for the defaults.
    #[serde(default)]
    relays: Vec<String>,
}

/// Saves the key once it reads as one, with its npub and the relays, each of which has to be wss:// at a public address.
/// Nothing is posted to check it, since a key can't be wrong the way a password can.
async fn connect_nostr(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<NostrBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    if b.key.trim().starts_with("npub") {
        return Err(AppError::BadRequest("That's the public key. Paste the secret key, which starts with nsec".into()));
    }
    let key = nostr::secret_key(&b.key).ok_or_else(|| AppError::BadRequest("Paste the secret key, an nsec or 64 hex characters".into()))?;
    let relays = nostr::relay_list(&b.relays).map_err(AppError::BadRequest)?;
    for relay in &relays {
        let url = reqwest::Url::parse(relay).map_err(|e| AppError::BadRequest(e.to_string()))?;
        hooks::public_addrs(&url).await.map_err(|e| AppError::BadRequest(format!("Can't use {relay} ({e:#})")))?;
    }
    let pubkey = nostr::pubkey(&key);
    let enc = crypto::encrypt(&state.cfg.app_secret, &hex::encode(key.to_bytes()));
    set_nostr(&state, id, &enc, &pubkey, &nostr::npub(&pubkey), &relays).await
}

async fn disconnect_nostr(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_nostr(&state, id, "", "", "", &[]).await
}

async fn set_nostr(state: &AppState, id: Uuid, key: &str, pubkey: &str, npub: &str, relays: &[String]) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set nostr_key_enc = $2, nostr_pubkey = $3, nostr_npub = $4, nostr_relays = $5, nostr_error = null, nostr_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(key)
    .bind(pubkey)
    .bind(npub)
    .bind(relays)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct HashnodeBody {
    token: String,
}

/// Saves the token once Hashnode accepts it, with the account's username and publications. Articles go in the first one until
/// another is picked, and an account without one only comments.
async fn connect_hashnode(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<HashnodeBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let token = b.token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("Paste the access token".into()));
    }
    let refused = |e: Failure| AppError::BadRequest(e.message());
    let account = Hashnode::new(token).map_err(refused)?.me().await.map_err(refused)?;
    let first = account.publications.first().and_then(|p| p["id"].as_str()).unwrap_or_default().to_string();
    let enc = crypto::encrypt(&state.cfg.app_secret, token);
    set_hashnode(&state, id, &enc, &account.username, json!(account.publications), &first).await
}

async fn disconnect_hashnode(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_hashnode(&state, id, "", "", json!([]), "").await
}

async fn set_hashnode(state: &AppState, id: Uuid, token: &str, username: &str, publications: Value, publication: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set hashnode_token_enc = $2, hashnode_username = $3, hashnode_publications = $4, hashnode_publication_id = $5,
           hashnode_error = null, hashnode_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(token)
    .bind(username)
    .bind(publications)
    .bind(publication)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct PublicationBody {
    publication_id: String,
}

/// Picks which of the account's publications articles go in.
async fn pick_publication(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<PublicationBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set hashnode_publication_id = $2
         where project_id = $1 and hashnode_publications @> jsonb_build_array(jsonb_build_object('id', $2::text)) returning *",
    )
    .bind(id)
    .bind(b.publication_id.trim())
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::BadRequest("That isn't one of the account's publications".into()))?;
    Ok(Json(settings))
}

async fn list_forums(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<DiscourseForum>>> {
    auth.project(&state.db, id).await?;
    let forums = sqlx::query_as("select * from discourse_forums where project_id = $1 order by created_at").bind(id).fetch_all(&state.db).await?;
    Ok(Json(forums))
}

#[derive(Deserialize)]
struct ForumBody {
    url: String,
    key: String,
    /// Only for an API key an admin made, which acts as this user. A User API Key knows its user.
    #[serde(default)]
    username: String,
    /// Where new topics go: the category's name, slug or link. Empty for replies only.
    #[serde(default)]
    category: String,
}

/// Adds a forum once its key works, with the user the key posts as. Adding the same user on the same forum again replaces
/// the key and the category, and keeps the drafts for it.
async fn add_forum(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<ForumBody>) -> AppResult<Json<DiscourseForum>> {
    auth.project(&state.db, id).await?;
    let url = discourse::forum_url(&b.url).ok_or_else(|| AppError::BadRequest("Write the forum's address, like forum.example.com".into()))?;
    let (key, username) = (b.key.trim(), b.username.trim().trim_start_matches('@'));
    if key.is_empty() {
        return Err(AppError::BadRequest("Paste the API key".into()));
    }
    let refused = |e: Failure| AppError::BadRequest(e.message());
    let forum = Discourse::new(&url, key, (!username.is_empty()).then_some(username)).await.map_err(refused)?;
    let me = forum.me().await.map_err(refused)?;
    let category = match b.category.trim() {
        "" => None,
        wanted => {
            let categories = forum.categories().await.map_err(refused)?;
            let found = discourse::find_category(&categories, wanted)
                .ok_or_else(|| AppError::BadRequest(format!("{url} has no category called {wanted} that {me} can see")))?;
            // 1 is Discourse's "create" permission. Replying only, or seeing only, are 2 and 3.
            if found["permission"].as_i64().is_some_and(|p| p != 1) {
                return Err(AppError::BadRequest(format!("{me} can't start topics in {}", found["name"].as_str().unwrap_or(wanted))));
            }
            Some((found["id"].as_i64().unwrap_or_default(), found["name"].as_str().unwrap_or(wanted).to_string()))
        }
    };
    let account = discourse::account(&url, &me);
    let others: i64 = sqlx::query_scalar("select count(*) from discourse_forums where project_id = $1 and account <> $2").bind(id).bind(&account).fetch_one(&state.db).await?;
    if others >= MAX_FORUMS {
        return Err(AppError::BadRequest(format!("A project can join up to {MAX_FORUMS} forums")));
    }
    let added = sqlx::query_as::<_, DiscourseForum>(
        "insert into discourse_forums (project_id, url, username, key_kind, key_enc, account, category_id, category_name)
         values ($1, $2, $3, $4, $5, $6, $7, $8)
         on conflict (project_id, account) do update set username = excluded.username, key_kind = excluded.key_kind, key_enc = excluded.key_enc,
           category_id = excluded.category_id, category_name = excluded.category_name, error = null, failed_at = null
         returning *",
    )
    .bind(id)
    .bind(&url)
    .bind(&me)
    .bind(if username.is_empty() { "user" } else { "admin" })
    .bind(crypto::encrypt(&state.cfg.app_secret, key))
    .bind(&account)
    .bind(category.as_ref().map(|c| c.0))
    .bind(category.map(|c| c.1).unwrap_or_default())
    .fetch_one(&state.db)
    .await?;
    Ok(Json(added))
}

/// The forum's approved posts go back to drafts, since adding the forum again makes a new row they don't point at.
async fn remove_forum(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let forum: DiscourseForum = util_scope::load(&state.db, "discourse_forums", id, auth.org_id).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("update social_posts set status = 'draft', updated_at = now() where forum_id = $1 and status = 'approved'").bind(forum.id).execute(&mut *tx).await?;
    sqlx::query("delete from discourse_forums where id = $1").bind(forum.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlx::PgPool;

    use super::*;
    use crate::crypto::sha256_hex;

    #[test]
    fn hashnode_articles_need_a_title_and_tags_follow_each_platform() {
        let body = "Warm the domain up over four weeks.";
        assert!(check("hashnode", false, "", "How we warm up", body, 500).is_ok());
        assert!(matches!(check("hashnode", false, "", "", body, 500), Err(AppError::BadRequest(m)) if m == "A Hashnode article needs a title"));
        assert!(matches!(check("hashnode", false, "", &"a".repeat(251), body, 500), Err(AppError::BadRequest(m)) if m == "Hashnode titles are at most 250 characters"));
        assert!(check("hashnode", true, "", "", "Same here", 500).is_ok());
        let long = check("tumblr", true, "", "", &"a".repeat(4_097), 500);
        assert!(matches!(long, Err(AppError::BadRequest(m)) if m == "Reblog comments on Tumblr are at most 4096 characters"));
        let raw = ["#Cold Email".to_string(), "sales".to_string()];
        assert_eq!(tags("tumblr", false, &raw), Some(vec!["Cold Email".to_string(), "sales".to_string()]));
        assert_eq!(tags("hashnode", false, &raw), Some(vec!["cold-email".to_string(), "sales".to_string()]));
        assert_eq!((tags("tumblr", true, &raw), tags("nostr", false, &raw)), (None, None));
    }

    /// Ana's project in Acme, and another org's project with a Hashnode draft.
    async fn server(db: &PgPool) -> (String, Uuid, Uuid, Uuid) {
        let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('ana@acme.test', 'Ana', 'x') returning id").fetch_one(db).await.unwrap();
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(db).await.unwrap();
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(user).execute(db).await.unwrap();
        sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
            .bind(sha256_hex("session-acme"))
            .bind(user)
            .bind(org)
            .execute(db)
            .await
            .unwrap();
        let ours: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(db).await.unwrap();
        let other: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Other') returning id").fetch_one(db).await.unwrap();
        let theirs: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Other') returning id").bind(other).fetch_one(db).await.unwrap();
        let draft: Uuid = sqlx::query_scalar("insert into social_posts (project_id, platform, title, body) values ($1, 'hashnode', 'Hi there', 'Hi') returning id")
            .bind(theirs)
            .fetch_one(db)
            .await
            .unwrap();
        let fake = crate::mail::oauth::tests::FakeProvider::start().await;
        let state = AppState { db: db.clone(), cfg: Arc::new(crate::mail::oauth::tests::config(&fake)), events: tokio::sync::broadcast::channel(1).0 };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", crate::routes::router()).with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await });
        (base, ours, theirs, draft)
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn nostr_takes_public_wss_relays_and_other_orgs_projects_are_refused(db: PgPool) {
        let (base, ours, theirs, draft) = server(&db).await;
        let client = reqwest::Client::new();
        let put = |project: Uuid, body: Value| {
            client.put(format!("{base}/projects/{project}/integrations/nostr")).header("cookie", "splee_session=session-acme").json(&body).send()
        };
        let nsec = "nsec1kls4zc52a54x40m3tzqfea8nca3ww9s08z6d5448snvsg5vselhsjv8uxn";
        // Addresses as IPs, so nothing is looked up.
        let res = put(ours, json!({ "key": nsec, "relays": ["wss://203.0.113.7", "1.1.1.1"] })).await.unwrap();
        assert_eq!(res.status(), 400, "203.0.113.0/24 is for documentation, not a public relay");
        let res = put(ours, json!({ "key": nsec, "relays": ["wss://10.0.0.5"] })).await.unwrap();
        assert!(res.text().await.unwrap().contains("not a public address"));
        let res = put(ours, json!({ "key": "npub1mlcawle2vuw97dscxundkg6phev0atsa5t0vakzrys8hk5pt5evssm7a0a" })).await.unwrap();
        assert!(res.text().await.unwrap().contains("That's the public key"));

        let res = put(ours, json!({ "key": nsec, "relays": ["1.1.1.1\nwss://8.8.8.8"] })).await.unwrap();
        assert_eq!(res.status(), 200);
        let settings: Value = res.json().await.unwrap();
        assert_eq!((settings["nostr_connected"].clone(), settings["nostr_relays"].clone()), (json!(true), json!(["wss://1.1.1.1", "wss://8.8.8.8"])));
        assert_eq!(settings["nostr_npub"], "npub1mlcawle2vuw97dscxundkg6phev0atsa5t0vakzrys8hk5pt5evssm7a0a");
        assert!(settings.get("nostr_key_enc").is_none(), "the key stays on the server");
        let saved: String = sqlx::query_scalar("select nostr_key_enc from project_settings where project_id = $1").bind(ours).fetch_one(&db).await.unwrap();
        assert_eq!(crypto::decrypt("test-secret", &saved).unwrap(), "b7e151628aed2a6abf7158809cf4f3c762e7160f38b4da56a784d9045190cfef");

        // Another org's project and its drafts are out of reach.
        assert_eq!(put(theirs, json!({ "key": nsec })).await.unwrap().status(), 404);
        let hashnode = client.put(format!("{base}/projects/{theirs}/integrations/hashnode")).header("cookie", "splee_session=session-acme").json(&json!({ "token": "x" }));
        assert_eq!(hashnode.send().await.unwrap().status(), 404);
        let approve = client.patch(format!("{base}/social/{draft}")).header("cookie", "splee_session=session-acme").json(&json!({ "status": "approved" }));
        assert_eq!(approve.send().await.unwrap().status(), 404);

        // An approved Hashnode article needs a publication, and a Nostr note just the key.
        let create = |body: Value| client.post(format!("{base}/projects/{ours}/social")).header("cookie", "splee_session=session-acme").json(&body).send();
        sqlx::query("update project_settings set hashnode_token_enc = 'x', hashnode_username = 'acme' where project_id = $1").bind(ours).execute(&db).await.unwrap();
        let article = create(json!({ "platform": "hashnode", "title": "How we warm up", "body": "Four weeks", "status": "approved" })).await.unwrap();
        assert!(article.text().await.unwrap().contains("Pick a Hashnode publication"));
        assert_eq!(create(json!({ "platform": "nostr", "body": "Four weeks", "status": "approved" })).await.unwrap().status(), 200);
        let tumblr = create(json!({ "platform": "tumblr", "body": "Four weeks", "status": "approved" })).await.unwrap();
        assert!(tumblr.text().await.unwrap().contains("Connect Tumblr"));
    }

    #[test]
    fn youtube_takes_comments_only_and_threads_stops_at_500_characters() {
        assert!(check("youtube", true, "", "", "Same here", 500).is_ok());
        assert!(matches!(check("youtube", false, "", "", "A lesson", 500), Err(AppError::BadRequest(m)) if m == "YouTube only takes comments"));
        assert!(check("threads", false, "", "", &"a".repeat(500), 500).is_ok());
        assert!(matches!(check("threads", true, "", "", &"a".repeat(501), 500), Err(AppError::BadRequest(m)) if m == "Replies on Threads are at most 500 characters"));
    }

    #[test]
    fn stack_exchange_takes_answers_only_and_dev_articles_need_a_title() {
        let answer = "Set the pool size to the number of cores.";
        assert!(check("stackexchange", true, "", "", answer, 500).is_ok());
        assert!(matches!(check("stackexchange", false, "", "", answer, 500), Err(AppError::BadRequest(m)) if m == "Stack Exchange only takes answers"));
        let short = check("stackexchange", true, "", "", "Use a pool.", 500);
        assert!(matches!(short, Err(AppError::BadRequest(m)) if m == "Answers on Stack Exchange are at least 30 characters"));
        assert!(check("dev", false, "", "Warming up a domain", "# Day one", 500).is_ok());
        assert!(matches!(check("dev", false, "", " ", "# Day one", 500), Err(AppError::BadRequest(m)) if m == "A DEV article needs a title"));
        assert!(matches!(check("dev", false, "", &"a".repeat(129), "Hi", 500), Err(AppError::BadRequest(m)) if m == "DEV titles are at most 128 characters"));
    }

    #[test]
    fn lemmy_posts_need_a_community_and_discourse_topics_a_long_enough_title() {
        let body = "Warm the domain up over four weeks.";
        assert!(check("lemmy", false, "sales@lemmy.world", "What works?", body, 500).is_ok());
        assert!(matches!(check("lemmy", false, "", "What works?", body, 500), Err(AppError::BadRequest(m)) if m == "A Lemmy post needs a community and a title"));
        assert!(matches!(check("lemmy", false, "sales@lemmy.world", &"a".repeat(201), body, 500), Err(AppError::BadRequest(m)) if m.contains("at most 200")));
        assert!(check("lemmy", true, "", "", "Same", 500).is_ok());
        assert!(check("discourse", false, "", "How we warm up domains", body, 500).is_ok());
        assert!(matches!(check("discourse", false, "", "Warm up", body, 500), Err(AppError::BadRequest(m)) if m == "Discourse titles are 15 to 255 characters"));
        let short = check("discourse", true, "", "", "Same here", 500);
        assert!(matches!(short, Err(AppError::BadRequest(m)) if m == "Replies on Discourse are at least 20 characters"));
    }
}
