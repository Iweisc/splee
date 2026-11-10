//! Posts approved X and Reddit drafts from each project's connected accounts: one per account each tick, at most
//! DAILY_POSTS a day, so a day's posts spread out instead of going in a burst. agent::social finds threads and drafts.

use std::collections::HashSet;

use sqlx::PgPool;
use uuid::Uuid;

use crate::agent::x::{self, Cookies};
use crate::crypto::decrypt;
use crate::hooks::{self, Alert};
use crate::models::SocialPost;
use crate::reddit::{Login, Reddit, Target};
use crate::state::AppState;

/// Posts per X or Reddit account a day. New accounts that post more than this get flagged as spam by both.
pub const DAILY_POSTS: i64 = 15;
/// A post still `posting` after this long was cut off mid-send, by a crash or a restart.
const STUCK_MINUTES: i32 = 15;

#[derive(Debug)]
pub enum Failure {
    /// Something about this post, like a deleted thread or a ban from the subreddit. The post fails with the reason.
    Post(String),
    /// The account, like expired cookies or a rate limit. Its posting stops for an hour and the post waits.
    Account(String),
}

/// A project's X and Reddit sign-ins as stored, encrypted.
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
}

const ACCOUNTS: &str = "select s.project_id, s.x_auth_token_enc, s.x_ct0_enc, s.x_handle, s.reddit_client_id, s.reddit_secret_enc, s.reddit_username,
                               s.reddit_password_enc
                        from project_settings s";

pub async fn accounts(db: &PgPool, project_id: Uuid) -> sqlx::Result<Option<Accounts>> {
    sqlx::query_as(&format!("{ACCOUNTS} where s.project_id = $1")).bind(project_id).fetch_optional(db).await
}

impl Accounts {
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
        for platform in ["x", "reddit"] {
            let account = if platform == "x" { &a.x_handle } else { &a.reddit_username };
            if !used.insert((platform, account.to_lowercase())) {
                continue;
            }
            if let Err(e) = publish_next(state, &a, platform).await {
                tracing::error!(project_id = %a.project_id, platform, error = ?e, "social posting failed");
            }
        }
    }
    Ok(())
}

async fn publish_next(state: &AppState, a: &Accounts, platform: &str) -> anyhow::Result<()> {
    let (account, enc, alert) = match platform {
        "x" => (&a.x_handle, &a.x_auth_token_enc, Alert::X),
        _ => (&a.reddit_username, &a.reddit_password_enc, Alert::Reddit),
    };
    if enc.is_empty() {
        return Ok(());
    }
    let Some(post) = next_due(&state.db, a.project_id, platform, account).await? else { return Ok(()) };
    if !claim(&state.db, &post, account).await? {
        return Ok(());
    }
    let published = match platform {
        "x" => post_x(state, a, &post).await,
        _ => post_reddit(state, a, &post).await,
    };
    let result = finish(&state.db, &post, account, published).await?;
    hooks::record(&state.db, a.project_id, alert, enc, &result).await;
    Ok(())
}

/// The project's longest-approved post on the platform, unless the account (in any project) is at its daily cap or had
/// a failure in the last hour.
async fn next_due(db: &PgPool, project_id: Uuid, platform: &str, account: &str) -> sqlx::Result<Option<SocialPost>> {
    // The names are fixed strings, never user input.
    let (name, failed_at) = if platform == "x" { ("x_handle", "x_failed_at") } else { ("reddit_username", "reddit_failed_at") };
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
    .bind(DAILY_POSTS)
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
}
