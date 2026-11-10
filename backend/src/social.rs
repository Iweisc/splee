//! Posts approved social drafts from each project's connected accounts: one per account each tick, at most the platform's
//! daily cap, so a day's posts spread out instead of going in a burst. agent::social finds threads and drafts.

use std::collections::HashSet;

use sqlx::PgPool;
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

use crate::agent::x::{self, Cookies};
use crate::bluesky::{self, Bluesky};
use crate::crypto::decrypt;
use crate::hooks::{self, Alert};
use crate::mastodon::Mastodon;
use crate::models::SocialPost;
use crate::reddit::{Login, Reddit, Target};
use crate::state::AppState;
use crate::threads::{self, Threads};
use crate::unipile::{self, Unipile};
use crate::youtube::{self, YouTube};

pub const PLATFORMS: [&str; 7] = ["x", "reddit", "bluesky", "mastodon", "linkedin", "threads", "youtube"];
/// A post still `posting` after this long was cut off mid-send, by a crash or a restart.
const STUCK_MINUTES: i32 = 15;

/// Posts per account a day. New accounts that post more get flagged as spam, and LinkedIn restricts automation the most.
pub fn daily_cap(platform: &str) -> i64 {
    if platform == "linkedin" { 5 } else { 15 }
}

pub fn name(platform: &str) -> &'static str {
    match platform {
        "x" => "X",
        "reddit" => "Reddit",
        "bluesky" => "Bluesky",
        "mastodon" => "Mastodon",
        "threads" => "Threads",
        "youtube" => "YouTube",
        _ => "LinkedIn",
    }
}

/// YouTube only takes comments, so it gets no original posts.
pub fn takes_posts(platform: &str) -> bool {
    platform != "youtube"
}

/// The most a reply or post can hold, as `length` counts it. Mastodon's limit is the instance's, read on connect.
pub fn max_length(platform: &str, reply: bool, mastodon_chars: usize) -> usize {
    match (platform, reply) {
        ("x", _) => 280,
        ("bluesky", _) => bluesky::GRAPHEMES,
        ("mastodon", _) => mastodon_chars,
        ("threads", _) => threads::CHARS,
        ("youtube", _) => youtube::CHARS,
        ("linkedin", true) => 1250,
        ("linkedin", false) => 3000,
        (_, true) => 10_000,
        _ => 40_000,
    }
}

/// Bluesky counts graphemes, the rest count characters.
pub fn length(platform: &str, text: &str) -> usize {
    if platform == "bluesky" { text.graphemes(true).count() } else { text.chars().count() }
}

pub fn fits(platform: &str, reply: bool, text: &str, mastodon_chars: usize) -> bool {
    !text.is_empty() && length(platform, text) <= max_length(platform, reply, mastodon_chars)
}

#[derive(Debug)]
pub enum Failure {
    /// Something about this post, like a deleted thread or a ban from the subreddit. The post fails with the reason.
    Post(String),
    /// The account, like expired cookies or a rate limit. Its posting stops for an hour and the post waits.
    Account(String),
}

impl Failure {
    pub fn message(self) -> String {
        match self {
            Failure::Post(m) | Failure::Account(m) => m,
        }
    }
}

/// A project's social sign-ins as stored, encrypted. LinkedIn is the project's Unipile connection.
#[derive(sqlx::FromRow)]
pub struct Accounts {
    pub project_id: Uuid,
    pub x_auth_token_enc: String,
    pub x_ct0_enc: String,
    pub x_handle: String,
    pub reddit_client_id: String,
    pub reddit_secret_enc: String,
    pub reddit_username: String,
    pub reddit_password_enc: String,
    pub bluesky_handle: String,
    pub bluesky_password_enc: String,
    pub mastodon_url: String,
    pub mastodon_token_enc: String,
    pub mastodon_handle: String,
    pub mastodon_max_chars: i32,
    pub threads_token_enc: String,
    pub threads_user_id: String,
    pub threads_username: String,
    pub youtube_client_id: String,
    pub youtube_secret_enc: String,
    pub youtube_token_enc: String,
    pub youtube_channel_id: String,
    pub unipile_dsn: String,
    pub unipile_token_enc: String,
    pub unipile_account_id: String,
}

const ACCOUNTS: &str = "select s.project_id, s.x_auth_token_enc, s.x_ct0_enc, s.x_handle, s.reddit_client_id, s.reddit_secret_enc, s.reddit_username,
                               s.reddit_password_enc, s.bluesky_handle, s.bluesky_password_enc, s.mastodon_url, s.mastodon_token_enc,
                               s.mastodon_handle, s.mastodon_max_chars, s.threads_token_enc, s.threads_user_id, s.threads_username,
                               s.youtube_client_id, s.youtube_secret_enc, s.youtube_token_enc, s.youtube_channel_id, s.unipile_dsn,
                               s.unipile_token_enc, s.unipile_account_id
                        from project_settings s";

pub async fn accounts(db: &PgPool, project_id: Uuid) -> sqlx::Result<Option<Accounts>> {
    sqlx::query_as(&format!("{ACCOUNTS} where s.project_id = $1")).bind(project_id).fetch_optional(db).await
}

/// The project_settings columns for a platform's account: its name, which the daily cap counts by in any project, and when it
/// last failed. Fixed strings, never user input.
fn columns(platform: &str) -> (&'static str, &'static str) {
    match platform {
        "x" => ("x_handle", "x_failed_at"),
        "reddit" => ("reddit_username", "reddit_failed_at"),
        "bluesky" => ("bluesky_handle", "bluesky_failed_at"),
        "mastodon" => ("mastodon_handle", "mastodon_failed_at"),
        "threads" => ("threads_username", "threads_failed_at"),
        "youtube" => ("youtube_channel_id", "youtube_failed_at"),
        _ => ("unipile_account_id", "unipile_failed_at"),
    }
}

fn alert(platform: &str) -> Alert {
    match platform {
        "x" => Alert::X,
        "reddit" => Alert::Reddit,
        "bluesky" => Alert::Bluesky,
        "mastodon" => Alert::Mastodon,
        "threads" => Alert::Threads,
        "youtube" => Alert::YouTube,
        _ => Alert::Unipile,
    }
}

impl Accounts {
    /// The account's name and its encrypted secret, which hooks::record matches on. The secret is empty when it isn't connected.
    pub fn account(&self, platform: &str) -> (&str, &str) {
        match platform {
            "x" => (&self.x_handle, &self.x_auth_token_enc),
            "reddit" => (&self.reddit_username, &self.reddit_password_enc),
            "bluesky" => (&self.bluesky_handle, &self.bluesky_password_enc),
            "mastodon" => (&self.mastodon_handle, &self.mastodon_token_enc),
            "threads" => (&self.threads_username, &self.threads_token_enc),
            "youtube" => (&self.youtube_channel_id, &self.youtube_token_enc),
            _ => (&self.unipile_account_id, &self.unipile_token_enc),
        }
    }

    pub fn connected(&self) -> Vec<&'static str> {
        PLATFORMS.into_iter().filter(|p| !self.account(p).1.is_empty()).collect()
    }

    pub fn cookies(&self, secret: &str) -> anyhow::Result<Option<Cookies>> {
        if self.x_auth_token_enc.is_empty() {
            return Ok(None);
        }
        Ok(Some(Cookies { auth_token: decrypt(secret, &self.x_auth_token_enc)?, ct0: decrypt(secret, &self.x_ct0_enc)? }))
    }

    pub fn login(&self, secret: &str) -> anyhow::Result<Option<Login>> {
        if self.reddit_password_enc.is_empty() {
            return Ok(None);
        }
        Ok(Some(Login {
            client_id: self.reddit_client_id.clone(),
            secret: decrypt(secret, &self.reddit_secret_enc)?,
            username: self.reddit_username.clone(),
            password: decrypt(secret, &self.reddit_password_enc)?,
        }))
    }

    pub fn bluesky(&self, secret: &str) -> anyhow::Result<Option<bluesky::Login>> {
        if self.bluesky_password_enc.is_empty() {
            return Ok(None);
        }
        Ok(Some(bluesky::Login { handle: self.bluesky_handle.clone(), password: decrypt(secret, &self.bluesky_password_enc)? }))
    }

    pub async fn mastodon(&self, secret: &str) -> Result<Mastodon, Failure> {
        let token = decrypt(secret, &self.mastodon_token_enc).map_err(|_| Failure::Account("Couldn't read the saved Mastodon token. Connect Mastodon again".into()))?;
        Mastodon::new(&self.mastodon_url, &token).await
    }

    pub fn threads(&self, secret: &str) -> Result<Threads, Failure> {
        let token = decrypt(secret, &self.threads_token_enc).map_err(|_| Failure::Account("Couldn't read the saved Threads token. Connect Threads again".into()))?;
        Threads::new(&token, &self.threads_user_id)
    }

    pub fn youtube(&self, secret: &str) -> anyhow::Result<Option<youtube::Login>> {
        if self.youtube_token_enc.is_empty() {
            return Ok(None);
        }
        Ok(Some(youtube::Login {
            client_id: self.youtube_client_id.clone(),
            secret: decrypt(secret, &self.youtube_secret_enc)?,
            refresh_token: decrypt(secret, &self.youtube_token_enc)?,
        }))
    }

    pub fn unipile(&self, secret: &str) -> Result<Unipile, Failure> {
        let token = decrypt(secret, &self.unipile_token_enc).map_err(|_| Failure::Account("Couldn't read the saved Unipile token. Connect Unipile again".into()))?;
        Unipile::new(&self.unipile_dsn, &token, &self.unipile_account_id).map_err(|e| Failure::Account(e.to_string()))
    }

    pub fn mastodon_chars(&self) -> usize {
        self.mastodon_max_chars.max(1) as usize
    }
}

pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    // It may have gone out, so it fails for the user to check rather than going back in the queue.
    sqlx::query(
        "update social_posts set status = 'failed', updated_at = now(),
           error = 'Splee stopped while posting this, so it may have gone out. Check the account before you approve it again.'
         where status = 'posting' and updated_at < now() - make_interval(mins => $1)",
    )
    .bind(STUCK_MINUTES)
    .execute(&state.db)
    .await?;
    let waiting = sqlx::query_as::<_, Accounts>(&format!(
        "{ACCOUNTS} join projects p on p.id = s.project_id
         where p.org_id is not null and exists (select 1 from social_posts sp where sp.project_id = s.project_id and sp.status = 'approved')"
    ))
    .fetch_all(&state.db)
    .await?;
    // The same account can be connected to several projects, and it still posts once a tick.
    let mut used = HashSet::new();
    for a in waiting {
        for platform in a.connected() {
            if let Err(e) = publish_next(state, &a, platform, &mut used).await {
                tracing::error!(project_id = %a.project_id, platform, error = ?e, "social posting failed");
            }
        }
    }
    Ok(())
}

/// `used` holds the accounts that already posted this tick. An account only counts once it claims a post, so a project with
/// nothing due doesn't hold up another project on the same account.
async fn publish_next(state: &AppState, a: &Accounts, platform: &'static str, used: &mut HashSet<(&'static str, String)>) -> anyhow::Result<()> {
    let (account, enc) = a.account(platform);
    let key = (platform, account.to_lowercase());
    if used.contains(&key) {
        return Ok(());
    }
    let Some(post) = next_due(&state.db, a.project_id, platform, account).await? else { return Ok(()) };
    if !claim(&state.db, &post, account).await? {
        return Ok(());
    }
    used.insert(key);
    let published = match platform {
        "x" => post_x(state, a, &post).await,
        "reddit" => post_reddit(state, a, &post).await,
        "bluesky" => post_bluesky(state, a, &post).await,
        "mastodon" => post_mastodon(state, a, &post).await,
        "threads" => post_threads(state, a, &post).await,
        "youtube" => post_youtube(state, a, &post).await,
        _ => post_linkedin(state, a, &post).await,
    };
    let result = finish(&state.db, &post, account, published).await?;
    hooks::record(&state.db, a.project_id, alert(platform), enc, &result).await;
    Ok(())
}

/// The project's longest-approved post on the platform, unless the account (in any project) is at the platform's daily cap
/// or had a failure in the last hour.
async fn next_due(db: &PgPool, project_id: Uuid, platform: &str, account: &str) -> sqlx::Result<Option<SocialPost>> {
    let (name, failed_at) = columns(platform);
    sqlx::query_as(&format!(
        "select sp.* from social_posts sp
         where sp.project_id = $1 and sp.platform = $2 and sp.status = 'approved'
           and (select count(*) from social_posts o where o.platform = $2 and o.account = $3
                and (o.status = 'posting' or o.status = 'posted' and o.posted_at > now() - interval '1 day')) < $4
           and not exists (select 1 from project_settings s where s.{name} = $3 and s.{failed_at} > now() - interval '1 hour')
         order by sp.updated_at limit 1"
    ))
    .bind(project_id)
    .bind(platform)
    .bind(account)
    .bind(daily_cap(platform))
    .fetch_optional(db)
    .await
}

/// Marks the post as being sent, unless the user changed it since. Saved before sending, so a crash can't send it twice.
async fn claim(db: &PgPool, post: &SocialPost, account: &str) -> sqlx::Result<bool> {
    let claimed = sqlx::query("update social_posts set status = 'posting', account = $2, updated_at = now() where id = $1 and status = 'approved'")
        .bind(post.id)
        .bind(account)
        .execute(db)
        .await?;
    Ok(claimed.rows_affected() == 1)
}

/// Saves how posting went. Returns how the account did, for the integrations page.
async fn finish(db: &PgPool, post: &SocialPost, account: &str, published: Result<String, Failure>) -> sqlx::Result<anyhow::Result<()>> {
    // Account failures, like expired cookies or a rate limit, happen before anything is posted, so the post waits again.
    let (status, url, error) = match published {
        Ok(url) => ("posted", url, None),
        Err(Failure::Post(reason)) => ("failed", String::new(), Some(reason)),
        Err(Failure::Account(reason)) => {
            sqlx::query("update social_posts set status = 'approved', updated_at = now() where id = $1 and status = 'posting'").bind(post.id).execute(db).await?;
            return Ok(Err(anyhow::anyhow!(reason)));
        }
    };
    sqlx::query(
        "update social_posts set status = $2, url = $3, error = $4, account = $5, posted_at = case when $2 = 'posted' then now() end,
           updated_at = now()
         where id = $1",
    )
    .bind(post.id)
    .bind(status)
    .bind(url)
    .bind(error)
    .bind(account)
    .execute(db)
    .await?;
    Ok(Ok(()))
}

async fn post_x(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let bird = state.cfg.bird_bin.as_deref().ok_or_else(|| Failure::Account("Posting to X needs BIRD_BIN set on the server".into()))?;
    let cookies = a.cookies(&state.cfg.app_secret).ok().flatten().ok_or_else(|| Failure::Account("Couldn't read the saved X cookies. Connect X again".into()))?;
    x::post(bird, &cookies, post.in_reply_to.as_deref(), &post.body).await
}

async fn post_reddit(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let login = a.login(&state.cfg.app_secret).ok().flatten().ok_or_else(|| Failure::Account("Couldn't read the saved Reddit login. Connect Reddit again".into()))?;
    let reddit = Reddit::sign_in(&login).await?;
    match post.in_reply_to.as_deref() {
        Some(link) => reddit.comment(&Target::parse(link).ok_or_else(|| Failure::Post("That isn't a link to a Reddit post".into()))?, &post.body).await,
        None => reddit.submit(&post.subreddit, &post.title, &post.body).await,
    }
}

async fn post_bluesky(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let login = a.bluesky(&state.cfg.app_secret).ok().flatten().ok_or_else(|| Failure::Account("Couldn't read the saved Bluesky app password. Connect Bluesky again".into()))?;
    Bluesky::sign_in(&login).await?.post(post.in_reply_to.as_deref(), &post.body).await
}

async fn post_mastodon(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    // The post's id as the idempotency key, so Mastodon posts it once even if the request goes twice.
    a.mastodon(&state.cfg.app_secret).await?.post(post.in_reply_to.as_deref(), &post.body, &post.id.to_string()).await
}

async fn post_threads(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    a.threads(&state.cfg.app_secret)?.post(post.in_reply_to.as_deref(), &post.body).await
}

async fn post_youtube(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let login = a.youtube(&state.cfg.app_secret).ok().flatten().ok_or_else(|| Failure::Account("Couldn't read the saved YouTube login. Connect YouTube again".into()))?;
    let link = post.in_reply_to.as_deref().ok_or_else(|| Failure::Post("YouTube only takes comments".into()))?;
    let target = youtube::Target::parse(link).ok_or_else(|| Failure::Post("That isn't a link to a YouTube video".into()))?;
    YouTube::sign_in(&login).await?.comment(&target, &post.body).await
}

async fn post_linkedin(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let unipile = a.unipile(&state.cfg.app_secret)?;
    match post.in_reply_to.as_deref() {
        Some(link) => {
            let urn = unipile::post_urn(link).ok_or_else(|| Failure::Post("That isn't a link to a LinkedIn post".into()))?;
            unipile.comment(&urn, &post.body).await.map(|_| link.to_string())
        }
        None => unipile.publish(&post.body).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn approved(db: &PgPool, project: Uuid, platform: &str) -> Uuid {
        sqlx::query_scalar("insert into social_posts (project_id, platform, body, status) values ($1, $2, 'Hi', 'approved') returning id")
            .bind(project)
            .bind(platform)
            .fetch_one(db)
            .await
            .unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn posting_stops_at_the_daily_cap_and_after_an_account_failure(db: PgPool) {
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into project_settings (project_id, x_auth_token_enc, x_handle) values ($1, 'x', 'acme')").bind(project).execute(&db).await.unwrap();
        let first = approved(&db, project, "x").await;
        approved(&db, project, "reddit").await;

        // The oldest approved X post comes first, and a post failure fails just that post.
        let due = next_due(&db, project, "x", "acme").await.unwrap().unwrap();
        assert_eq!(due.id, first);
        assert!(finish(&db, &due, "acme", Err(Failure::Post("Status is a duplicate. (187)".into()))).await.unwrap().is_ok());
        let failed: SocialPost = sqlx::query_as("select * from social_posts where id = $1").bind(first).fetch_one(&db).await.unwrap();
        assert_eq!((failed.status.as_str(), failed.error.as_deref()), ("failed", Some("Status is a duplicate. (187)")));

        // An account failure leaves the post approved for later.
        let next = approved(&db, project, "x").await;
        let due = next_due(&db, project, "x", "acme").await.unwrap().unwrap();
        // Claimed once, so a second worker or a restart can't send it again.
        assert!(claim(&db, &due, "acme").await.unwrap());
        assert!(!claim(&db, &due, "acme").await.unwrap());
        assert!(next_due(&db, project, "x", "acme").await.unwrap().is_none());
        assert!(finish(&db, &due, "acme", Err(Failure::Account("signed out".into()))).await.unwrap().is_err());
        sqlx::query("update project_settings set x_failed_at = now() where project_id = $1").bind(project).execute(&db).await.unwrap();
        assert!(next_due(&db, project, "x", "acme").await.unwrap().is_none());
        sqlx::query("update project_settings set x_failed_at = null where project_id = $1").bind(project).execute(&db).await.unwrap();

        let due = next_due(&db, project, "x", "acme").await.unwrap().unwrap();
        assert_eq!(due.id, next);
        finish(&db, &due, "acme", Ok("https://x.com/i/status/1".into())).await.unwrap().unwrap();
        let posted: SocialPost = sqlx::query_as("select * from social_posts where id = $1").bind(next).fetch_one(&db).await.unwrap();
        assert_eq!((posted.status.as_str(), posted.url.as_str(), posted.account.as_str()), ("posted", "https://x.com/i/status/1", "acme"));
        assert!(posted.posted_at.is_some());

        // 15 posts from the account in the last day, in any project, hold the rest until tomorrow.
        approved(&db, project, "x").await;
        sqlx::query(
            "insert into social_posts (project_id, platform, body, status, account, posted_at) select $1, 'x', 'Hi', 'posted', 'acme', now() from generate_series(1, 14)",
        )
        .bind(project)
        .execute(&db)
        .await
        .unwrap();
        assert!(next_due(&db, project, "x", "acme").await.unwrap().is_none());
        assert!(next_due(&db, project, "reddit", "acme_on_reddit").await.unwrap().is_some());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn linkedin_stops_at_five_posts_a_day_and_the_others_at_fifteen(db: PgPool) {
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        sqlx::query(
            "insert into project_settings (project_id, unipile_token_enc, unipile_account_id, bluesky_password_enc, bluesky_handle, threads_token_enc,
               threads_username, youtube_token_enc, youtube_channel_id)
             values ($1, 'x', 'ACC', 'x', 'acme.bsky.social', 'x', 'acme', 'x', 'UCacme')",
        )
        .bind(project)
        .execute(&db)
        .await
        .unwrap();
        let a = accounts(&db, project).await.unwrap().unwrap();
        assert_eq!(a.connected(), ["bluesky", "linkedin", "threads", "youtube"]);
        let accounts = [("linkedin", "ACC"), ("bluesky", "acme.bsky.social"), ("threads", "acme"), ("youtube", "UCacme")];
        for (platform, account) in accounts {
            approved(&db, project, platform).await;
            sqlx::query("insert into social_posts (project_id, platform, body, status, account, posted_at) select $1, $2, 'Hi', 'posted', $3, now() from generate_series(1, 5)")
                .bind(project)
                .bind(platform)
                .bind(account)
                .execute(&db)
                .await
                .unwrap();
        }
        assert!(next_due(&db, project, "linkedin", "ACC").await.unwrap().is_none());
        for (platform, account) in &accounts[1..] {
            assert!(next_due(&db, project, platform, account).await.unwrap().is_some(), "{platform}");
        }
        // Threads and YouTube stop at 15 too.
        sqlx::query("insert into social_posts (project_id, platform, body, status, account, posted_at) select $1, 'threads', 'Hi', 'posted', 'acme', now() from generate_series(1, 10)")
            .bind(project)
            .execute(&db)
            .await
            .unwrap();
        assert!(next_due(&db, project, "threads", "acme").await.unwrap().is_none());
        sqlx::query("update project_settings set youtube_failed_at = now() where project_id = $1").bind(project).execute(&db).await.unwrap();
        assert!(next_due(&db, project, "youtube", "UCacme").await.unwrap().is_none());
        // A Unipile failure, from sending LinkedIn messages or posting, holds LinkedIn posting too.
        sqlx::query("delete from social_posts where platform = 'linkedin' and status = 'posted'").execute(&db).await.unwrap();
        assert!(next_due(&db, project, "linkedin", "ACC").await.unwrap().is_some());
        sqlx::query("update project_settings set unipile_failed_at = now() where project_id = $1").bind(project).execute(&db).await.unwrap();
        assert!(next_due(&db, project, "linkedin", "ACC").await.unwrap().is_none());
    }

    #[test]
    fn each_platform_has_its_own_limit() {
        assert!(fits("x", true, &"a".repeat(280), 500) && !fits("x", true, &"a".repeat(281), 500));
        // 300 thumbs with a skin tone are 600 characters but 300 graphemes.
        assert!(fits("bluesky", false, &"👍🏽".repeat(300), 500) && !fits("bluesky", false, &"a".repeat(301), 500));
        assert!(fits("mastodon", false, &"a".repeat(1000), 1000) && !fits("mastodon", false, &"a".repeat(501), 500));
        assert!(fits("linkedin", false, &"a".repeat(3000), 500) && !fits("linkedin", true, &"a".repeat(1251), 500));
        assert!(fits("threads", true, &"é".repeat(500), 500) && !fits("threads", false, &"a".repeat(501), 500));
        assert!(fits("youtube", true, &"a".repeat(10_000), 500) && !fits("youtube", true, &"a".repeat(10_001), 500));
        assert!(!fits("reddit", true, "", 500));
    }
}
