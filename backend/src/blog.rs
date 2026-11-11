//! The company's own blogs on WordPress, Ghost and Webflow, where approved articles go in as drafts for the user to publish
//! there, or published, as each blog is set. Articles are Markdown, turned into HTML here. WordPress sites and Ghost's admin
//! are addresses the user typed, so requests only go to a public address (see hooks::public_client). Each blog takes its
//! weekly cap of articles, counted by its account in any project, one a tick. agent::social drafts them.
//!
//! A request that timed out may still have made the post, so each article gets a slug before its first try, and every try
//! looks for a post with that slug before it makes one.

use std::collections::HashSet;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::Config;
use crate::crypto::decrypt;
use crate::hooks::{self, Alert};
use crate::models::{Blog, SocialPost};
use crate::social::{self, Failure};
use crate::state::AppState;

pub const KINDS: [&str; 3] = ["wordpress", "ghost", "webflow"];
pub const TITLE_CHARS: usize = 200;
pub const MAX_TAGS: usize = 5;
/// Ghost's limit on a custom excerpt. The others take it too.
pub const EXCERPT_CHARS: usize = 300;
const WEBFLOW_API: &str = "https://api.webflow.com/v2";
/// A try still `posting` after this long was cut off by a crash or a restart.
const STUCK_MINUTES: i32 = 15;

pub fn name(kind: &str) -> &'static str {
    match kind {
        "wordpress" => "WordPress",
        "ghost" => "Ghost",
        _ => "Webflow",
    }
}

#[derive(Debug)]
pub enum Fault {
    /// The blog turned the credentials down, or the account can't post there. The blog pauses until it's connected again.
    Refused(String),
    /// A rate limit, a timeout or a server error. The article waits an hour, and since it may have gone through, the next try
    /// looks for it first.
    Later(String),
    /// Something about this article, like a title the blog turns down. It fails with the reason.
    Article(String),
}

impl Fault {
    pub fn message(self) -> String {
        match self {
            Fault::Refused(m) | Fault::Later(m) | Fault::Article(m) => m,
        }
    }
}

/// The blog's address from what the user typed: https, with the path the blog lives under and no query. WordPress's
/// wp-admin and wp-json and Ghost's admin pages are cut off, so a link to any of them works too.
pub fn site_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let url = Url::parse(&if raw.contains("://") { raw.to_string() } else { format!("https://{raw}") }).ok()?;
    let host = url.host_str()?;
    // The tests' fake blogs run on plain http on this machine. hooks::public_client still turns away anything else there.
    let fake = cfg!(test) && host == "127.0.0.1";
    if !(url.scheme() == "https" || fake) || !host.contains('.') || !url.username().is_empty() {
        return None;
    }
    let ends = ["wp-admin", "wp-json", "wp-login.php", "ghost"];
    let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).take_while(|s| !ends.contains(s)).collect();
    let path = if path.is_empty() { String::new() } else { format!("/{}", path.join("/")) };
    Some(format!("{}{path}", url.origin().ascii_serialization()))
}

/// What the weekly cap counts a WordPress or Ghost blog by, in any project: its address without the scheme, lowercase.
pub fn account(url: &str) -> String {
    url.split_once("://").map_or(url, |(_, rest)| rest).to_lowercase()
}

fn host(url: &str) -> &str {
    url.split_once("://").map_or(url, |(_, rest)| rest)
}

/// Tags as typed, without # or repeats in any case, at most 40 characters and 5 of them.
pub fn tags(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in raw {
        let tag = tag.trim().trim_start_matches('#').split_whitespace().collect::<Vec<_>>().join(" ");
        if !tag.is_empty() && tag.chars().count() <= 40 && !out.iter().any(|t| t.to_lowercase() == tag.to_lowercase()) {
            out.push(tag);
        }
    }
    out.truncate(MAX_TAGS);
    out
}

/// The slug Splee picks for an article: the title's words, cut to 60 characters, then the start of the article's id, so no
/// other post on the blog has it and looking it up finds only this article.
pub fn slug(title: &str, id: Uuid) -> String {
    let words = title.to_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-");
    let words: String = words.chars().take(60).collect();
    let short = &id.simple().to_string()[..8];
    match words.trim_end_matches('-') {
        "" => format!("post-{short}"),
        words => format!("{words}-{short}"),
    }
}

/// The article's Markdown as HTML. HTML written in the Markdown comes out as text, and links that aren't http, mailto or
/// relative lose their target, since an article drafted from strangers' posts shouldn't put markup or scripts on the
/// company's site.
pub fn html(markdown: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, Tag};
    let safe = |url: &str| {
        let url = url.trim().to_lowercase();
        !url.split(['/', '?', '#']).next().unwrap_or_default().contains(':') || ["http:", "https:", "mailto:"].iter().any(|s| url.starts_with(s))
    };
    let events = Parser::new_ext(markdown, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) if !safe(&dest_url) => Event::Start(Tag::Link { link_type, dest_url: "".into(), title, id }),
        Event::Start(Tag::Image { link_type, dest_url, title, id }) if !safe(&dest_url) => Event::Start(Tag::Image { link_type, dest_url: "".into(), title, id }),
        event => event,
    });
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, events);
    out
}

/// The entities WordPress keeps site titles and tag names with.
fn unescape(s: &str) -> String {
    s.replace("&#039;", "'").replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// The address with its query, since reqwest here is built without its own.
fn at(url: &str, query: &[(&str, &str)]) -> Result<Url, Fault> {
    Url::parse_with_params(url, query).map_err(|e| Fault::Article(e.to_string()))
}

fn said(v: &Value) -> String {
    v.as_str().unwrap_or_default().trim().to_string()
}

/// Sends a request to the blog. `message` reads the blog's own error from its answer, and `refused` is what to tell the user
/// when it turns the credentials down.
async fn send(request: RequestBuilder, site: &str, message: fn(&Value) -> String, refused: &str) -> Result<Value, Fault> {
    let res = request.header("Accept", "application/json").send().await;
    let res = res.map_err(|e| Fault::Later(format!("{site} didn't answer: {}", e.without_url())))?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    let said = message(&body);
    let paren = if said.is_empty() { String::new() } else { format!(" ({said})") };
    Err(match status {
        s if s.is_success() => return Ok(body),
        // A server error may come after the post was saved, so the next try looks for it first.
        s if s.is_server_error() => Fault::Later(format!("{site} answered {s}{paren}")),
        StatusCode::UNAUTHORIZED => Fault::Refused(refused.into()),
        StatusCode::FORBIDDEN => Fault::Refused(format!("{site} won't let the account do that{paren}. Check its role there, then connect the blog again")),
        StatusCode::TOO_MANY_REQUESTS => Fault::Later(format!("{site} is rate limiting the account")),
        _ if !said.is_empty() => Fault::Article(said),
        s => Fault::Article(format!("{site} answered {s}")),
    })
}

/// What reading a blog before saving it found.
pub struct Site {
    pub name: String,
    /// Who the WordPress application password belongs to. Empty on Ghost and Webflow, whose keys aren't a person's.
    pub user: String,
}

/// A post the blog has: where to open it, and whether it's a draft there.
#[derive(Debug, PartialEq)]
pub struct Made {
    pub url: String,
    pub draft: bool,
}

pub struct Article<'a> {
    pub title: &'a str,
    pub html: &'a str,
    pub slug: &'a str,
    pub tags: &'a [String],
    pub excerpt: &'a str,
}

/// WordPress's REST API, self-hosted or on a WordPress.com Business plan, with a username and an application password.
pub struct WordPress {
    client: reqwest::Client,
    site: String,
    username: String,
    password: String,
}

impl WordPress {
    pub async fn new(site: &str, username: &str, password: &str) -> Result<Self, Fault> {
        let url = Url::parse(site).map_err(|e| Fault::Refused(e.to_string()))?;
        let client = hooks::public_client(&url).await.map_err(|e| Fault::Later(format!("Couldn't reach {} ({e:#})", host(site))))?;
        Ok(Self { client, site: site.into(), username: username.into(), password: password.into() })
    }

    fn api(&self, path: &str) -> String {
        format!("{}/wp-json/wp/v2/{path}", self.site)
    }

    async fn call(&self, request: RequestBuilder) -> Result<Value, Fault> {
        let refused = format!("{} didn't accept the username and application password. Make a new application password and connect the blog again", host(&self.site));
        send(request.basic_auth(&self.username, Some(&self.password)), host(&self.site), |b| said(&b["message"]), &refused).await
    }

    /// The site's title and who the password belongs to, which checks it, and that they may publish.
    pub async fn check(&self) -> Result<Site, Fault> {
        let me = self.call(self.client.get(at(&self.api("users/me"), &[("context", "edit")])?)).await?;
        let user = me["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(&self.username).to_string();
        if me["capabilities"]["publish_posts"] != true {
            return Err(Fault::Refused(format!("{user} can't publish posts on {}. Give them the Author role or higher", host(&self.site))));
        }
        let index = self.call(self.client.get(format!("{}/wp-json/", self.site))).await?;
        Ok(Site { name: unescape(index["name"].as_str().unwrap_or_default()), user })
    }

    /// A draft opens in the editor, since its link only works for someone signed in.
    fn made(&self, post: &Value) -> Made {
        let draft = !matches!(post["status"].as_str(), Some("publish" | "future"));
        let url = match post["link"].as_str() {
            Some(link) if !draft => link.to_string(),
            _ => format!("{}/wp-admin/post.php?post={}&action=edit", self.site, post["id"]),
        };
        Made { url, draft }
    }

    async fn find(&self, slug: &str) -> Result<Option<Made>, Fault> {
        let query = [("slug", slug), ("status", "publish,future,draft,pending,private"), ("context", "edit"), ("_fields", "id,link,status")];
        let found = self.call(self.client.get(at(&self.api("posts"), &query)?)).await?;
        Ok(found.as_array().and_then(|posts| posts.first()).map(|p| self.made(p)))
    }

    /// The tags' ids, making the ones the site doesn't have yet. A tag WordPress won't make is left off rather than failing the
    /// article.
    async fn tag_ids(&self, tags: &[String]) -> Result<Vec<i64>, Fault> {
        let mut ids = Vec::new();
        for tag in tags {
            let query = [("search", tag.as_str()), ("per_page", "100"), ("_fields", "id,name")];
            let found = self.call(self.client.get(at(&self.api("tags"), &query)?)).await?;
            let same = |t: &&Value| t["name"].as_str().is_some_and(|n| unescape(n).to_lowercase() == tag.to_lowercase());
            let id = match found.as_array().and_then(|all| all.iter().find(same)) {
                Some(t) => t["id"].as_i64(),
                None => match self.call(self.client.post(self.api("tags")).json(&json!({ "name": tag }))).await {
                    Ok(made) => made["id"].as_i64(),
                    Err(Fault::Article(_)) => None,
                    Err(e) => return Err(e),
                },
            };
            ids.extend(id);
        }
        Ok(ids)
    }

    async fn create(&self, a: &Article<'_>, publish: bool) -> Result<Made, Fault> {
        let tags = self.tag_ids(a.tags).await?;
        let status = if publish { "publish" } else { "draft" };
        let post = json!({ "title": a.title, "content": a.html, "excerpt": a.excerpt, "slug": a.slug, "status": status, "tags": tags });
        let made = self.call(self.client.post(self.api("posts")).json(&post)).await?;
        Ok(self.made(&made))
    }
}

/// An Admin API key is {id}:{secret}, with the secret in hex.
pub fn ghost_key(raw: &str) -> Option<(String, Vec<u8>)> {
    let (id, secret) = raw.trim().split_once(':')?;
    let secret = hex::decode(secret).ok().filter(|s| !s.is_empty())?;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())).then(|| (id.to_string(), secret))
}

/// The token Ghost's Admin API takes: a JWT signed with the key's secret, for /admin/, good for five minutes, Ghost's most.
pub fn ghost_token(id: &str, secret: &[u8], now: i64) -> String {
    let header = URL_SAFE_NO_PAD.encode(json!({ "alg": "HS256", "typ": "JWT", "kid": id }).to_string());
    let claims = URL_SAFE_NO_PAD.encode(json!({ "iat": now, "exp": now + 300, "aud": "/admin/" }).to_string());
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes a key of any length");
    mac.update(format!("{header}.{claims}").as_bytes());
    format!("{header}.{claims}.{}", URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

/// Ghost's Admin API, with the Admin API key of a custom integration.
pub struct Ghost {
    client: reqwest::Client,
    admin: String,
    id: String,
    secret: Vec<u8>,
}

impl Ghost {
    pub async fn new(admin: &str, key: &str) -> Result<Self, Fault> {
        let (id, secret) = ghost_key(key).ok_or_else(|| Fault::Refused("That isn't an Admin API key. It looks like an id, a colon, then 64 letters and digits".into()))?;
        let url = Url::parse(admin).map_err(|e| Fault::Refused(e.to_string()))?;
        let client = hooks::public_client(&url).await.map_err(|e| Fault::Later(format!("Couldn't reach {} ({e:#})", host(admin))))?;
        Ok(Self { client, admin: admin.into(), id, secret })
    }

    fn api(&self, path: &str) -> String {
        format!("{}/ghost/api/admin/{path}", self.admin)
    }

    async fn call(&self, request: RequestBuilder) -> Result<Value, Fault> {
        let token = ghost_token(&self.id, &self.secret, chrono::Utc::now().timestamp());
        let request = request.header("Authorization", format!("Ghost {token}")).header("Accept-Version", "v5.0");
        let refused = "Ghost didn't accept the Admin API key. Copy it again from the custom integration and connect the blog again";
        send(request, host(&self.admin), |b| said(&b["errors"][0]["message"]), refused).await
    }

    pub async fn check(&self) -> Result<Site, Fault> {
        let site = self.call(self.client.get(self.api("site/"))).await?;
        // Anyone can read the site's title, so reading a post is what checks the key.
        self.call(self.client.get(at(&self.api("posts/"), &[("limit", "1"), ("fields", "id")])?)).await?;
        Ok(Site { name: said(&site["site"]["title"]), user: String::new() })
    }

    /// A draft opens in Ghost's editor, since it has no public page yet.
    fn made(&self, post: &Value) -> Made {
        let draft = !matches!(post["status"].as_str(), Some("published" | "scheduled"));
        let url = match post["url"].as_str() {
            Some(url) if !draft => url.to_string(),
            _ => format!("{}/ghost/#/editor/post/{}", self.admin, post["id"].as_str().unwrap_or_default()),
        };
        Made { url, draft }
    }

    async fn find(&self, slug: &str) -> Result<Option<Made>, Fault> {
        let filter = format!("slug:{slug}");
        let found = self.call(self.client.get(at(&self.api("posts/"), &[("filter", &filter), ("fields", "id,url,status"), ("limit", "1")])?)).await?;
        Ok(found["posts"].as_array().and_then(|posts| posts.first()).map(|p| self.made(p)))
    }

    async fn create(&self, a: &Article<'_>, publish: bool) -> Result<Made, Fault> {
        let tags: Vec<Value> = a.tags.iter().map(|t| json!({ "name": t })).collect();
        let excerpt = Some(a.excerpt).filter(|e| !e.is_empty());
        let post = json!({ "title": a.title, "html": a.html, "slug": a.slug, "status": if publish { "published" } else { "draft" }, "tags": tags,
                           "custom_excerpt": excerpt });
        let made = self.call(self.client.post(at(&self.api("posts/"), &[("source", "html")])?).json(&json!({ "posts": [post] }))).await?;
        Ok(self.made(&made["posts"][0]))
    }
}

/// A Webflow site the token can see, with its public address: its first custom domain, else its webflow.io one.
pub struct WebflowSite {
    pub id: String,
    pub name: String,
    pub url: String,
}

/// Webflow's Data API v2, with a site API token that can read sites and read and write the CMS.
pub struct Webflow {
    client: reqwest::Client,
    api: String,
    token: String,
}

impl Webflow {
    /// `api` is Config::webflow_api_url, empty for Webflow itself.
    pub fn new(api: &str, token: &str) -> Result<Self, Fault> {
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().map_err(|e| Fault::Later(e.to_string()))?;
        Ok(Self { client, api: if api.is_empty() { WEBFLOW_API.into() } else { api.into() }, token: token.into() })
    }

    async fn call(&self, request: RequestBuilder) -> Result<Value, Fault> {
        let refused = "Webflow didn't accept the API token. Make a new one and connect the blog again";
        send(request.bearer_auth(&self.token), "Webflow", |b| said(&b["message"]), refused).await
    }

    pub async fn sites(&self) -> Result<Vec<WebflowSite>, Fault> {
        let found = self.call(self.client.get(format!("{}/sites", self.api))).await?;
        let sites = found["sites"].as_array().cloned().unwrap_or_default();
        Ok(sites
            .iter()
            .map(|s| {
                let domain = s["customDomains"][0]["url"].as_str().map(String::from);
                let domain = domain.unwrap_or_else(|| format!("{}.webflow.io", s["shortName"].as_str().unwrap_or_default()));
                WebflowSite { id: said(&s["id"]), name: said(&s["displayName"]), url: format!("https://{}", domain.trim_start_matches("https://")) }
            })
            .collect())
    }

    /// The site's collections, as [{id, displayName, slug}].
    pub async fn collections(&self, site: &str) -> Result<Vec<Value>, Fault> {
        let found = self.call(self.client.get(format!("{}/sites/{site}/collections", self.api))).await?;
        Ok(found["collections"].as_array().cloned().unwrap_or_default())
    }

    /// The collection with its fields, as {id, displayName, slug, fields: [{slug, displayName, type, isRequired}]}.
    pub async fn collection(&self, id: &str) -> Result<Value, Fault> {
        self.call(self.client.get(format!("{}/collections/{id}", self.api))).await
    }

    fn made(blog: &Blog, slug: &str, draft: bool) -> Made {
        Made { url: format!("{}/{}/{slug}", blog.url, blog.collection_slug), draft }
    }

    async fn find(&self, blog: &Blog, slug: &str) -> Result<Option<Made>, Fault> {
        let items = at(&format!("{}/collections/{}/items", self.api, blog.collection_id), &[("slug", slug)])?;
        let found = self.call(self.client.get(items)).await?;
        let items = found["items"].as_array().cloned().unwrap_or_default();
        let item = items.iter().find(|i| i["fieldData"]["slug"] == slug);
        Ok(item.map(|i| Self::made(blog, slug, i["isDraft"] == true || i["lastPublished"].is_null())))
    }

    /// A published article goes live at once, and a draft waits in the CMS. Tags go in the tags field as a list, when the
    /// collection has one picked.
    async fn create(&self, blog: &Blog, a: &Article<'_>, publish: bool) -> Result<Made, Fault> {
        let mut fields = json!({ "slug": a.slug });
        fields[&blog.title_field] = json!(a.title);
        fields[&blog.body_field] = json!(a.html);
        if !blog.tags_field.is_empty() {
            fields[&blog.tags_field] = json!(a.tags.join(", "));
        }
        if !blog.excerpt_field.is_empty() {
            fields[&blog.excerpt_field] = json!(a.excerpt);
        }
        let path = if publish { "items/live" } else { "items" };
        let item = json!({ "isArchived": false, "isDraft": !publish, "fieldData": fields });
        self.call(self.client.post(format!("{}/collections/{}/{path}", self.api, blog.collection_id)).json(&item)).await?;
        Ok(Self::made(blog, a.slug, !publish))
    }
}

pub enum Client {
    WordPress(WordPress),
    Ghost(Ghost),
    Webflow(Webflow),
}

impl Client {
    /// A client for a blog with what the user typed.
    pub async fn connect(cfg: &Config, kind: &str, url: &str, username: &str, secret: &str) -> Result<Self, Fault> {
        Ok(match kind {
            "wordpress" => Client::WordPress(WordPress::new(url, username, secret).await?),
            "ghost" => Client::Ghost(Ghost::new(url, secret).await?),
            _ => Client::Webflow(Webflow::new(&cfg.webflow_api_url, secret)?),
        })
    }

    /// A client for a saved blog.
    pub async fn of(cfg: &Config, blog: &Blog) -> Result<Self, Fault> {
        let what = match blog.kind.as_str() {
            "wordpress" => "application password",
            "ghost" => "Admin API key",
            _ => "API token",
        };
        let secret = decrypt(&cfg.app_secret, &blog.secret_enc).map_err(|_| Fault::Refused(format!("Couldn't read the saved {what}. Connect the blog again")))?;
        Self::connect(cfg, &blog.kind, &blog.url, &blog.username, &secret).await
    }

    async fn find(&self, blog: &Blog, slug: &str) -> Result<Option<Made>, Fault> {
        match self {
            Client::WordPress(wp) => wp.find(slug).await,
            Client::Ghost(g) => g.find(slug).await,
            Client::Webflow(wf) => wf.find(blog, slug).await,
        }
    }

    async fn create(&self, blog: &Blog, a: &Article<'_>) -> Result<Made, Fault> {
        match self {
            Client::WordPress(wp) => wp.create(a, blog.publish).await,
            Client::Ghost(g) => g.create(a, blog.publish).await,
            Client::Webflow(wf) => wf.create(blog, a, blog.publish).await,
        }
    }
}

/// How many more articles each of the project's blogs that isn't paused takes this week. Drafts count, so a run doesn't draft
/// more than the week's articles, unless they're skipped.
pub async fn room(db: &PgPool, project_id: Uuid) -> sqlx::Result<Vec<(Uuid, i64)>> {
    sqlx::query_as(&format!("select b.id, {ROOM} from blogs b where b.project_id = $1 and not b.paused")).bind(project_id).fetch_all(db).await
}

/// The rest of the week's articles for one blog, b in the query.
const ROOM: &str = "b.weekly_cap - (select count(*) from social_posts sp where sp.blog_id = b.id and sp.created_at > now() - interval '7 days'
                                    and sp.status not in ('skipped', 'failed'))";

/// Whether the blog takes another draft this week, holding its row so two runs saving at once can't both take the last one.
/// Call it in the transaction that saves the draft.
pub async fn take_room(tx: &mut sqlx::PgConnection, blog_id: Uuid, project_id: Uuid) -> sqlx::Result<bool> {
    let locked: Option<Uuid> = sqlx::query_scalar("select id from blogs where id = $1 and project_id = $2 and not paused for update")
        .bind(blog_id)
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?;
    if locked.is_none() {
        return Ok(false);
    }
    let left: i64 = sqlx::query_scalar(&format!("select {ROOM} from blogs b where b.id = $1")).bind(blog_id).fetch_one(&mut *tx).await?;
    Ok(left > 0)
}

pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    // Trying again is safe, since a try starts by looking for the article on the blog.
    sqlx::query("update social_posts set status = 'approved', updated_at = now() where platform = 'blog' and status = 'posting' and updated_at < now() - make_interval(mins => $1)")
        .bind(STUCK_MINUTES)
        .execute(&state.db)
        .await?;
    let waiting: Vec<Blog> = sqlx::query_as(
        "select b.* from blogs b join projects p on p.id = b.project_id
         where p.org_id is not null and exists (select 1 from social_posts sp where sp.blog_id = b.id and sp.status = 'approved')
         order by b.created_at",
    )
    .fetch_all(&state.db)
    .await?;
    // The same blog can be connected to several projects, and it still takes one article a tick.
    let mut used = HashSet::new();
    for blog in waiting {
        if used.contains(&blog.account) {
            continue;
        }
        match publish_next(state, &blog).await {
            Ok(true) => {
                used.insert(blog.account.clone());
            }
            Ok(false) => {}
            Err(e) => tracing::error!(project_id = %blog.project_id, blog = %blog.url, error = ?e, "blog publishing failed"),
        }
    }
    Ok(())
}

/// The blog's longest-approved article, unless it's paused, its account had a failure in the last hour, or the account has
/// its weekly cap of articles out or going out.
async fn next_due(db: &PgPool, blog: &Blog) -> sqlx::Result<Option<SocialPost>> {
    sqlx::query_as(
        "select sp.* from social_posts sp
         where sp.blog_id = $1 and sp.status = 'approved'
           and not exists (select 1 from blogs b where b.id = $1 and b.paused)
           and not exists (select 1 from blogs b where b.account = $2 and b.failed_at > now() - interval '1 hour')
           and (select count(*) from social_posts o where o.platform = 'blog' and o.account = $2
                and (o.status = 'posting' or o.status = 'posted' and o.posted_at > now() - interval '7 days')) < $3
         order by sp.updated_at limit 1",
    )
    .bind(blog.id)
    .bind(&blog.account)
    .bind(blog.weekly_cap as i64)
    .fetch_optional(db)
    .await
}

/// Marks the article as going out and picks its slug on the first try, both before anything is sent. None when the user
/// changed it since, or another worker took the account's last article of the week. The account's lock makes that count
/// and the claim one step, since next_due's count can be stale by then.
async fn claim(db: &PgPool, post: &SocialPost, blog: &Blog) -> sqlx::Result<Option<String>> {
    let mut tx = db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('blog:' || $1, 0))").bind(&blog.account).execute(&mut *tx).await?;
    let slug = sqlx::query_scalar(
        "update social_posts set status = 'posting', account = $2, slug = case when slug = '' then $3 else slug end, updated_at = now()
         where id = $1 and status = 'approved'
           and (select count(*) from social_posts o where o.platform = 'blog' and o.account = $2
                and (o.status = 'posting' or o.status = 'posted' and o.posted_at > now() - interval '7 days')) < $4
         returning slug",
    )
    .bind(post.id)
    .bind(&blog.account)
    .bind(slug(&post.title, post.id))
    .bind(blog.weekly_cap as i64)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(slug)
}

async fn publish(state: &AppState, blog: &Blog, post: &SocialPost, slug: &str) -> Result<Made, Fault> {
    let client = Client::of(&state.cfg, blog).await?;
    if let Some(made) = client.find(blog, slug).await? {
        return Ok(made);
    }
    let html = html(&post.body);
    client.create(blog, &Article { title: &post.title, html: &html, slug, tags: &post.tags, excerpt: &post.excerpt }).await
}

/// Returns whether it claimed an article.
async fn publish_next(state: &AppState, blog: &Blog) -> anyhow::Result<bool> {
    let Some(post) = next_due(&state.db, blog).await? else { return Ok(false) };
    let Some(slug) = claim(&state.db, &post, blog).await? else { return Ok(false) };
    let (published, draft) = match publish(state, blog, &post, &slug).await {
        Ok(made) => (Ok(made.url), made.draft),
        Err(Fault::Article(reason)) => (Err(Failure::Post(reason)), false),
        Err(Fault::Later(reason)) => (Err(Failure::Account(reason)), false),
        Err(Fault::Refused(reason)) => {
            // Matched on the secret, so a reconnect in the meantime isn't paused.
            sqlx::query("update blogs set paused = true where id = $1 and secret_enc = $2").bind(blog.id).bind(&blog.secret_enc).execute(&state.db).await?;
            (Err(Failure::Account(reason)), false)
        }
    };
    let result = social::finish(&state.db, &post, &blog.account, published).await?;
    if draft {
        sqlx::query("update social_posts set saved_as_draft = true where id = $1").bind(post.id).execute(&state.db).await?;
    }
    hooks::record(&state.db, blog.project_id, Alert::Blog, &blog.secret_enc, &result).await;
    Ok(true)
}

/// WordPress, Ghost and Webflow as their docs describe them, on one local server: WordPress under /wp, Ghost's admin under
/// /gh and Webflow's API under /webflow. They keep what they're sent, and a title of "Gateway" gets a 504 after the post is
/// saved, the first time, like a proxy that gave up on a slow blog.
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, Query, State};
    use axum::http::HeaderMap;
    use axum::routing::get;
    use axum::{Json, Router};

    use super::*;

    pub const WP_USER: &str = "ana";
    pub const WP_PASSWORD: &str = "abcd efgh ijkl mnop";
    pub const GHOST_KEY: &str = "6489a1b2c3d4e5f6a7b8c9d0:3f2a9c7d1e4b6a8f0c2e4d6b8a0f1c3e5d7b9a1c3e5f7d9b1a3c5e7f9d1b3a5c";
    pub const WEBFLOW_TOKEN: &str = "wf-good";

    #[derive(Default)]
    pub struct World {
        /// What each blog was sent, as ("wordpress" | "ghost" | "webflow", the post).
        pub posts: Vec<(String, Value)>,
        /// WordPress's tags as (id, name). It starts with "Sales".
        pub tags: Vec<(i64, String)>,
        /// The blogs that already gave up on a "Gateway".
        gave_up: Vec<String>,
    }

    pub type Shared = Arc<Mutex<World>>;
    type Answer = (StatusCode, Json<Value>);

    fn wp_refused(headers: &HeaderMap) -> Option<Answer> {
        let good = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{WP_USER}:{WP_PASSWORD}")));
        let message = json!({ "code": "incorrect_password", "message": "The provided password is an invalid application password.", "data": { "status": 401 } });
        (headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(&good)).then_some((StatusCode::UNAUTHORIZED, Json(message)))
    }

    /// Ghost checks the token's signature with the key's secret, its audience and that it lasts five minutes at most.
    fn ghost_refused(headers: &HeaderMap) -> Option<Answer> {
        let refused = || Some((StatusCode::UNAUTHORIZED, Json(json!({ "errors": [{ "message": "Invalid token", "type": "UnauthorizedError" }] }))));
        let Some(token) = headers.get("authorization").and_then(|v| v.to_str().ok()?.strip_prefix("Ghost ")) else { return refused() };
        let parts: Vec<&str> = token.split('.').collect();
        let (id, secret) = ghost_key(GHOST_KEY).unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(&secret).unwrap();
        mac.update(format!("{}.{}", parts[0], parts.get(1).unwrap_or(&"")).as_bytes());
        let signed = parts.get(2).and_then(|s| URL_SAFE_NO_PAD.decode(s).ok()).is_some_and(|s| mac.verify_slice(&s).is_ok());
        let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap_or_default()).unwrap_or_default();
        let claims: Value = parts.get(1).and_then(|c| serde_json::from_slice(&URL_SAFE_NO_PAD.decode(c).ok()?).ok()).unwrap_or_default();
        let lasts = claims["exp"].as_i64().unwrap_or_default() - claims["iat"].as_i64().unwrap_or_default();
        let fine = signed && header["kid"] == id && claims["aud"] == "/admin/" && (1..=300).contains(&lasts) && headers["accept-version"] == "v5.0";
        if fine { None } else { refused() }
    }

    fn webflow_refused(headers: &HeaderMap) -> Option<Answer> {
        let good = format!("Bearer {WEBFLOW_TOKEN}");
        let message = json!({ "message": "Request not authorized", "code": "not_authorized" });
        (headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(&good)).then_some((StatusCode::UNAUTHORIZED, Json(message)))
    }

    /// Saves the post, and answers the blog's first "Gateway" with a 504 after saving it.
    fn save(world: &Shared, kind: &str, post: Value) -> Option<Answer> {
        let mut w = world.lock().unwrap();
        w.posts.push((kind.into(), post.clone()));
        let title = [&post["title"], &post["fieldData"]["name"]].into_iter().find_map(Value::as_str).unwrap_or_default();
        if title == "Gateway" && !w.gave_up.iter().any(|k| k == kind) {
            w.gave_up.push(kind.into());
            return Some((StatusCode::GATEWAY_TIMEOUT, Json(json!({}))));
        }
        None
    }

    fn ok(v: Value) -> Answer {
        (StatusCode::OK, Json(v))
    }

    async fn wp_index(headers: HeaderMap) -> Answer {
        wp_refused(&headers).unwrap_or_else(|| ok(json!({ "name": "Acme &amp; Co&#039;s blog", "url": "https://acme.test" })))
    }

    async fn wp_me(headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Answer {
        assert_eq!(q["context"], "edit");
        wp_refused(&headers).unwrap_or_else(|| ok(json!({ "id": 1, "name": "Ana Silva", "capabilities": { "publish_posts": true } })))
    }

    async fn wp_tags(State(world): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Answer {
        if let Some(refused) = wp_refused(&headers) {
            return refused;
        }
        let w = world.lock().unwrap();
        let wanted = q["search"].to_lowercase();
        ok(json!(w.tags.iter().filter(|(_, n)| n.to_lowercase().contains(&wanted)).map(|(id, name)| json!({ "id": id, "name": name })).collect::<Vec<_>>()))
    }

    async fn wp_new_tag(State(world): State<Shared>, headers: HeaderMap, Json(body): Json<Value>) -> Answer {
        if let Some(refused) = wp_refused(&headers) {
            return refused;
        }
        let mut w = world.lock().unwrap();
        let id = w.tags.len() as i64 + 1;
        w.tags.push((id, body["name"].as_str().unwrap().into()));
        (StatusCode::CREATED, Json(json!({ "id": id, "name": body["name"] })))
    }

    fn wp_post(id: usize, post: &Value) -> Value {
        json!({ "id": id, "status": post["status"], "link": format!("https://acme.test/{}/", post["slug"].as_str().unwrap_or_default()) })
    }

    async fn wp_posts(State(world): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Answer {
        if let Some(refused) = wp_refused(&headers) {
            return refused;
        }
        assert!(q["status"].contains("draft") && q["context"] == "edit", "drafts are only found with them");
        let w = world.lock().unwrap();
        let found = w.posts.iter().enumerate().filter(|(_, (k, p))| k == "wordpress" && p["slug"] == q["slug"].as_str()).map(|(i, (_, p))| wp_post(i + 1, p));
        ok(json!(found.collect::<Vec<_>>()))
    }

    async fn wp_new_post(State(world): State<Shared>, headers: HeaderMap, Json(body): Json<Value>) -> Answer {
        if let Some(refused) = wp_refused(&headers) {
            return refused;
        }
        if let Some(gave_up) = save(&world, "wordpress", body.clone()) {
            return gave_up;
        }
        (StatusCode::CREATED, Json(wp_post(world.lock().unwrap().posts.len(), &body)))
    }

    async fn ghost_site() -> Answer {
        ok(json!({ "site": { "title": "Acme Notes", "url": "https://notes.acme.test/", "version": "5.96" } }))
    }

    fn ghost_post(id: usize, post: &Value) -> Value {
        let slug = post["slug"].as_str().unwrap_or_default();
        json!({ "id": format!("ghost{id}"), "status": post["status"], "slug": slug, "url": format!("https://notes.acme.test/{slug}/") })
    }

    async fn ghost_posts(State(world): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Answer {
        if let Some(refused) = ghost_refused(&headers) {
            return refused;
        }
        let slug = q.get("filter").and_then(|f| f.strip_prefix("slug:")).unwrap_or_default().to_string();
        let w = world.lock().unwrap();
        let found = w.posts.iter().enumerate().filter(|(_, (k, p))| k == "ghost" && p["slug"] == slug.as_str()).map(|(i, (_, p))| ghost_post(i + 1, p));
        ok(json!({ "posts": found.collect::<Vec<_>>() }))
    }

    async fn ghost_new_post(State(world): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>, Json(body): Json<Value>) -> Answer {
        if let Some(refused) = ghost_refused(&headers) {
            return refused;
        }
        assert_eq!(q["source"], "html", "Ghost only keeps HTML it's told is HTML");
        let post = body["posts"][0].clone();
        if let Some(gave_up) = save(&world, "ghost", post.clone()) {
            return gave_up;
        }
        (StatusCode::CREATED, Json(json!({ "posts": [ghost_post(world.lock().unwrap().posts.len(), &post)] })))
    }

    async fn webflow_sites(headers: HeaderMap) -> Answer {
        let site = json!({ "id": "site1", "displayName": "Acme", "shortName": "acme", "customDomains": [{ "id": "d1", "url": "www.acme.test" }] });
        webflow_refused(&headers).unwrap_or_else(|| ok(json!({ "sites": [site] })))
    }

    async fn webflow_collections(headers: HeaderMap, Path(site): Path<String>) -> Answer {
        assert_eq!(site, "site1");
        let collections = json!([{ "id": "col1", "displayName": "Blog Posts", "slug": "blog" }, { "id": "col2", "displayName": "Team", "slug": "team" }]);
        webflow_refused(&headers).unwrap_or_else(|| ok(json!({ "collections": collections })))
    }

    pub fn webflow_fields() -> Value {
        json!([
            { "slug": "name", "displayName": "Name", "type": "PlainText", "isRequired": true },
            { "slug": "slug", "displayName": "Slug", "type": "PlainText", "isRequired": true },
            { "slug": "post-body", "displayName": "Post Body", "type": "RichText", "isRequired": false },
            { "slug": "post-summary", "displayName": "Post Summary", "type": "PlainText", "isRequired": false },
            { "slug": "tags", "displayName": "Tags", "type": "PlainText", "isRequired": false },
            { "slug": "main-image", "displayName": "Main Image", "type": "Image", "isRequired": false },
        ])
    }

    async fn webflow_collection(headers: HeaderMap, Path(id): Path<String>) -> Answer {
        // Team has a required field Splee can't fill.
        let team = json!([{ "slug": "name", "displayName": "Name", "type": "PlainText", "isRequired": true },
                          { "slug": "role", "displayName": "Role", "type": "PlainText", "isRequired": true }]);
        let (name, slug, fields) = if id == "col2" { ("Team", "team", team) } else { ("Blog Posts", "blog", webflow_fields()) };
        webflow_refused(&headers).unwrap_or_else(|| ok(json!({ "id": id, "displayName": name, "slug": slug, "fields": fields })))
    }

    fn webflow_item(id: usize, item: &Value, live: bool) -> Value {
        let published = if live { json!("2026-11-12T10:00:00Z") } else { Value::Null };
        json!({ "id": format!("item{id}"), "isDraft": item["isDraft"], "lastPublished": published, "fieldData": item["fieldData"] })
    }

    async fn webflow_items(State(world): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Answer {
        if let Some(refused) = webflow_refused(&headers) {
            return refused;
        }
        let w = world.lock().unwrap();
        let found = w.posts.iter().enumerate().filter(|(_, (k, p))| k.starts_with("webflow") && p["fieldData"]["slug"] == q["slug"].as_str());
        ok(json!({ "items": found.map(|(i, (k, p))| webflow_item(i + 1, p, k == "webflow live")).collect::<Vec<_>>() }))
    }

    async fn webflow_new_item(world: Shared, headers: HeaderMap, body: Value, live: bool) -> Answer {
        if let Some(refused) = webflow_refused(&headers) {
            return refused;
        }
        if let Some(gave_up) = save(&world, if live { "webflow live" } else { "webflow" }, body.clone()) {
            return gave_up;
        }
        (StatusCode::ACCEPTED, Json(webflow_item(world.lock().unwrap().posts.len(), &body, live)))
    }

    /// The app's state, with Webflow's API at the fake.
    pub async fn state(db: &PgPool, base: &str) -> AppState {
        let provider = crate::mail::oauth::tests::FakeProvider::start().await;
        let cfg = Config { webflow_api_url: format!("{base}/webflow"), ..crate::mail::oauth::tests::config(&provider) };
        AppState { db: db.clone(), cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(1).0 }
    }

    /// The fake's address and what it was sent.
    pub async fn start() -> (String, Shared) {
        let world = Shared::default();
        world.lock().unwrap().tags.push((1, "Sales".into()));
        let app = Router::new()
            .route("/wp/wp-json/", get(wp_index))
            .route("/wp/wp-json/wp/v2/users/me", get(wp_me))
            .route("/wp/wp-json/wp/v2/tags", get(wp_tags).post(wp_new_tag))
            .route("/wp/wp-json/wp/v2/posts", get(wp_posts).post(wp_new_post))
            .route("/gh/ghost/api/admin/site/", get(ghost_site))
            .route("/gh/ghost/api/admin/posts/", get(ghost_posts).post(ghost_new_post))
            .route("/webflow/sites", get(webflow_sites))
            .route("/webflow/sites/{site}/collections", get(webflow_collections))
            .route("/webflow/collections/{id}", get(webflow_collection))
            .route(
                "/webflow/collections/{id}/items",
                get(webflow_items).post(|State(w): State<Shared>, h: HeaderMap, Json(b): Json<Value>| webflow_new_item(w, h, b, false)),
            )
            .route("/webflow/collections/{id}/items/live", axum::routing::post(|State(w): State<Shared>, h: HeaderMap, Json(b): Json<Value>| webflow_new_item(w, h, b, true)))
            .with_state(world.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // hooks::public_client lets this port through as if it were public.
        hooks::TEST_RECEIVERS.lock().unwrap().push(port);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://127.0.0.1:{port}"), world)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::encrypt;

    #[test]
    fn markdown_becomes_html_without_markup_or_scripts_from_the_text() {
        let out = html("## Day one\n\nSend **20** a day, see [the guide](https://acme.test/guide).\n\n```\nwarmup --days 28\n```\n\n| Week | Sends |\n|---|---|\n| 1 | 20 |");
        assert!(out.contains("<h2>Day one</h2>") && out.contains("<strong>20</strong>") && out.contains(r#"<a href="https://acme.test/guide">the guide</a>"#), "{out}");
        assert!(out.contains("<pre><code>warmup --days 28\n</code></pre>") && out.contains("<table>"), "{out}");
        let out = html("Hi <script>alert(1)</script> and [x](javascript:alert(1)) and [y](/pricing) and [z](mailto:ana@acme.test)");
        assert!(!out.contains("<script>") && out.contains("&lt;script&gt;"), "{out}");
        assert!(out.contains(r#"<a href="">x</a>"#) && out.contains(r#"<a href="/pricing">y</a>"#) && out.contains(r#"href="mailto:ana@acme.test""#), "{out}");
    }

    #[test]
    fn addresses_slugs_and_tags_get_one_form() {
        assert_eq!(site_url("acme.com/blog/wp-admin/post-new.php?x=1").as_deref(), Some("https://acme.com/blog"));
        assert_eq!(site_url("https://notes.acme.com/ghost/#/dashboard").as_deref(), Some("https://notes.acme.com"));
        assert_eq!(site_url("http://acme.com"), None);
        assert_eq!(site_url("localhost"), None);
        assert_eq!(account("https://Acme.com/Blog"), "acme.com/blog");
        let id = Uuid::parse_str("3f2a9c7d-1e4b-4a8f-8c2e-4d6b8a0f1c3e").unwrap();
        assert_eq!(slug("How we warm up a new domain: 28 days, 4 steps!", id), "how-we-warm-up-a-new-domain-28-days-4-steps-3f2a9c7d");
        assert_eq!(slug("Как мы греем домены", id), "post-3f2a9c7d");
        assert_eq!(slug(&"word ".repeat(30), id).len(), 59 + 9);
        let raw = ["#Cold Email", "cold  email", "Sales", "", "b2b", "growth", "devops", "extra"].map(String::from);
        assert_eq!(tags(&raw), ["Cold Email", "Sales", "b2b", "growth", "devops"]);
        assert_eq!(ghost_key("abc:zz"), None);
        assert_eq!(ghost_key("6489a1:0a0b").unwrap(), ("6489a1".to_string(), vec![10, 11]));
    }

    #[test]
    fn the_ghost_token_is_a_jwt_signed_with_the_secret_for_five_minutes() {
        let (id, secret) = ghost_key(fake::GHOST_KEY).unwrap();
        let token = ghost_token(&id, &secret, 1_700_000_000);
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let decode = |p: &str| serde_json::from_slice::<Value>(&URL_SAFE_NO_PAD.decode(p).unwrap()).unwrap();
        assert_eq!(decode(parts[0]), json!({ "alg": "HS256", "typ": "JWT", "kid": "6489a1b2c3d4e5f6a7b8c9d0" }));
        assert_eq!(decode(parts[1]), json!({ "iat": 1_700_000_000, "exp": 1_700_000_300, "aud": "/admin/" }));
        let mut mac = Hmac::<Sha256>::new_from_slice(&secret).unwrap();
        mac.update(format!("{}.{}", parts[0], parts[1]).as_bytes());
        mac.verify_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
    }

    #[tokio::test]
    async fn each_blog_checks_its_credentials_and_says_whose_problem_a_failure_is() {
        let (base, _) = fake::start().await;
        let wp = WordPress::new(&format!("{base}/wp"), fake::WP_USER, fake::WP_PASSWORD).await.ok().unwrap();
        let site = wp.check().await.ok().unwrap();
        assert_eq!((site.name.as_str(), site.user.as_str()), ("Acme & Co's blog", "Ana Silva"));
        let wrong = WordPress::new(&format!("{base}/wp"), fake::WP_USER, "nope").await.ok().unwrap();
        assert!(matches!(wrong.check().await, Err(Fault::Refused(m)) if m.contains("application password") && m.contains("connect the blog again")));

        let ghost = Ghost::new(&format!("{base}/gh"), fake::GHOST_KEY).await.ok().unwrap();
        assert_eq!(ghost.check().await.ok().unwrap().name, "Acme Notes");
        let other = format!("{}:{}", "6489a1b2c3d4e5f6a7b8c9d0", "ab".repeat(32));
        assert!(matches!(Ghost::new(&format!("{base}/gh"), &other).await.ok().unwrap().check().await, Err(Fault::Refused(m)) if m.contains("Admin API key")));
        assert!(matches!(Ghost::new(&format!("{base}/gh"), "no-colon").await, Err(Fault::Refused(_))));

        let webflow = Webflow::new(&format!("{base}/webflow"), fake::WEBFLOW_TOKEN).unwrap();
        let sites = webflow.sites().await.ok().unwrap();
        assert_eq!((sites[0].id.as_str(), sites[0].url.as_str()), ("site1", "https://www.acme.test"));
        assert_eq!(webflow.collections("site1").await.ok().unwrap()[0]["slug"], "blog");
        assert!(matches!(Webflow::new(&format!("{base}/webflow"), "old").unwrap().sites().await, Err(Fault::Refused(m)) if m.contains("API token")));

        // Sites on this machine, or anywhere but the public internet, are turned away.
        assert!(matches!(WordPress::new("http://127.0.0.1:9/wp", "ana", "x").await, Err(Fault::Later(m)) if m.contains("not a public address")));
    }

    async fn project(db: &PgPool) -> Uuid {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(db).await.unwrap();
        sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(db).await.unwrap()
    }

    async fn add_blog(state: &AppState, project: Uuid, kind: &str, url: &str, secret: &str, publish: bool) -> Blog {
        let account = if kind == "webflow" { "webflow/col1".to_string() } else { account(url) };
        sqlx::query_as(
            "insert into blogs (project_id, kind, url, name, username, secret_enc, account, site_id, collection_id, collection_name, collection_slug,
               title_field, body_field, tags_field, excerpt_field, publish)
             values ($1, $2, $3, 'Acme', $4, $5, $6, 'site1', 'col1', 'Blog Posts', 'blog', 'name', 'post-body', 'tags', 'post-summary', $7) returning *",
        )
        .bind(project)
        .bind(kind)
        .bind(url)
        .bind(if kind == "wordpress" { fake::WP_USER } else { "" })
        .bind(encrypt(&state.cfg.app_secret, secret))
        .bind(account)
        .bind(publish)
        .fetch_one(&state.db)
        .await
        .unwrap()
    }

    async fn article(db: &PgPool, project: Uuid, blog: Uuid, title: &str, status: &str) -> Uuid {
        sqlx::query_scalar(
            "insert into social_posts (project_id, platform, blog_id, title, body, tags, excerpt, status)
             values ($1, 'blog', $2, $3, '## Day one\n\nSend **20** a day.', $4, 'Four weeks, step by step.', $5) returning id",
        )
        .bind(project)
        .bind(blog)
        .bind(title)
        .bind(vec!["sales".to_string(), "Cold email".to_string()])
        .bind(status)
        .fetch_one(db)
        .await
        .unwrap()
    }

    async fn post(db: &PgPool, id: Uuid) -> SocialPost {
        sqlx::query_as("select * from social_posts where id = $1").bind(id).fetch_one(db).await.unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn approved_articles_go_in_as_drafts_or_published_with_their_tags(db: PgPool) {
        let (base, world) = fake::start().await;
        let state = fake::state(&db, &base).await;
        let project = project(&db).await;
        let wp = add_blog(&state, project, "wordpress", &format!("{base}/wp"), fake::WP_PASSWORD, false).await;
        let ghost = add_blog(&state, project, "ghost", &format!("{base}/gh"), fake::GHOST_KEY, true).await;
        let webflow = add_blog(&state, project, "webflow", "https://www.acme.test", fake::WEBFLOW_TOKEN, true).await;
        // A draft waits for the user's approval.
        let draft = article(&db, project, wp.id, "Not yet", "draft").await;
        let to_wp = article(&db, project, wp.id, "How we warm up", "approved").await;
        let to_ghost = article(&db, project, ghost.id, "How we warm up", "approved").await;
        let to_webflow = article(&db, project, webflow.id, "How we warm up", "approved").await;
        tick(&state).await.unwrap();

        let sent = world.lock().unwrap().posts.clone();
        assert_eq!(sent.len(), 3);
        let html = "<h2>Day one</h2>\n<p>Send <strong>20</strong> a day.</p>\n";
        let wp_slug = slug("How we warm up", to_wp);
        // "sales" was already a tag on the site, and "Cold email" is made.
        assert_eq!(sent[0], ("wordpress".to_string(), json!({ "title": "How we warm up", "content": html, "excerpt": "Four weeks, step by step.", "slug": wp_slug,
                                                              "status": "draft", "tags": [1, 2] })));
        assert_eq!(world.lock().unwrap().tags[1], (2, "Cold email".to_string()));
        let wp_post = post(&db, to_wp).await;
        assert_eq!((wp_post.status.as_str(), wp_post.saved_as_draft), ("posted", true));
        assert_eq!(wp_post.url, format!("{base}/wp/wp-admin/post.php?post=1&action=edit"));

        assert_eq!(sent[1].1["status"], "published");
        assert_eq!(sent[1].1["tags"], json!([{ "name": "sales" }, { "name": "Cold email" }]));
        assert_eq!((sent[1].1["html"].as_str(), sent[1].1["custom_excerpt"].as_str()), (Some(html), Some("Four weeks, step by step.")));
        let ghost_post = post(&db, to_ghost).await;
        assert_eq!((ghost_post.url, ghost_post.saved_as_draft), (format!("https://notes.acme.test/{}/", slug("How we warm up", to_ghost)), false));

        let webflow_slug = slug("How we warm up", to_webflow);
        assert_eq!(sent[2], ("webflow live".to_string(), json!({ "isArchived": false, "isDraft": false, "fieldData": { "name": "How we warm up", "slug": webflow_slug,
            "post-body": html, "tags": "sales, Cold email", "post-summary": "Four weeks, step by step." } })));
        assert_eq!(post(&db, to_webflow).await.url, format!("https://www.acme.test/blog/{webflow_slug}"));
        assert_eq!(post(&db, draft).await.status, "draft");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_try_that_may_have_gone_through_is_found_by_its_slug_instead_of_posted_twice(db: PgPool) {
        let (base, world) = fake::start().await;
        let state = fake::state(&db, &base).await;
        let project = project(&db).await;
        for (kind, url, secret) in [("wordpress", format!("{base}/wp"), fake::WP_PASSWORD), ("ghost", format!("{base}/gh"), fake::GHOST_KEY),
                                    ("webflow", "https://www.acme.test".to_string(), fake::WEBFLOW_TOKEN)] {
            world.lock().unwrap().posts.clear();
            sqlx::query("delete from blogs").execute(&db).await.unwrap();
            let blog = add_blog(&state, project, kind, &url, secret, true).await;
            let id = article(&db, project, blog.id, "Gateway", "approved").await;
            // The blog saved it, then the answer was a 504, so it waits and the blog holds for an hour.
            tick(&state).await.unwrap();
            let waiting: (String, String) = sqlx::query_as("select status, slug from social_posts where id = $1").bind(id).fetch_one(&db).await.unwrap();
            assert_eq!(waiting, ("approved".to_string(), slug("Gateway", id)), "{kind}");
            let failed: Option<String> = sqlx::query_scalar("select error from blogs where id = $1").bind(blog.id).fetch_one(&db).await.unwrap();
            assert!(failed.is_some_and(|e| e.contains("504")), "{kind}");
            tick(&state).await.unwrap();
            assert_eq!(post(&db, id).await.status, "approved", "{kind}: held for an hour");

            sqlx::query("update blogs set failed_at = now() - interval '2 hours'").execute(&db).await.unwrap();
            tick(&state).await.unwrap();
            let posted = post(&db, id).await;
            assert_eq!(posted.status, "posted", "{kind}");
            assert!(posted.url.contains(&slug("Gateway", id)), "{kind}: {}", posted.url);
            assert_eq!(world.lock().unwrap().posts.len(), 1, "{kind} got it once");
            let cleared: Option<String> = sqlx::query_scalar("select error from blogs where id = $1").bind(blog.id).fetch_one(&db).await.unwrap();
            assert_eq!(cleared, None, "{kind}");
        }

        // One cut off mid-send by a restart goes back in the queue, and is found too.
        let blog: Blog = sqlx::query_as("select * from blogs").fetch_one(&db).await.unwrap();
        let id = article(&db, project, blog.id, "Gateway", "approved").await;
        sqlx::query("update social_posts set status = 'posting', slug = 'gateway-left', updated_at = now() - interval '1 hour' where id = $1").bind(id).execute(&db).await.unwrap();
        world.lock().unwrap().posts = vec![("webflow live".into(), json!({ "isDraft": false, "fieldData": { "name": "Gateway", "slug": "gateway-left" } }))];
        sqlx::query("update social_posts set posted_at = now() - interval '8 days' where status = 'posted'").execute(&db).await.unwrap();
        tick(&state).await.unwrap();
        assert_eq!(post(&db, id).await.url, "https://www.acme.test/blog/gateway-left");
        assert_eq!(world.lock().unwrap().posts.len(), 1);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn turned_down_credentials_pause_the_blog_and_the_weekly_cap_holds_the_rest(db: PgPool) {
        let (base, world) = fake::start().await;
        let state = fake::state(&db, &base).await;
        let project = project(&db).await;
        let wrong = format!("{}:{}", "6489a1b2c3d4e5f6a7b8c9d0", "ab".repeat(32));
        let ghost = add_blog(&state, project, "ghost", &format!("{base}/gh"), &wrong, true).await;
        let id = article(&db, project, ghost.id, "How we warm up", "approved").await;
        tick(&state).await.unwrap();
        let paused: Blog = sqlx::query_as("select * from blogs where id = $1").bind(ghost.id).fetch_one(&db).await.unwrap();
        assert!(paused.paused);
        assert!(paused.error.is_some_and(|e| e.contains("connect the blog again")));
        assert_eq!(post(&db, id).await.status, "approved");
        // Paused, it stays that way past the hour a failure holds a blog.
        sqlx::query("update blogs set failed_at = now() - interval '2 hours'").execute(&db).await.unwrap();
        tick(&state).await.unwrap();
        assert_eq!(post(&db, id).await.status, "approved");
        assert!(world.lock().unwrap().posts.is_empty());
        assert!(room(&db, project).await.unwrap().is_empty(), "a paused blog gets no drafts");

        // 2 a week by default, counted by the blog's account in any project.
        let wp = add_blog(&state, project, "wordpress", &format!("{base}/wp"), fake::WP_PASSWORD, true).await;
        let next = article(&db, project, wp.id, "How we warm up", "approved").await;
        let other = self::project(&db).await;
        let posted = "insert into social_posts (project_id, platform, title, body, status, account, posted_at) values ($1, 'blog', 'Hi', 'Hi', 'posted', $2, now() - $3::interval)";
        sqlx::query(posted).bind(other).bind(&wp.account).bind("6 days").execute(&db).await.unwrap();
        sqlx::query(posted).bind(other).bind(&wp.account).bind("8 days").execute(&db).await.unwrap();
        assert_eq!(next_due(&db, &wp).await.unwrap().map(|p| p.id), Some(next));
        sqlx::query(posted).bind(other).bind(&wp.account).bind("1 day").execute(&db).await.unwrap();
        assert!(next_due(&db, &wp).await.unwrap().is_none());
        sqlx::query("update blogs set weekly_cap = 3 where id = $1").bind(wp.id).execute(&db).await.unwrap();
        let wp: Blog = sqlx::query_as("select * from blogs where id = $1").bind(wp.id).fetch_one(&db).await.unwrap();
        assert_eq!(next_due(&db, &wp).await.unwrap().map(|p| p.id), Some(next));
        // Drafts count against the week's room too.
        assert_eq!(room(&db, project).await.unwrap(), [(wp.id, 2)]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_last_article_of_the_week_goes_to_one_worker_and_one_run(db: PgPool) {
        let (base, _) = fake::start().await;
        let state = fake::state(&db, &base).await;
        let project = project(&db).await;
        let wp = add_blog(&state, project, "wordpress", &format!("{base}/wp"), fake::WP_PASSWORD, true).await;
        sqlx::query("insert into social_posts (project_id, platform, title, body, status, account, posted_at) values ($1, 'blog', 'Hi', 'Hi', 'posted', $2, now())")
            .bind(project)
            .bind(&wp.account)
            .execute(&db)
            .await
            .unwrap();
        // Two workers each found a different article while one slot was left.
        let (a, b) = (article(&db, project, wp.id, "One", "approved").await, article(&db, project, wp.id, "Two", "approved").await);
        let (a, b) = (post(&db, a).await, post(&db, b).await);
        let (x, y) = tokio::join!(claim(&db, &a, &wp), claim(&db, &b, &wp));
        assert_eq!([x.unwrap(), y.unwrap()].iter().flatten().count(), 1);

        // Two runs saving a draft for a blog with one slot left: the second waits for the first, then finds none.
        let ghost = add_blog(&state, project, "ghost", &format!("{base}/gh"), fake::GHOST_KEY, true).await;
        article(&db, project, ghost.id, "Drafted", "draft").await;
        let mut first = db.begin().await.unwrap();
        assert!(take_room(&mut first, ghost.id, project).await.unwrap());
        let second = tokio::spawn({
            let db = db.clone();
            async move {
                let mut tx = db.begin().await.unwrap();
                take_room(&mut tx, ghost.id, project).await.unwrap()
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        sqlx::query("insert into social_posts (project_id, platform, blog_id, title, body) values ($1, 'blog', $2, 'Mine', 'Hi')")
            .bind(project)
            .bind(ghost.id)
            .execute(&mut *first)
            .await
            .unwrap();
        first.commit().await.unwrap();
        assert!(!second.await.unwrap());
    }
}
