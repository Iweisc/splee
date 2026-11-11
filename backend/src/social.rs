//! Posts approved social drafts from each project's connected accounts: one per account each tick, at most the platform's
//! daily cap, so a day's posts spread out instead of going in a burst. agent::social finds threads and drafts.

use std::collections::HashSet;

use sqlx::PgPool;
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

use crate::agent::x::{self, Cookies};
use crate::bluesky::{self, Bluesky};
use crate::crypto::{decrypt, encrypt};
use crate::dev::Dev;
use crate::discord;
use crate::discourse::{self, Discourse, Topic};
use crate::hashnode::Hashnode;
use crate::hooks::{self, Alert};
use crate::lemmy::{self, Lemmy};
use crate::mastodon::Mastodon;
use crate::meta::{self, Graph, Published};
use crate::models::{DiscourseForum, SocialChannel, SocialPost};
use crate::nostr::{self, Nostr};
use crate::reddit::{Login, Reddit, Target};
use crate::stackexchange::{self, Question, StackExchange};
use crate::state::AppState;
use crate::telegram;
use crate::threads::{self, Threads};
use crate::tumblr::{self, Tumblr};
use crate::unipile::{self, Unipile};
use crate::youtube::{self, YouTube};

pub const PLATFORMS: [&str; 18] = [
    "x", "reddit", "bluesky", "mastodon", "linkedin", "threads", "youtube", "stackexchange", "dev", "discourse", "lemmy", "nostr", "tumblr", "hashnode", "discord",
    "telegram", "facebook", "instagram",
];
/// A post still `posting` after this long was cut off mid-send, by a crash or a restart.
const STUCK_MINUTES: i32 = 15;

/// Posts per account a day. New accounts that post more get flagged as spam, and LinkedIn restricts automation the most. A
/// Stack Exchange answer and a DEV or Hashnode article take far longer to read and judge than a reply, so they get fewer, and
/// so does each Discourse forum, Lemmy, Tumblr and the Discord and Telegram bots, whose communities are small enough that a
/// company replying often stands out. Hashnode's articles and comments each have their own cap, and so do a Facebook Page's
/// and an Instagram account's posts and answers to comments, since followers see every post but only the commenter an answer.
pub fn daily_cap(platform: &str, reply: bool) -> i64 {
    match (platform, reply) {
        ("linkedin" | "stackexchange" | "discourse", _) | ("facebook" | "instagram", false) => 5,
        ("lemmy" | "tumblr" | "discord" | "telegram", _) | ("hashnode", true) => 10,
        ("dev" | "hashnode", _) => 2,
        _ => 15,
    }
}

/// Whether replies and original posts count against the daily cap apart.
fn split_cap(platform: &str) -> bool {
    daily_cap(platform, true) != daily_cap(platform, false)
}

pub fn name(platform: &str) -> &'static str {
    match platform {
        "x" => "X",
        "reddit" => "Reddit",
        "bluesky" => "Bluesky",
        "mastodon" => "Mastodon",
        "threads" => "Threads",
        "youtube" => "YouTube",
        "stackexchange" => "Stack Exchange",
        "dev" => "DEV",
        "discourse" => "Discourse",
        "lemmy" => "Lemmy",
        "nostr" => "Nostr",
        "tumblr" => "Tumblr",
        "hashnode" => "Hashnode",
        "discord" => "Discord",
        "telegram" => "Telegram",
        "facebook" => "Facebook",
        "instagram" => "Instagram",
        _ => "LinkedIn",
    }
}

/// YouTube only takes comments and Stack Exchange answers, so they get no original posts.
pub fn takes_posts(platform: &str) -> bool {
    !matches!(platform, "youtube" | "stackexchange")
}

/// Forem's API can't write comments, so DEV only gets articles.
pub fn takes_replies(platform: &str) -> bool {
    platform != "dev"
}

/// The most a reply or post can hold, as `length` counts it. Mastodon's limit is the instance's, read on connect.
pub fn max_length(platform: &str, reply: bool, mastodon_chars: usize) -> usize {
    match (platform, reply) {
        ("x", _) => 280,
        ("bluesky", _) => bluesky::GRAPHEMES,
        ("mastodon", _) => mastodon_chars,
        ("threads", _) => threads::CHARS,
        ("youtube", _) => youtube::CHARS,
        ("stackexchange", _) => stackexchange::CHARS,
        ("discourse", _) => discourse::CHARS,
        ("lemmy", _) => lemmy::CHARS,
        ("nostr", _) => nostr::CHARS,
        ("tumblr", _) => tumblr::CHARS,
        ("discord", _) => discord::CHARS,
        ("telegram", _) => telegram::CHARS,
        ("facebook", true) => meta::COMMENT_CHARS,
        ("facebook", false) => meta::POST_CHARS,
        ("instagram", _) => meta::CAPTION_CHARS,
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

/// The least a reply or post needs. Stack Exchange and Discourse turn down short ones.
pub fn min_length(platform: &str) -> usize {
    match platform {
        "stackexchange" => stackexchange::MIN_CHARS,
        "discourse" => discourse::MIN_CHARS,
        _ => 1,
    }
}

pub fn fits(platform: &str, reply: bool, text: &str, mastodon_chars: usize) -> bool {
    (min_length(platform)..=max_length(platform, reply, mastodon_chars)).contains(&length(platform, text))
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
#[derive(Default, sqlx::FromRow)]
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
    pub stackexchange_key: String,
    pub stackexchange_token_enc: String,
    pub stackexchange_site: String,
    pub stackexchange_account: String,
    pub dev_key_enc: String,
    pub dev_username: String,
    pub lemmy_url: String,
    pub lemmy_username: String,
    pub lemmy_password_enc: String,
    pub lemmy_jwt_enc: String,
    pub lemmy_handle: String,
    pub nostr_key_enc: String,
    pub nostr_npub: String,
    pub nostr_relays: Vec<String>,
    pub tumblr_username: String,
    pub tumblr_blog: String,
    pub hashnode_token_enc: String,
    pub hashnode_username: String,
    pub hashnode_publication_id: String,
    pub discord_token_enc: String,
    pub discord_bot_id: String,
    pub discord_bot: String,
    pub telegram_token_enc: String,
    pub telegram_bot_id: String,
    pub telegram_bot: String,
    pub telegram_reads_all: bool,
    pub telegram_offset: i64,
    /// Who signed in with Facebook, empty when nobody is. Each Page's token is on its channel.
    pub meta_user_id: String,
    /// Meta's usage headers asked to wait, see meta::settle.
    pub meta_held: bool,
    pub unipile_dsn: String,
    pub unipile_token_enc: String,
    pub unipile_account_id: String,
    /// Each forum is an account of its own. Loaded with `load_forums`.
    #[sqlx(skip)]
    pub forums: Vec<DiscourseForum>,
    /// The Discord and Telegram channels the bots can see, and the Facebook Pages and Instagram accounts. Loaded with
    /// `load_forums` too.
    #[sqlx(skip)]
    pub channels: Vec<SocialChannel>,
}

pub(crate) const ACCOUNTS: &str = "select s.project_id, s.x_auth_token_enc, s.x_ct0_enc, s.x_handle, s.reddit_client_id, s.reddit_secret_enc, s.reddit_username,
                               s.reddit_password_enc, s.bluesky_handle, s.bluesky_password_enc, s.mastodon_url, s.mastodon_token_enc,
                               s.mastodon_handle, s.mastodon_max_chars, s.threads_token_enc, s.threads_user_id, s.threads_username,
                               s.youtube_client_id, s.youtube_secret_enc, s.youtube_token_enc, s.youtube_channel_id, s.stackexchange_key,
                               s.stackexchange_token_enc, s.stackexchange_site, s.stackexchange_account, s.dev_key_enc, s.dev_username,
                               s.lemmy_url, s.lemmy_username, s.lemmy_password_enc, s.lemmy_jwt_enc, s.lemmy_handle, s.nostr_key_enc,
                               s.nostr_npub, s.nostr_relays, s.tumblr_username, s.tumblr_blog, s.hashnode_token_enc, s.hashnode_username,
                               s.hashnode_publication_id, s.discord_token_enc, s.discord_bot_id, s.discord_bot, s.telegram_token_enc,
                               s.telegram_bot_id, s.telegram_bot, s.telegram_reads_all, s.telegram_offset, s.meta_user_id,
                               coalesce(s.meta_held_until > now(), false) as meta_held, s.unipile_dsn, s.unipile_token_enc, s.unipile_account_id
                        from project_settings s";

pub async fn accounts(db: &PgPool, project_id: Uuid) -> sqlx::Result<Option<Accounts>> {
    let Some(mut a) = sqlx::query_as::<_, Accounts>(&format!("{ACCOUNTS} where s.project_id = $1")).bind(project_id).fetch_optional(db).await? else {
        return Ok(None);
    };
    a.load_forums(db).await?;
    Ok(Some(a))
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
        "stackexchange" => ("stackexchange_account", "stackexchange_failed_at"),
        "dev" => ("dev_username", "dev_failed_at"),
        "lemmy" => ("lemmy_handle", "lemmy_failed_at"),
        "nostr" => ("nostr_npub", "nostr_failed_at"),
        "tumblr" => ("tumblr_username", "tumblr_failed_at"),
        "hashnode" => ("hashnode_username", "hashnode_failed_at"),
        "discord" => ("discord_bot_id", "discord_failed_at"),
        "telegram" => ("telegram_bot_id", "telegram_failed_at"),
        "facebook" => ("meta_user_id", "facebook_failed_at"),
        "instagram" => ("meta_user_id", "instagram_failed_at"),
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
        "stackexchange" => Alert::StackExchange,
        "dev" => Alert::Dev,
        "discourse" => Alert::Discourse,
        "lemmy" => Alert::Lemmy,
        "nostr" => Alert::Nostr,
        "tumblr" => Alert::Tumblr,
        "hashnode" => Alert::Hashnode,
        "discord" => Alert::Discord,
        "telegram" => Alert::Telegram,
        "facebook" => Alert::Facebook,
        "instagram" => Alert::Instagram,
        _ => Alert::Unipile,
    }
}

impl Accounts {
    /// The account's name and its encrypted secret, which hooks::record matches on. The secret is empty when it isn't connected.
    /// Discourse has one account per forum, see `forums`, and Facebook and Instagram one per Page or account, see `meta_accounts`.
    pub fn account(&self, platform: &str) -> (&str, &str) {
        match platform {
            "x" => (&self.x_handle, &self.x_auth_token_enc),
            "reddit" => (&self.reddit_username, &self.reddit_password_enc),
            "bluesky" => (&self.bluesky_handle, &self.bluesky_password_enc),
            "mastodon" => (&self.mastodon_handle, &self.mastodon_token_enc),
            "threads" => (&self.threads_username, &self.threads_token_enc),
            "youtube" => (&self.youtube_channel_id, &self.youtube_token_enc),
            "stackexchange" => (&self.stackexchange_account, &self.stackexchange_token_enc),
            "dev" => (&self.dev_username, &self.dev_key_enc),
            "lemmy" => (&self.lemmy_handle, &self.lemmy_password_enc),
            "nostr" => (&self.nostr_npub, &self.nostr_key_enc),
            // Its refresh token changes each time it's used, so the username, set only while connected, stands in for it.
            "tumblr" => (&self.tumblr_username, &self.tumblr_username),
            "hashnode" => (&self.hashnode_username, &self.hashnode_token_enc),
            "discord" => (&self.discord_bot_id, &self.discord_token_enc),
            "telegram" => (&self.telegram_bot_id, &self.telegram_token_enc),
            "facebook" | "instagram" => (&self.meta_user_id, &self.meta_user_id),
            "discourse" => ("", ""),
            _ => (&self.unipile_account_id, &self.unipile_token_enc),
        }
    }

    pub fn connected(&self) -> Vec<&'static str> {
        PLATFORMS.into_iter().filter(|p| if *p == "discourse" { !self.forums.is_empty() } else { !self.account(p).1.is_empty() }).collect()
    }

    pub async fn load_forums(&mut self, db: &PgPool) -> sqlx::Result<()> {
        self.forums = sqlx::query_as("select * from discourse_forums where project_id = $1 order by created_at").bind(self.project_id).fetch_all(db).await?;
        self.channels = crate::social_channels::list(db, self.project_id).await?;
        Ok(())
    }

    /// The picked Pages or Instagram accounts that still have a token.
    pub fn meta_accounts(&self, platform: &str) -> Vec<&SocialChannel> {
        self.channels.iter().filter(|c| c.platform == platform && c.picked && !c.token_enc.is_empty()).collect()
    }

    /// A client with the Page's token, which is its Instagram account's too.
    pub fn graph(&self, state: &AppState, c: &SocialChannel) -> Result<Graph, Failure> {
        let token = decrypt(&state.cfg.app_secret, &c.token_enc).map_err(|_| Failure::Account(format!("Couldn't read the saved token for {}. Sign in with Facebook again", c.label())))?;
        Graph::new(&state.cfg.graph_api_url, &token)
    }

    /// The Lemmy home instance's host, which a community's bare name is on.
    pub fn lemmy_host(&self) -> &str {
        self.lemmy_url.trim_start_matches("https://")
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

    pub fn stackexchange(&self, secret: &str) -> Result<StackExchange, Failure> {
        let token = decrypt(secret, &self.stackexchange_token_enc)
            .map_err(|_| Failure::Account("Couldn't read the saved Stack Exchange token. Connect Stack Exchange again".into()))?;
        StackExchange::new(&self.stackexchange_key, &token, &self.stackexchange_site)
    }

    pub fn dev(&self, secret: &str) -> Result<Dev, Failure> {
        let key = decrypt(secret, &self.dev_key_enc).map_err(|_| Failure::Account("Couldn't read the saved DEV API key. Connect DEV again".into()))?;
        Dev::new(&key)
    }

    pub async fn lemmy(&self, secret: &str) -> Result<Lemmy, Failure> {
        let password = decrypt(secret, &self.lemmy_password_enc).map_err(|_| Failure::Account("Couldn't read the saved Lemmy password. Connect Lemmy again".into()))?;
        // A JWT that can't be read is only traded for a new one.
        let jwt = decrypt(secret, &self.lemmy_jwt_enc).unwrap_or_default();
        Lemmy::new(&self.lemmy_url, &self.lemmy_username, &password, &jwt).await
    }

    /// Saves the JWT the client ended up with when it signed in again, so the next call doesn't have to.
    pub async fn keep_lemmy_jwt(&self, db: &PgPool, secret: &str, lemmy: &Lemmy) {
        let jwt = lemmy.jwt();
        if jwt.is_empty() || decrypt(secret, &self.lemmy_jwt_enc).is_ok_and(|old| old == jwt) {
            return;
        }
        // Matched on the password, so a reconnect since doesn't get an old account's JWT.
        let saved = sqlx::query("update project_settings set lemmy_jwt_enc = $2 where project_id = $1 and lemmy_password_enc = $3")
            .bind(self.project_id)
            .bind(encrypt(secret, &jwt))
            .bind(&self.lemmy_password_enc)
            .execute(db)
            .await;
        if let Err(e) = saved {
            tracing::warn!(error = %e, "couldn't save the Lemmy JWT");
        }
    }

    pub fn nostr(&self, secret: &str) -> Result<Nostr, Failure> {
        let key = decrypt(secret, &self.nostr_key_enc).ok().and_then(|k| nostr::secret_key(&k));
        let key = key.ok_or_else(|| Failure::Account("Couldn't read the saved Nostr key. Connect Nostr again".into()))?;
        Ok(Nostr::new(key, self.nostr_relays.clone()))
    }

    /// Renews the access token first when it's about to run out.
    pub async fn tumblr(&self, state: &AppState) -> Result<Tumblr, Failure> {
        let token = tumblr::access_token(&state.db, &state.cfg, self.project_id).await?;
        Tumblr::new(tumblr::app(&state.cfg)?, &token, &self.tumblr_blog)
    }

    pub fn hashnode(&self, secret: &str) -> Result<Hashnode, Failure> {
        let token = decrypt(secret, &self.hashnode_token_enc).map_err(|_| Failure::Account("Couldn't read the saved Hashnode token. Connect Hashnode again".into()))?;
        Hashnode::new(&token)
    }

    pub fn unipile(&self, secret: &str) -> Result<Unipile, Failure> {
        let token = decrypt(secret, &self.unipile_token_enc).map_err(|_| Failure::Account("Couldn't read the saved Unipile token. Connect Unipile again".into()))?;
        Unipile::new(&self.unipile_dsn, &token, &self.unipile_account_id).map_err(|e| Failure::Account(e.to_string()))
    }

    pub fn mastodon_chars(&self) -> usize {
        self.mastodon_max_chars.max(1) as usize
    }
}

impl DiscourseForum {
    pub async fn client(&self, secret: &str) -> Result<Discourse, Failure> {
        let key = decrypt(secret, &self.key_enc).map_err(|_| Failure::Account(format!("Couldn't read the saved key for {}. Connect the forum again", self.url)))?;
        Discourse::new(&self.url, &key, (self.key_kind == "admin").then_some(self.username.as_str())).await
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
    for mut a in waiting {
        a.load_forums(&state.db).await?;
        for platform in a.connected() {
            let accounts = match platform {
                "discourse" => a.forums.iter().map(|f| (f.account.as_str(), f.key_enc.as_str(), Some(f))).collect(),
                // Each Page and Instagram account has its own daily cap, while a failure holds them all, since they share a sign-in.
                "facebook" | "instagram" => a.meta_accounts(platform).into_iter().map(|c| (c.chat_id.as_str(), a.meta_user_id.as_str(), None)).collect(),
                _ => vec![(a.account(platform).0, a.account(platform).1, None)],
            };
            for (account, enc, forum) in accounts {
                if let Err(e) = publish_next(state, &a, platform, account, enc, forum, &mut used).await {
                    tracing::error!(project_id = %a.project_id, platform, error = ?e, "social posting failed");
                }
            }
        }
    }
    Ok(())
}

/// `used` holds the accounts that already posted this tick. An account only counts once it claims a post, so a project with
/// nothing due doesn't hold up another project on the same account. `forum` is the Discourse forum `account` is on.
async fn publish_next(
    state: &AppState,
    a: &Accounts,
    platform: &'static str,
    account: &str,
    enc: &str,
    forum: Option<&DiscourseForum>,
    used: &mut HashSet<(&'static str, String)>,
) -> anyhow::Result<()> {
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
        "stackexchange" => post_stackexchange(state, a, &post).await,
        "dev" => post_dev(state, a, &post).await,
        "lemmy" => post_lemmy(state, a, &post).await,
        "nostr" => post_nostr(state, a, &post).await,
        "tumblr" => post_tumblr(state, a, &post).await,
        "hashnode" => post_hashnode(state, a, &post).await,
        "discord" => post_discord(state, a, &post).await,
        "telegram" => post_telegram(state, a, &post).await,
        "facebook" => post_facebook(state, a, &post).await,
        "instagram" => post_instagram(state, a, &post).await,
        "discourse" => match forum {
            Some(forum) => post_discourse(state, forum, &post).await,
            None => Err(Failure::Post("The forum isn't connected anymore".into())),
        },
        _ => post_linkedin(state, a, &post).await,
    };
    let result = finish(&state.db, &post, account, published).await?;
    hooks::record(&state.db, a.project_id, alert(platform), enc, &result).await;
    Ok(())
}

/// The project's longest-approved post on the platform, unless the account (in any project) is at the platform's daily cap,
/// had a failure in the last hour or, on Stack Exchange and Discourse, is waiting out the wait the API asked for. A Discourse
/// forum is its own account, which only posts what's for that forum, and so is a Page or an Instagram account, which also
/// waits out Meta's usage. On Hashnode, Facebook and Instagram, posts wait at their cap while comments still go out, and the
/// other way around.
async fn next_due(db: &PgPool, project_id: Uuid, platform: &str, account: &str) -> sqlx::Result<Option<SocialPost>> {
    let held = match platform {
        "discourse" => "and sp.forum_id in (select f.id from discourse_forums f where f.project_id = $1 and f.account = $3)
           and not exists (select 1 from discourse_forums f
                           where f.account = $3 and (f.failed_at > now() - interval '1 hour' or f.held_until > now()))"
            .to_string(),
        "facebook" | "instagram" => format!(
            "and sp.channel_id in (select c.id from social_channels c where c.project_id = $1 and c.platform = $2 and c.chat_id = $3)
             and not exists (select 1 from project_settings s
                             where s.project_id = $1 and (s.{platform}_failed_at > now() - interval '1 hour' or s.meta_held_until > now()))"
        ),
        _ => {
            let (name, failed_at) = columns(platform);
            // The wait Stack Exchange, Discord or Telegram asked for.
            let backoff = match platform {
                "stackexchange" => "and not exists (select 1 from project_settings s where s.stackexchange_account = $3 and s.stackexchange_backoff_until > now())",
                "discord" => "and not exists (select 1 from project_settings s where s.discord_bot_id = $3 and s.discord_held_until > now())",
                "telegram" => "and not exists (select 1 from project_settings s where s.telegram_bot_id = $3 and s.telegram_held_until > now())",
                _ => "",
            };
            format!("and not exists (select 1 from project_settings s where s.{name} = $3 and s.{failed_at} > now() - interval '1 hour') {backoff}")
        }
    };
    sqlx::query_as(&format!(
        "select sp.* from social_posts sp
         where sp.project_id = $1 and sp.platform = $2 and sp.status = 'approved'
           and (select count(*) from social_posts o where o.platform = $2 and o.account = $3
                and (not $6 or (o.in_reply_to is null) = (sp.in_reply_to is null))
                and (o.status = 'posting' or o.status = 'posted' and o.posted_at > now() - interval '1 day'))
               < case when sp.in_reply_to is null then $4 else $5 end
           {held}
         order by sp.updated_at limit 1"
    ))
    .bind(project_id)
    .bind(platform)
    .bind(account)
    .bind(daily_cap(platform, false))
    .bind(daily_cap(platform, true))
    .bind(split_cap(platform))
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

async fn post_stackexchange(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let link = post.in_reply_to.as_deref().ok_or_else(|| Failure::Post("Stack Exchange only takes answers".into()))?;
    let question = Question::parse(link).ok_or_else(|| Failure::Post("That isn't a link to a Stack Exchange question".into()))?;
    let se = a.stackexchange(&state.cfg.app_secret)?;
    let answered = se.answer(&question, &post.body).await;
    // The client forgets the backoff once this returns, and it can outlast the ten minutes until the account's next answer.
    let wait = se.backoff("answer");
    if wait > 0 {
        let held = sqlx::query("update project_settings set stackexchange_backoff_until = now() + make_interval(secs => $2) where stackexchange_account = $1")
            .bind(&a.stackexchange_account)
            .bind(wait as f64)
            .execute(&state.db)
            .await;
        if let Err(e) = held {
            tracing::warn!(error = %e, "couldn't save the Stack Exchange backoff");
        }
    }
    answered
}

async fn post_dev(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    a.dev(&state.cfg.app_secret)?.publish(&post.title, &post.body, &post.tags).await
}

async fn post_lemmy(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let lemmy = a.lemmy(&state.cfg.app_secret).await?;
    let posted = match post.in_reply_to.as_deref() {
        Some(link) => {
            let link = lemmy::Link::parse(link).ok_or_else(|| Failure::Post("That isn't a link to a Lemmy post or comment".into()))?;
            lemmy.comment(&link, &post.body).await
        }
        None => lemmy.create_post(&post.subreddit, &post.title, &post.body).await,
    };
    a.keep_lemmy_jwt(&state.db, &state.cfg.app_secret, &lemmy).await;
    posted
}

async fn post_nostr(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let nostr = a.nostr(&state.cfg.app_secret)?;
    match post.in_reply_to.as_deref() {
        Some(link) => nostr.reply(link, &post.body).await,
        None => nostr.post(&post.body).await,
    }
}

async fn post_tumblr(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let tumblr = a.tumblr(state).await?;
    match post.in_reply_to.as_deref() {
        Some(link) => tumblr.reblog(&tumblr::PostRef::parse(link).ok_or_else(|| Failure::Post("That isn't a link to a Tumblr post".into()))?, &post.body).await,
        None => tumblr.create(&post.body, &post.tags).await,
    }
}

async fn post_hashnode(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let hashnode = a.hashnode(&state.cfg.app_secret)?;
    match post.in_reply_to.as_deref() {
        Some(link) => hashnode.comment(link, &post.body).await,
        None if a.hashnode_publication_id.is_empty() => Err(Failure::Account("Pick a Hashnode publication for articles first".into())),
        None => hashnode.publish(&a.hashnode_publication_id, &post.title, &post.body, &post.tags).await,
    }
}

/// The picked channel a Discord or Telegram post goes to, or the Page or Instagram account.
fn channel<'a>(a: &'a Accounts, post: &SocialPost) -> Result<&'a SocialChannel, Failure> {
    let channel = a.channels.iter().find(|c| Some(c.id) == post.channel_id && c.platform == post.platform);
    let gone = match post.platform.as_str() {
        "facebook" => "The Page isn't one you manage anymore".to_string(),
        "instagram" => "The Instagram account isn't linked to a Page you manage anymore".to_string(),
        p => format!("The bot can't see that {} channel anymore", name(p)),
    };
    let channel = channel.ok_or(Failure::Post(gone))?;
    if !channel.picked {
        return Err(Failure::Post(format!("Pick {} on the integrations page again to post there", channel.label())));
    }
    Ok(channel)
}

async fn post_discord(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let c = channel(a, post)?;
    let reply = match post.in_reply_to.as_deref() {
        Some(link) => Some(discord::parse_link(link).ok_or_else(|| Failure::Post("That isn't a link to a Discord message".into()))?.2),
        None => None,
    };
    let discord = a.discord(state)?;
    // The post's id, cut to Discord's 25 characters, so a second send of it within a few minutes is dropped.
    let nonce: String = post.id.simple().to_string().chars().take(25).collect();
    let sent = discord.send(&c.guild_id, &c.chat_id, &post.body, reply.as_deref(), &nonce).await;
    if let Some(wait) = discord.retry_after() {
        crate::social_channels::hold(&state.db, a.project_id, "discord", wait).await;
    }
    sent
}

async fn post_telegram(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let c = channel(a, post)?;
    let reply = match post.in_reply_to.as_deref() {
        Some(link) => Some(telegram::parse_link(link).ok_or_else(|| Failure::Post("That isn't a link to a Telegram message".into()))?.1),
        None => None,
    };
    let tg = a.telegram(state)?;
    let sent = tg.send(&c.chat_id, &post.body, reply.as_deref()).await;
    if let Some(wait) = tg.retry_after() {
        crate::social_channels::hold(&state.db, a.project_id, "telegram", wait).await;
    }
    sent
}

/// A Page post, or an answer to a comment on one of the Page's posts.
async fn post_facebook(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let c = channel(a, post)?;
    let graph = a.graph(state, c)?;
    let sent = match post.in_reply_to.as_deref() {
        Some(link) => match meta::facebook_comment(link) {
            Some((_, comment)) => graph.reply_facebook(&comment, &post.body).await,
            None => Err(Failure::Post("That isn't a link to a comment on your Page".into())),
        },
        None => graph.page_post(&c.chat_id, &post.body, &post.link).await,
    };
    meta::settle(&state.db, a.project_id, &graph).await;
    sent
}

/// An answer to a comment, or a post, which Instagram makes in two steps: a media container for the picture, then publishing
/// it once it's ready. The container is saved first, so a try that finds it still processing leaves it for the next one, and
/// one that published it without hearing back finds it published. It's dropped only when Instagram rejects the container.
async fn post_instagram(state: &AppState, a: &Accounts, post: &SocialPost) -> Result<String, Failure> {
    let c = channel(a, post)?;
    let graph = a.graph(state, c)?;
    let sent = match post.in_reply_to.as_deref() {
        Some(link) => match meta::instagram_comment(link) {
            Some((_, comment)) => graph.reply_instagram(&comment, &post.body, link).await,
            None => Err(Failure::Post("That isn't a link to a comment on your Instagram account".into())),
        },
        None => publish_instagram(&state.db, &graph, c, post).await,
    };
    meta::settle(&state.db, a.project_id, &graph).await;
    sent
}

async fn publish_instagram(db: &PgPool, graph: &Graph, c: &SocialChannel, post: &SocialPost) -> Result<String, Failure> {
    let container = match post.container_id.as_str() {
        "" => {
            let made = graph.container(&c.chat_id, &post.image_url, &post.body).await?;
            let saved = sqlx::query("update social_posts set container_id = $2 where id = $1").bind(post.id).bind(&made).execute(db).await;
            saved.map_err(|e| Failure::Post(e.to_string()))?;
            made
        }
        made => made.to_string(),
    };
    match graph.publish(&c.chat_id, &container).await? {
        Published::Live(url) => Ok(url),
        Published::Processing => Err(Failure::Account("Instagram is still getting the picture ready. The post goes out on the next try".into())),
        Published::Rejected(why) => {
            if let Err(e) = sqlx::query("update social_posts set container_id = '' where id = $1").bind(post.id).execute(db).await {
                tracing::warn!(error = %e, "couldn't drop a container Instagram turned down");
            }
            Err(Failure::Post(why))
        }
    }
}

async fn post_discourse(state: &AppState, forum: &DiscourseForum, post: &SocialPost) -> Result<String, Failure> {
    let d = forum.client(&state.cfg.app_secret).await?;
    let posted = match (post.in_reply_to.as_deref(), forum.category_id) {
        (Some(link), _) => d.reply(&Topic::parse(link).ok_or_else(|| Failure::Post("That isn't a link to a Discourse topic".into()))?, &post.body).await,
        (None, Some(category)) => d.create_topic(category, &post.title, &post.body).await,
        (None, None) => Err(Failure::Post(format!("Pick a category for new topics on {} first", forum.url))),
    };
    // The forum's Retry-After can be shorter or longer than the hour a failure holds it.
    if let Some(wait) = d.retry_after() {
        let held = sqlx::query("update discourse_forums set held_until = now() + make_interval(secs => $2) where account = $1")
            .bind(&forum.account)
            .bind(wait as f64)
            .execute(&state.db)
            .await;
        if let Err(e) = held {
            tracing::warn!(error = %e, "couldn't save the forum's Retry-After");
        }
    }
    posted
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
        // Stack Exchange stops at 5 and DEV at 2, and the API's backoff holds Stack Exchange too.
        sqlx::query(
            "update project_settings set stackexchange_token_enc = 'x', stackexchange_account = 'stackoverflow.com/users/31', dev_key_enc = 'x',
               dev_username = 'acme' where project_id = $1",
        )
        .bind(project)
        .execute(&db)
        .await
        .unwrap();
        for (platform, account, posted) in [("stackexchange", "stackoverflow.com/users/31", 4), ("dev", "acme", 1)] {
            approved(&db, project, platform).await;
            let post = "insert into social_posts (project_id, platform, body, status, account, posted_at) select $1, $2, 'Hi', 'posted', $3, now() from generate_series(1, $4)";
            sqlx::query(post).bind(project).bind(platform).bind(account).bind(posted).execute(&db).await.unwrap();
            assert!(next_due(&db, project, platform, account).await.unwrap().is_some(), "{platform}");
            sqlx::query(post).bind(project).bind(platform).bind(account).bind(1).execute(&db).await.unwrap();
            assert!(next_due(&db, project, platform, account).await.unwrap().is_none(), "{platform}");
        }
        sqlx::query("delete from social_posts where platform = 'stackexchange' and status = 'posted'").execute(&db).await.unwrap();
        assert!(next_due(&db, project, "stackexchange", "stackoverflow.com/users/31").await.unwrap().is_some());
        sqlx::query("update project_settings set stackexchange_backoff_until = now() + interval '5 minutes' where project_id = $1").bind(project).execute(&db).await.unwrap();
        assert!(next_due(&db, project, "stackexchange", "stackoverflow.com/users/31").await.unwrap().is_none());
        // A Unipile failure, from sending LinkedIn messages or posting, holds LinkedIn posting too.
        sqlx::query("delete from social_posts where platform = 'linkedin' and status = 'posted'").execute(&db).await.unwrap();
        assert!(next_due(&db, project, "linkedin", "ACC").await.unwrap().is_some());
        sqlx::query("update project_settings set unipile_failed_at = now() where project_id = $1").bind(project).execute(&db).await.unwrap();
        assert!(next_due(&db, project, "linkedin", "ACC").await.unwrap().is_none());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_discourse_forum_posts_its_own_and_waits_out_its_retry_after(db: PgPool) {
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into project_settings (project_id, lemmy_password_enc, lemmy_handle) values ($1, 'x', 'ana@lemmy.world')")
            .bind(project)
            .execute(&db)
            .await
            .unwrap();
        let mut forums = Vec::new();
        for (url, account) in [("https://forum.acme.com", "forum.acme.com/u/ana"), ("https://acme.com/forum", "acme.com/forum/u/ana")] {
            let id: Uuid = sqlx::query_scalar(
                "insert into discourse_forums (project_id, url, username, key_kind, key_enc, account) values ($1, $2, 'ana', 'user', $3, $3) returning id",
            )
            .bind(project)
            .bind(url)
            .bind(account)
            .fetch_one(&db)
            .await
            .unwrap();
            forums.push((id, account));
        }
        let a = accounts(&db, project).await.unwrap().unwrap();
        assert_eq!(a.connected(), ["discourse", "lemmy"]);
        let (first, second) = (forums[0], forums[1]);
        let post: Uuid = sqlx::query_scalar("insert into social_posts (project_id, platform, body, status, forum_id) values ($1, 'discourse', 'Hi', 'approved', $2) returning id")
            .bind(project)
            .bind(first.0)
            .fetch_one(&db)
            .await
            .unwrap();
        // The post is for the first forum, so the second has nothing due.
        assert_eq!(next_due(&db, project, "discourse", first.1).await.unwrap().map(|p| p.id), Some(post));
        assert!(next_due(&db, project, "discourse", second.1).await.unwrap().is_none());
        // A 429's Retry-After holds that forum only.
        sqlx::query("update discourse_forums set held_until = now() + interval '2 minutes' where id = $1").bind(first.0).execute(&db).await.unwrap();
        assert!(next_due(&db, project, "discourse", first.1).await.unwrap().is_none());
        sqlx::query("update discourse_forums set held_until = now() - interval '1 second' where id = $1").bind(first.0).execute(&db).await.unwrap();
        // A failure, recorded on the forum by its key, holds it for an hour.
        hooks::record(&db, project, Alert::Discourse, first.1, &Err(anyhow::anyhow!("signed out"))).await;
        assert!(next_due(&db, project, "discourse", first.1).await.unwrap().is_none());
        assert!(next_due(&db, project, "discourse", second.1).await.unwrap().is_none());
        hooks::record(&db, project, Alert::Discourse, first.1, &Ok(())).await;
        // 5 replies a day per forum, and 10 on Lemmy.
        let posted = "insert into social_posts (project_id, platform, body, status, account, posted_at) select $1, $2, 'Hi', 'posted', $3, now() from generate_series(1, $4)";
        sqlx::query(posted).bind(project).bind("discourse").bind(first.1).bind(4).execute(&db).await.unwrap();
        assert!(next_due(&db, project, "discourse", first.1).await.unwrap().is_some());
        sqlx::query(posted).bind(project).bind("discourse").bind(first.1).bind(1).execute(&db).await.unwrap();
        assert!(next_due(&db, project, "discourse", first.1).await.unwrap().is_none());
        approved(&db, project, "lemmy").await;
        sqlx::query(posted).bind(project).bind("lemmy").bind("ana@lemmy.world").bind(9).execute(&db).await.unwrap();
        assert!(next_due(&db, project, "lemmy", "ana@lemmy.world").await.unwrap().is_some());
        sqlx::query(posted).bind(project).bind("lemmy").bind("ana@lemmy.world").bind(1).execute(&db).await.unwrap();
        assert!(next_due(&db, project, "lemmy", "ana@lemmy.world").await.unwrap().is_none());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn nostr_stops_at_fifteen_tumblr_at_ten_and_hashnode_counts_articles_and_comments_apart(db: PgPool) {
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        sqlx::query(
            "insert into project_settings (project_id, nostr_key_enc, nostr_npub, tumblr_refresh_enc, tumblr_username, hashnode_token_enc, hashnode_username)
             values ($1, 'x', 'npub1acme', 'x', 'acme', 'x', 'acme')",
        )
        .bind(project)
        .execute(&db)
        .await
        .unwrap();
        let a = accounts(&db, project).await.unwrap().unwrap();
        assert_eq!(a.connected(), ["nostr", "tumblr", "hashnode"]);
        let posted = "insert into social_posts (project_id, platform, body, status, account, posted_at, in_reply_to)
                      select $1, $2, 'Hi', 'posted', $3, now(), case when $5 then 'https://bo.dev/' || n end from generate_series(1, $4) n";
        for (platform, account, cap) in [("nostr", "npub1acme", 15), ("tumblr", "acme", 10)] {
            approved(&db, project, platform).await;
            sqlx::query(posted).bind(project).bind(platform).bind(account).bind(cap - 1).bind(false).execute(&db).await.unwrap();
            assert!(next_due(&db, project, platform, account).await.unwrap().is_some(), "{platform}");
            sqlx::query(posted).bind(project).bind(platform).bind(account).bind(1).bind(false).execute(&db).await.unwrap();
            assert!(next_due(&db, project, platform, account).await.unwrap().is_none(), "{platform}");
        }

        // Two articles a day hold the next article, while comments still go out until ten.
        let article = approved(&db, project, "hashnode").await;
        let comment: Uuid = sqlx::query_scalar(
            "insert into social_posts (project_id, platform, body, status, in_reply_to) values ($1, 'hashnode', 'Hi', 'approved', 'https://bo.dev/x?hashnode=1')
             returning id",
        )
        .bind(project)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(next_due(&db, project, "hashnode", "acme").await.unwrap().map(|p| p.id), Some(article));
        sqlx::query(posted).bind(project).bind("hashnode").bind("acme").bind(2).bind(false).execute(&db).await.unwrap();
        assert_eq!(next_due(&db, project, "hashnode", "acme").await.unwrap().map(|p| p.id), Some(comment));
        sqlx::query(posted).bind(project).bind("hashnode").bind("acme").bind(10).bind(true).execute(&db).await.unwrap();
        assert!(next_due(&db, project, "hashnode", "acme").await.unwrap().is_none());
        sqlx::query("delete from social_posts where platform = 'hashnode' and status = 'posted' and in_reply_to is null").execute(&db).await.unwrap();
        assert_eq!(next_due(&db, project, "hashnode", "acme").await.unwrap().map(|p| p.id), Some(article));
        // A Tumblr failure is recorded on the account by its username.
        hooks::record(&db, project, Alert::Tumblr, "acme", &Err(anyhow::anyhow!("signed out"))).await;
        let failed: Option<String> = sqlx::query_scalar("select tumblr_error from project_settings where project_id = $1").bind(project).fetch_one(&db).await.unwrap();
        assert_eq!(failed.as_deref(), Some("signed out"));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_bots_stop_at_ten_a_day_and_wait_out_a_429(db: PgPool) {
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        sqlx::query(
            "insert into project_settings (project_id, discord_token_enc, discord_bot_id, telegram_token_enc, telegram_bot_id) values ($1, 'x', '900', 'x', '777')",
        )
        .bind(project)
        .execute(&db)
        .await
        .unwrap();
        let a = accounts(&db, project).await.unwrap().unwrap();
        assert_eq!(a.connected(), ["discord", "telegram"]);
        let posted = "insert into social_posts (project_id, platform, body, status, account, posted_at) select $1, $2, 'Hi', 'posted', $3, now() from generate_series(1, $4)";
        for (platform, bot) in [("discord", "900"), ("telegram", "777")] {
            approved(&db, project, platform).await;
            sqlx::query(posted).bind(project).bind(platform).bind(bot).bind(9).execute(&db).await.unwrap();
            assert!(next_due(&db, project, platform, bot).await.unwrap().is_some(), "{platform}");
            crate::social_channels::hold(&db, project, platform, 60.0).await;
            assert!(next_due(&db, project, platform, bot).await.unwrap().is_none(), "{platform}");
            sqlx::query(&format!("update project_settings set {platform}_held_until = null where project_id = $1")).bind(project).execute(&db).await.unwrap();
            sqlx::query(posted).bind(project).bind(platform).bind(bot).bind(1).execute(&db).await.unwrap();
            assert!(next_due(&db, project, platform, bot).await.unwrap().is_none(), "{platform}");
        }
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
        assert!(fits("stackexchange", true, &"a".repeat(30), 500) && !fits("stackexchange", true, &"a".repeat(29), 500));
        assert!(!fits("stackexchange", true, &"a".repeat(30_001), 500));
        assert!(!fits("reddit", true, "", 500));
        assert!(fits("discourse", true, &"a".repeat(20), 500) && !fits("discourse", true, &"a".repeat(19), 500));
        assert!(fits("lemmy", false, &"a".repeat(10_000), 500) && !fits("lemmy", true, &"a".repeat(10_001), 500));
        assert!(fits("nostr", true, &"a".repeat(5_000), 500) && !fits("nostr", false, &"a".repeat(5_001), 500));
        assert!(fits("tumblr", true, &"a".repeat(4_096), 500) && !fits("tumblr", false, &"a".repeat(4_097), 500));
        assert!(fits("discord", true, &"a".repeat(2_000), 500) && !fits("discord", false, &"a".repeat(2_001), 500));
        assert!(fits("telegram", true, &"a".repeat(4_096), 500) && !fits("telegram", false, &"a".repeat(4_097), 500));
    }
}
