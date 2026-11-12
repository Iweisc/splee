//! Ad audiences on LinkedIn, Google Ads and Meta, so a team can show ads to the people it's emailing. A project signs in to
//! each platform on the integrations page (routes/ads.rs) and picks an ad account. Each campaign can then have an audience
//! there, "Splee: <campaign>", holding its leads from the first email on, and the project can have one of its hot leads from
//! every campaign. The worker here syncs each audience every hour.
//!
//! Only SHA-256 hashes of normalized addresses, and for Google and Meta phone numbers, ever leave Splee. ad_audience_members
//! keeps the hashes of everyone who may be in each audience, written before they're sent, so a sync only sends the difference:
//! leads who unsubscribed, bounced, went on the do-not-contact list or left the campaign come out, even from an upload that
//! partly failed, and none of them is ever put in. Syncs and account changes on one connection take turns (`lock`).
//!
//! LinkedIn and Google hand out refresh tokens, renewed here like crm::signin's. Meta's long-lived token has none, so once Meta
//! turns it down the project signs in again. A platform that asks Splee to slow down is tried again after a short wait, and
//! when it keeps asking, the connection waits for as long as it said, or 15 minutes.

pub mod google;
pub mod linkedin;
pub mod meta;

#[cfg(test)]
pub mod fake;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use chrono::Utc;
use reqwest::header::HeaderMap;
use reqwest::{RequestBuilder, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{ConnectOptions, FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::config::{Config, OAuthApp};
use crate::crypto::{self, sha256_hex};
use crate::mail::oauth::{TokenError, pkce_challenge, token_request};
use crate::models::Ts;
use crate::state::AppState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    LinkedIn,
    Google,
    Meta,
}

pub const PLATFORMS: [Platform; 3] = [Platform::LinkedIn, Platform::Google, Platform::Meta];

impl Platform {
    pub fn parse(name: &str) -> Option<Self> {
        PLATFORMS.into_iter().find(|p| p.name() == name)
    }

    /// As stored in ad_connections.platform.
    pub fn name(self) -> &'static str {
        match self {
            Platform::LinkedIn => "linkedin",
            Platform::Google => "google",
            Platform::Meta => "meta",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Platform::LinkedIn => "LinkedIn",
            Platform::Google => "Google Ads",
            Platform::Meta => "Meta",
        }
    }

    /// The most people one request adds or removes: LinkedIn's streaming limit, and what Google and Meta advise per request.
    pub fn batch(self) -> usize {
        match self {
            Platform::LinkedIn => 5_000,
            Platform::Google | Platform::Meta => 10_000,
        }
    }

    /// Google Ads signs in with the mailbox sign-in's Google app, and only works with a developer token. Meta uses the app
    /// Facebook Pages sign in with.
    pub fn app(self, cfg: &Config) -> Option<&OAuthApp> {
        match self {
            Platform::LinkedIn => cfg.linkedin_ads.as_ref(),
            Platform::Google => cfg.google.as_ref().filter(|_| cfg.google_ads_developer_token.is_some()),
            Platform::Meta => cfg.meta.as_ref(),
        }
    }

    /// Only what managing audiences needs. Meta's Page sign-in never asks for ads_management.
    fn scope(self) -> &'static str {
        match self {
            Platform::LinkedIn => linkedin::SCOPE,
            Platform::Google => google::SCOPE,
            Platform::Meta => meta::SCOPE,
        }
    }
}

/// The platforms this Splee has an app for, which the integrations page offers.
pub fn available(cfg: &Config) -> Vec<&'static str> {
    PLATFORMS.into_iter().filter(|p| p.app(cfg).is_some()).map(Platform::name).collect()
}

pub fn not_set_up(platform: Platform) -> String {
    let vars = match platform {
        Platform::LinkedIn => "LINKEDIN_ADS_CLIENT_ID and LINKEDIN_ADS_CLIENT_SECRET",
        Platform::Google => "GOOGLE_CLIENT_ID, GOOGLE_CLIENT_SECRET and GOOGLE_ADS_DEVELOPER_TOKEN",
        Platform::Meta => "META_APP_ID and META_APP_SECRET",
    };
    format!("{} audiences aren't set up on this Splee. Set {vars} on the server.", platform.label())
}

pub fn redirect_uri(cfg: &Config) -> String {
    format!("{}/api/ads/oauth/callback", cfg.public_url.trim_end_matches('/'))
}

/// The platform's sign-in page. Google needs PKCE, and only hands out a refresh token again with prompt=consent.
pub fn authorize_url(cfg: &Config, platform: Platform, state: &str, verifier: &str) -> Option<String> {
    let app = platform.app(cfg)?;
    let mut url = Url::parse(&app.authorize_url).ok()?;
    let mut query = url.query_pairs_mut();
    query.append_pair("client_id", &app.client_id).append_pair("response_type", "code").append_pair("redirect_uri", &redirect_uri(cfg)).append_pair("state", state);
    query.append_pair("scope", platform.scope());
    if platform == Platform::Google {
        query.append_pair("access_type", "offline").append_pair("prompt", "consent");
        query.append_pair("code_challenge", &pkce_challenge(verifier)).append_pair("code_challenge_method", "S256");
    }
    drop(query);
    Some(url.into())
}

/// An address as every platform wants it before hashing: trimmed and lowercased. None for something that isn't one.
pub fn normalized_email(email: &str) -> Option<String> {
    let email = email.trim().to_lowercase();
    let (user, domain) = email.split_once('@')?;
    (!user.is_empty() && domain.contains('.') && !email.chars().any(char::is_whitespace)).then_some(email)
}

/// A number in E.164, like +447700900123, which is how leads' numbers are stored. Spaces and dashes are dropped, and anything
/// without a country code is left out, since a platform can't match it.
pub fn e164(phone: &str) -> Option<String> {
    let kept: String = phone.chars().filter(|c| !matches!(c, ' ' | '-' | '(' | ')' | '.')).collect();
    let digits = kept.strip_prefix('+')?;
    let ok = (8..=15).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) && !digits.starts_with('0');
    ok.then(|| format!("+{digits}"))
}

/// A lead as an audience gets it: the hashes the platform matches on. Google hashes the number in E.164 and Meta its digits
/// with the country code and no plus. LinkedIn only takes addresses.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct Member {
    pub lead_id: Uuid,
    pub email_hash: String,
    pub phone_hash: String,
}

pub fn member(platform: Platform, lead_id: Uuid, email: &str, phone: &str) -> Option<Member> {
    let email = normalized_email(email)?;
    let phone = match platform {
        Platform::LinkedIn => None,
        Platform::Google => e164(phone),
        Platform::Meta => e164(phone).map(|p| p[1..].to_string()),
    };
    Some(Member { lead_id, email_hash: sha256_hex(&email), phone_hash: phone.map(|p| sha256_hex(&p)).unwrap_or_default() })
}

/// A row of ad_audience_members: hashes that were sent, and whether the platform confirmed it holds them.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct Sent {
    #[sqlx(flatten)]
    pub member: Member,
    pub confirmed: bool,
}

/// What a sync sends: who to take out, then who to put in. Everyone sent who isn't wanted comes out, confirmed or not, and
/// everyone wanted goes in until the platform confirms it. Both are harmless to repeat on every platform. A lead whose
/// address or number changed is in both.
pub fn diff(sent: &[Sent], wanted: &[Member]) -> (Vec<Member>, Vec<Member>) {
    let had: HashMap<Uuid, &Sent> = sent.iter().map(|s| (s.member.lead_id, s)).collect();
    let want: HashMap<Uuid, &Member> = wanted.iter().map(|m| (m.lead_id, m)).collect();
    let remove = sent.iter().filter(|s| want.get(&s.member.lead_id) != Some(&&s.member)).map(|s| s.member.clone()).collect();
    let add = wanted.iter().filter(|m| !had.get(&m.lead_id).is_some_and(|s| s.confirmed && &s.member == *m)).cloned().collect();
    (remove, add)
}

/// The leads an audience should hold: a campaign's from the first email on, or the project's hot and booked leads. Either way
/// only those with an address, and none whose address, domain or number is on the do-not-contact list. Unsubscribed, bounced
/// and do-not-contact leads have their own statuses, which neither list has.
const WANTED: &str = "select l.id, l.email::text, l.phone from leads l join projects p on p.id = l.project_id
     where l.project_id = $1 and l.email is not null
       and (($2::uuid is null and l.status in ('hot', 'meeting_booked'))
         or (l.segment_id = $2 and l.status in ('contacted', 'replied', 'hot', 'meeting_booked', 'not_interested')))
       and not exists (select 1 from suppressions s where s.org_id = p.org_id
                         and s.value in (lower(l.email::text), lower(split_part(l.email::text, '@', 2)), nullif(l.phone, '')))";

pub async fn wanted(db: &PgPool, platform: Platform, project_id: Uuid, segment_id: Option<Uuid>) -> sqlx::Result<Vec<Member>> {
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(WANTED).bind(project_id).bind(segment_id).fetch_all(db).await?;
    Ok(rows.into_iter().filter_map(|(id, email, phone)| member(platform, id, &email, &phone)).collect())
}

/// One of the ad accounts a sign-in can use, as ad_connections.ad_accounts lists them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdAccount {
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub enum AdError {
    /// The platform turned the token down.
    Unauthorized,
    /// The platform asked Splee to slow down, for this many seconds when it said.
    Limited(Option<u64>),
    /// The audience isn't there anymore, deleted in the platform's own tools.
    Gone,
    /// Anything else, as the page shows it.
    Failed(String),
}

impl AdError {
    pub fn message(&self, platform: Platform) -> String {
        let label = platform.label();
        match self {
            AdError::Unauthorized => signed_out(platform),
            AdError::Limited(_) => format!("{label} asked Splee to slow down, so audiences there sync again in a while."),
            AdError::Gone => format!("The audience was deleted in {label}, so Splee makes it again."),
            AdError::Failed(m) => m.clone(),
        }
    }
}

impl From<sqlx::Error> for AdError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error = %e, "ad audience sync failed");
        AdError::Failed("Something went wrong on Splee's side. It tries again within the hour.".into())
    }
}

fn signed_out(platform: Platform) -> String {
    format!("{0} signed Splee out, so audiences there stopped syncing. Reconnect {0}.", platform.label())
}

/// How many times a request is tried when the platform says it's busy, the unit Retry-After counts in, and the longest wait.
const TRIES: u32 = 3;
const SECOND: Duration = if cfg!(test) { Duration::from_millis(10) } else { Duration::from_secs(1) };
const MAX_WAIT_SECS: u64 = 30;
/// How long a connection waits when the platform kept asking and didn't say for how long.
const HOLD_SECS: u64 = 15 * 60;

/// What a platform answered, once it wasn't an error.
pub struct Answer {
    pub headers: HeaderMap,
    pub body: Value,
}

/// How a platform's answer reads, from each platform's module.
enum Verdict {
    Ok,
    Unauthorized,
    Limited,
    Gone,
    Failed(String),
}

/// Talks to one platform with one access token.
pub struct Client {
    platform: Platform,
    http: reqwest::Client,
    base: String,
    token: String,
    developer_token: String,
    /// Set once Meta's usage headers say a limit is nearly used up, so the next call waits instead of being turned down.
    hold: Mutex<Option<u64>>,
}

impl Client {
    pub fn new(cfg: &Config, platform: Platform, token: &str) -> Result<Self, AdError> {
        let fake = cfg.ads_api_url.trim_end_matches('/');
        let base = match platform {
            Platform::Meta => cfg.graph_api_url.trim_end_matches('/').to_string(),
            p if !fake.is_empty() => format!("{fake}/{}", p.name()),
            Platform::LinkedIn => linkedin::API.into(),
            Platform::Google => google::API.into(),
        };
        let http = reqwest::Client::builder().user_agent("Splee").timeout(Duration::from_secs(60)).build().map_err(|e| AdError::Failed(e.to_string()))?;
        let developer_token = cfg.google_ads_developer_token.clone().unwrap_or_default();
        Ok(Self { platform, http, base, token: token.into(), developer_token, hold: Mutex::new(None) })
    }

    /// Sends the request and reads the answer, waiting and trying again when the platform says it's busy.
    async fn send(&self, request: RequestBuilder) -> Result<Answer, AdError> {
        if let Some(secs) = *self.hold.lock().unwrap() {
            return Err(AdError::Limited(Some(secs)));
        }
        let label = self.platform.label();
        for attempt in 0..TRIES {
            let res = request.try_clone().expect("bodies are bytes").send().await;
            let res = res.map_err(|e| AdError::Failed(format!("Couldn't reach {label}: {}", e.without_url())))?;
            let status = res.status();
            let headers = res.headers().clone();
            let retry_after = headers.get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
            let body: Value = res.json().await.unwrap_or_default();
            let verdict = match self.platform {
                Platform::LinkedIn => linkedin::verdict(status, &body),
                Platform::Google => google::verdict(status, &body),
                Platform::Meta => meta::verdict(status, &body),
            };
            let busy = matches!(verdict, Verdict::Limited) || status == StatusCode::SERVICE_UNAVAILABLE;
            if busy && attempt + 1 < TRIES {
                tokio::time::sleep(SECOND * retry_after.unwrap_or(2 << attempt).min(MAX_WAIT_SECS) as u32).await;
                continue;
            }
            if self.platform == Platform::Meta {
                let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
                if let Some(hold) = crate::meta::usage(header("x-app-usage").as_deref(), header("x-business-use-case-usage").as_deref()) {
                    *self.hold.lock().unwrap() = Some(hold.secs as u64);
                }
            }
            return match verdict {
                Verdict::Ok => Ok(Answer { headers, body }),
                Verdict::Unauthorized => Err(AdError::Unauthorized),
                Verdict::Limited => Err(AdError::Limited(retry_after)),
                Verdict::Gone => Err(AdError::Gone),
                Verdict::Failed(m) => Err(AdError::Failed(m)),
            };
        }
        unreachable!("the last try returns")
    }

    pub async fn ad_accounts(&self) -> Result<Vec<AdAccount>, AdError> {
        match self.platform {
            Platform::LinkedIn => linkedin::ad_accounts(self).await,
            Platform::Google => google::ad_accounts(self).await,
            Platform::Meta => meta::ad_accounts(self).await,
        }
    }

    /// Makes an empty audience in the ad account and returns the platform's id for it.
    pub async fn create(&self, account: &str, name: &str) -> Result<String, AdError> {
        match self.platform {
            Platform::LinkedIn => linkedin::create(self, account, name).await,
            Platform::Google => google::create(self, account, name).await,
            Platform::Meta => meta::create(self, account, name).await,
        }
    }

    /// Adds or removes up to Platform::batch people on LinkedIn or Meta, which say straight away whether they took them.
    pub async fn change(&self, audience: &str, members: &[Member], add: bool) -> Result<(), AdError> {
        match self.platform {
            Platform::LinkedIn => linkedin::users(self, audience, members, add).await,
            Platform::Meta => meta::users(self, audience, members, add).await,
            Platform::Google => unreachable!("Google's changes go in jobs, through google::submit"),
        }
    }

    /// How many people the platform says the audience reaches. None while it's still matching, or too small to say.
    pub async fn size(&self, account: &str, audience: &str) -> Result<Option<i64>, AdError> {
        match self.platform {
            Platform::LinkedIn => linkedin::size(self, audience).await,
            Platform::Google => google::size(self, account, audience).await,
            Platform::Meta => meta::size(self, audience).await,
        }
    }

    /// One that's gone already counts as deleted.
    pub async fn delete(&self, account: &str, audience: &str) -> Result<(), AdError> {
        let deleted = match self.platform {
            Platform::LinkedIn => linkedin::delete(self, audience).await,
            Platform::Google => google::delete(self, account, audience).await,
            Platform::Meta => meta::delete(self, audience).await,
        };
        match deleted {
            Err(AdError::Gone) => Ok(()),
            other => other,
        }
    }
}

/// An id that goes into a platform's path, which only ever has digits.
fn digits(id: &str) -> Result<&str, AdError> {
    let ok = (1..=30).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit());
    ok.then_some(id).ok_or_else(|| AdError::Failed(format!("{id:?} isn't an id the platform gave")))
}

#[derive(Clone, Debug, FromRow)]
pub struct Connection {
    pub id: Uuid,
    pub project_id: Uuid,
    pub platform: String,
    pub ad_account_id: String,
    pub needs_reconnect: bool,
}

impl Connection {
    pub fn platform(&self) -> Platform {
        Platform::parse(&self.platform).expect("the column's check only allows known platforms")
    }

    pub async fn load(db: &PgPool, id: Uuid) -> sqlx::Result<Connection> {
        sqlx::query_as("select id, project_id, platform, ad_account_id, needs_reconnect from ad_connections where id = $1").bind(id).fetch_one(db).await
    }

    pub async fn find(db: &PgPool, project_id: Uuid, platform: Platform) -> sqlx::Result<Option<Connection>> {
        sqlx::query_as("select id, project_id, platform, ad_account_id, needs_reconnect from ad_connections where project_id = $1 and platform = $2")
            .bind(project_id)
            .bind(platform.name())
            .fetch_optional(db)
            .await
    }
}

/// An access token for the connection: the stored one while it has a minute left, else a new one, and whether it's new. Meta's
/// has no expiry here, since Meta says when it stops working. `rejected` is a token the platform just turned down: one another
/// sync saved since is used instead, and otherwise a new one is fetched. Under a lock on the connection, since the worker and a
/// toggle on the campaign page can ask at once.
async fn access(state: &AppState, id: Uuid, rejected: Option<&str>) -> Result<(String, bool), AdError> {
    let mut tx = state.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('ad-token:' || $1::text, 0))").bind(id).execute(&mut *tx).await?;
    let (platform, refresh_enc, access_enc, expires_at, needs_reconnect): (String, String, String, Option<Ts>, bool) =
        sqlx::query_as("select platform, refresh_enc, access_enc, expires_at, needs_reconnect from ad_connections where id = $1").bind(id).fetch_one(&mut *tx).await?;
    // Nothing sends the platform a token it turned down, so a revoked sign-in isn't tried on every sync.
    if needs_reconnect {
        return Err(AdError::Unauthorized);
    }
    let platform = Platform::parse(&platform).expect("the column's check only allows known platforms");
    let secret = &state.cfg.app_secret;
    let unreadable = |e: anyhow::Error| {
        tracing::error!(error = %e, "an ad sign-in couldn't be decrypted");
        AdError::Unauthorized
    };
    if !access_enc.is_empty() && expires_at.is_none_or(|at| at > Utc::now() + chrono::Duration::seconds(60)) {
        let access = crypto::decrypt(secret, &access_enc).map_err(unreadable)?;
        if rejected != Some(access.as_str()) {
            return Ok((access, false));
        }
    }
    // Meta's, and LinkedIn's for an app without LinkedIn's refresh tokens, only last until they run out.
    if refresh_enc.is_empty() {
        return Err(AdError::Unauthorized);
    }
    let app = platform.app(&state.cfg).ok_or_else(|| AdError::Failed(not_set_up(platform)))?;
    let refresh = crypto::decrypt(secret, &refresh_enc).map_err(unreadable)?;
    match token_request(platform.label(), app, &[("grant_type", "refresh_token"), ("refresh_token", &refresh)]).await {
        Ok(tokens) => {
            // Matched on the old refresh token, so a reconnect since keeps its own. LinkedIn may send a new one.
            sqlx::query(
                "update ad_connections set access_enc = $3, expires_at = $4, refresh_enc = coalesce($5, refresh_enc) where id = $1 and refresh_enc = $2",
            )
            .bind(id)
            .bind(&refresh_enc)
            .bind(crypto::encrypt(secret, &tokens.access_token))
            .bind(tokens.expires_at())
            .bind(tokens.refresh_token.as_deref().filter(|t| !t.is_empty()).map(|t| crypto::encrypt(secret, t)))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok((tokens.access_token, true))
        }
        Err(TokenError::Revoked(_)) => Err(AdError::Unauthorized),
        Err(TokenError::Unavailable(_, e)) => Err(AdError::Failed(format!("Couldn't reach {} to renew the sign-in: {e}", platform.label()))),
        Err(TokenError::Refused(_, e)) => Err(AdError::Failed(format!("{} refused to renew the sign-in: {e}", platform.label()))),
    }
}

/// Stops syncing until someone reconnects, and says so on the integrations page.
async fn sign_out(db: &PgPool, id: Uuid, platform: Platform) -> sqlx::Result<()> {
    tracing::warn!(connection = %id, platform = platform.name(), "an ad platform signed Splee out");
    sqlx::query("update ad_connections set needs_reconnect = true, access_enc = '', expires_at = null, error = $2, failed_at = now() where id = $1 and not needs_reconnect")
        .bind(id)
        .bind(signed_out(platform))
        .execute(db)
        .await?;
    Ok(())
}

/// Runs `f` with a client for the connection, and again with a new token when the platform turned down one that wasn't new,
/// since a token can stop working before it runs out. One turned down after that, or by Meta, signs the project out of the
/// platform until it reconnects. `f` has to be safe to run twice.
pub async fn authed<T, F, Fut>(state: &AppState, conn: &Connection, f: F) -> Result<T, AdError>
where
    F: Fn(Client) -> Fut,
    Fut: Future<Output = Result<T, AdError>>,
{
    let platform = conn.platform();
    let result = match access(state, conn.id, None).await {
        Ok((token, renewed)) => match f(Client::new(&state.cfg, platform, &token)?).await {
            Err(AdError::Unauthorized) if !renewed && platform != Platform::Meta => match access(state, conn.id, Some(&token)).await {
                Ok((token, _)) => f(Client::new(&state.cfg, platform, &token)?).await,
                Err(e) => Err(e),
            },
            other => other,
        },
        Err(e) => Err(e),
    };
    if matches!(result, Err(AdError::Unauthorized)) {
        sign_out(&state.db, conn.id, platform).await?;
    }
    result
}

#[derive(Clone, Debug, FromRow)]
pub struct Audience {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub project_id: Uuid,
    pub segment_id: Option<Uuid>,
    pub name: String,
    pub external_id: String,
}

impl Audience {
    /// None once it's turned off.
    pub async fn find(db: &PgPool, id: Uuid) -> sqlx::Result<Option<Audience>> {
        sqlx::query_as("select id, connection_id, project_id, segment_id, name, external_id from ad_audiences where id = $1").bind(id).fetch_optional(db).await
    }
}

/// A connection's turn, held until it's dropped. Syncs, account changes, turning audiences off and disconnecting each take it,
/// so a sync still running with the old account can't fill a ledger an account change just emptied. This looks odd, but it's
/// intentional: the lock is a session lock on a database connection of its own, outside the pool, so a sync that waits on a
/// platform for minutes doesn't keep one of the pool's busy, and Postgres lets go of it when that connection closes, even if
/// Splee dies.
struct Turn {
    _session: PgConnection,
    _slot: tokio::sync::SemaphorePermit<'static>,
}

/// How many of those connections can be open at once, so a burst of syncs, account changes or disconnects waits here
/// instead of using up Postgres's connections beyond the pool's 20.
const TURNS: usize = 4;
static TURN_SLOTS: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(TURNS));

/// Waits for the turn of the project's connection to the platform. Keyed by project and platform, which a sign-in knows before
/// its connection exists. Read the connection again after taking it.
async fn lock(db: &PgPool, project_id: Uuid, platform: Platform) -> sqlx::Result<Turn> {
    let slot = TURN_SLOTS.acquire().await.expect("the semaphore is never closed");
    let mut session = db.connect_options().connect().await?;
    sqlx::query("select pg_advisory_lock(hashtextextended('ad-sync:' || $1::text || ':' || $2, 0))")
        .bind(project_id)
        .bind(platform.name())
        .execute(&mut session)
        .await?;
    Ok(Turn { _session: session, _slot: slot })
}

/// The project and platform of an audience's connection, for `lock`. None once the audience is gone.
async fn owner(db: &PgPool, audience_id: Uuid) -> sqlx::Result<Option<(Uuid, Platform)>> {
    let found: Option<(Uuid, String)> =
        sqlx::query_as("select c.project_id, c.platform from ad_audiences a join ad_connections c on c.id = a.connection_id where a.id = $1")
            .bind(audience_id)
            .fetch_optional(db)
            .await?;
    Ok(found.map(|(project, platform)| (project, Platform::parse(&platform).expect("the column's check only allows known platforms"))))
}

/// The name an audience gets on the platform. The platforms cap names at a few hundred characters.
pub fn audience_name(campaign: Option<&str>) -> String {
    let name: String = campaign.map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Untitled campaign").chars().take(100).collect();
    match campaign {
        Some(_) => format!("Splee: {name}"),
        None => "Splee: Hot leads".into(),
    }
}

/// Makes the campaign's audience, or the hot leads' when `segment` is None, on the connection's platform, for the worker to
/// fill. Made here rather than by the worker, so the campaign page says straight away when the platform won't.
pub async fn enable(state: &AppState, conn: &Connection, segment: Option<(Uuid, &str)>) -> Result<(), AdError> {
    let platform = conn.platform();
    if conn.ad_account_id.is_empty() {
        return Err(AdError::Failed(format!("Pick a {} ad account on the integrations page first.", platform.label())));
    }
    let segment_id = segment.map(|(id, _)| id);
    let existing: Option<Uuid> = sqlx::query_scalar("select id from ad_audiences where connection_id = $1 and segment_id is not distinct from $2")
        .bind(conn.id)
        .bind(segment_id)
        .fetch_optional(&state.db)
        .await?;
    if existing.is_some() {
        return Ok(());
    }
    let name = audience_name(segment.map(|(_, name)| name));
    let external = authed(state, conn, |client| {
        let (account, name) = (&conn.ad_account_id, &name);
        async move { client.create(account, name).await }
    })
    .await?;
    let kind = if segment.is_some() { "campaign" } else { "hot" };
    let made = sqlx::query(
        "insert into ad_audiences (connection_id, project_id, segment_id, kind, name, external_id) values ($1, $2, $3, $4, $5, $6) on conflict do nothing",
    )
    .bind(conn.id)
    .bind(conn.project_id)
    .bind(segment_id)
    .bind(kind)
    .bind(&name)
    .bind(&external)
    .execute(&state.db)
    .await?;
    // Another toggle won the race, so the one made here would be left empty.
    if made.rows_affected() == 0 {
        forget_on_platform(state, conn, &external).await;
    }
    Ok(())
}

/// Deletes the audience on the platform where Splee still can, so nobody stays in it, then forgets it.
pub async fn disable(state: &AppState, audience_id: Uuid) -> sqlx::Result<()> {
    let Some((project_id, platform)) = owner(&state.db, audience_id).await? else { return Ok(()) };
    let _turn = lock(&state.db, project_id, platform).await?;
    let Some(audience) = Audience::find(&state.db, audience_id).await? else { return Ok(()) };
    let conn = Connection::load(&state.db, audience.connection_id).await?;
    if !audience.external_id.is_empty() {
        forget_on_platform(state, &conn, &audience.external_id).await;
    }
    sqlx::query("delete from ad_audiences where id = $1").bind(audience_id).execute(&state.db).await?;
    Ok(())
}

/// Best effort: the platform may be down or have signed Splee out, and turning an audience off works either way.
async fn forget_on_platform(state: &AppState, conn: &Connection, external: &str) {
    if conn.needs_reconnect || conn.ad_account_id.is_empty() {
        return;
    }
    let deleted = authed(state, conn, |client| {
        let account = &conn.ad_account_id;
        async move { client.delete(account, external).await }
    })
    .await;
    if let Err(e) = deleted {
        tracing::info!(error = e.message(conn.platform()), "couldn't delete an ad audience");
    }
}

/// Picks another of the sign-in's ad accounts. The audiences in the old one are deleted where Splee can, and the worker makes
/// them again in the new one, starting empty.
pub async fn pick_account(state: &AppState, project_id: Uuid, platform: Platform, account_id: &str, account_name: &str) -> sqlx::Result<()> {
    let _turn = lock(&state.db, project_id, platform).await?;
    let Some(old) = Connection::find(&state.db, project_id, platform).await? else { return Ok(()) };
    if old.ad_account_id == account_id {
        return Ok(());
    }
    let audiences: Vec<String> =
        sqlx::query_scalar("select external_id from ad_audiences where connection_id = $1 and external_id <> ''").bind(old.id).fetch_all(&state.db).await?;
    for external in &audiences {
        forget_on_platform(state, &old, external).await;
    }
    reset(&state.db, old.id, None).await?;
    sqlx::query("update ad_connections set ad_account_id = $2, ad_account_name = $3 where id = $1")
        .bind(old.id)
        .bind(account_id)
        .bind(account_name)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Forgets the platform's copy of the connection's audiences, or of one, so the next sync makes it again from nothing.
async fn reset(db: &PgPool, connection_id: Uuid, audience_id: Option<Uuid>) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    let only = "connection_id = $1 and ($2::uuid is null or id = $2)";
    sqlx::query(&format!("delete from ad_audience_members where audience_id in (select id from ad_audiences where {only})"))
        .bind(connection_id)
        .bind(audience_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(&format!("update ad_audiences set external_id = '', size = null, synced_at = null, tried_at = null, claimed_at = null, error = null where {only}"))
        .bind(connection_id)
        .bind(audience_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

/// Trades the callback's code for tokens, lists the ad accounts the sign-in can use and saves the connection. The account
/// picked before stays picked while the sign-in can still use it, and one is picked when there's only one. Errors are as the
/// integrations page shows them.
pub async fn connect(state: &AppState, org_id: Uuid, project_id: Uuid, platform: Platform, code: &str, verifier: &str) -> Result<(), String> {
    let label = platform.label();
    let app = platform.app(&state.cfg).ok_or_else(|| not_set_up(platform))?;
    let redirect = redirect_uri(&state.cfg);
    let (access, refresh, expires_at) = if platform == Platform::Meta {
        let graph = crate::meta::Graph::new(&state.cfg.graph_api_url, "").map_err(|e| e.message())?;
        (graph.exchange_code(app, code, &redirect).await.map_err(|e| e.message())?, String::new(), None)
    } else {
        let mut form = vec![("grant_type", "authorization_code"), ("code", code), ("redirect_uri", redirect.as_str())];
        if platform == Platform::Google {
            form.push(("code_verifier", verifier));
        }
        let tokens = token_request(label, app, &form).await.map_err(|e| match e {
            TokenError::Revoked(_) => format!("This sign-in expired. Connect {label} again."),
            TokenError::Unavailable(_, e) => format!("Couldn't reach {label}: {e}. Try again."),
            TokenError::Refused(_, e) => format!("{label} turned the sign-in down: {e}"),
        })?;
        let refresh = tokens.refresh_token.clone().unwrap_or_default();
        if platform == Platform::Google && refresh.is_empty() {
            return Err("Google didn't let Splee stay signed in. Connect Google Ads again.".into());
        }
        (tokens.access_token.clone(), refresh, Some(tokens.expires_at()))
    };
    let client = Client::new(&state.cfg, platform, &access).map_err(|e| e.message(platform))?;
    let accounts = client.ad_accounts().await.map_err(|e| match e {
        AdError::Unauthorized => format!("{label} didn't let Splee read your ad accounts. Connect {label} again and allow everything it asks for."),
        e => format!("{label} didn't list your ad accounts: {}", e.message(platform)),
    })?;
    if accounts.is_empty() {
        return Err(format!("This {label} sign-in can't use any ad accounts. Sign in as someone who manages one."));
    }
    let internal = |e: sqlx::Error| {
        tracing::error!(error = %e, platform = platform.name(), "couldn't save an ad sign-in");
        format!("Something went wrong saving the {label} sign-in. Try again.")
    };
    let _turn = lock(&state.db, project_id, platform).await.map_err(internal)?;
    let old = Connection::find(&state.db, project_id, platform).await.map_err(internal)?;
    let kept = old.as_ref().map(|c| c.ad_account_id.clone()).filter(|id| accounts.iter().any(|a| &a.id == id));
    let picked = kept.or_else(|| (accounts.len() == 1).then(|| accounts[0].id.clone())).unwrap_or_default();
    let name = accounts.iter().find(|a| a.id == picked).map(|a| a.name.clone()).unwrap_or_default();
    if let Some(old) = old.filter(|c| c.ad_account_id != picked && !c.ad_account_id.is_empty()) {
        reset(&state.db, old.id, None).await.map_err(internal)?;
    }
    let secret = &state.cfg.app_secret;
    sqlx::query(
        "insert into ad_connections (org_id, project_id, platform, refresh_enc, access_enc, expires_at, ad_accounts, ad_account_id, ad_account_name)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         on conflict (project_id, platform) do update set refresh_enc = excluded.refresh_enc, access_enc = excluded.access_enc,
           expires_at = excluded.expires_at, ad_accounts = excluded.ad_accounts, ad_account_id = excluded.ad_account_id,
           ad_account_name = excluded.ad_account_name, needs_reconnect = false, error = null, failed_at = null, held_until = null",
    )
    .bind(org_id)
    .bind(project_id)
    .bind(platform.name())
    .bind(if refresh.is_empty() { String::new() } else { crypto::encrypt(secret, &refresh) })
    .bind(crypto::encrypt(secret, &access))
    .bind(expires_at)
    .bind(serde_json::to_value(&accounts).expect("a list of strings serializes"))
    .bind(&picked)
    .bind(&name)
    .execute(&state.db)
    .await
    .map_err(internal)?;
    Ok(())
}

/// Deletes the project's audiences on the platform where Splee still can, then forgets the sign-in and the audiences.
pub async fn disconnect(state: &AppState, conn: &Connection) -> sqlx::Result<()> {
    let _turn = lock(&state.db, conn.project_id, conn.platform()).await?;
    // A sync or re-sign-in may have changed the account or its tokens while this waited.
    let Some(conn) = Connection::find(&state.db, conn.project_id, conn.platform()).await? else { return Ok(()) };
    let conn = &conn;
    let audiences: Vec<String> =
        sqlx::query_scalar("select external_id from ad_audiences where connection_id = $1 and external_id <> ''").bind(conn.id).fetch_all(&state.db).await?;
    for external in &audiences {
        forget_on_platform(state, conn, external).await;
    }
    sqlx::query("delete from ad_connections where id = $1").bind(conn.id).execute(&state.db).await?;
    Ok(())
}

/// Brings one audience up to date and saves how it went.
pub async fn sync(state: &AppState, audience_id: Uuid) -> sqlx::Result<()> {
    let Some((project_id, platform)) = owner(&state.db, audience_id).await? else { return Ok(()) };
    let _turn = lock(&state.db, project_id, platform).await?;
    let Some(audience) = Audience::find(&state.db, audience_id).await? else { return Ok(()) };
    let conn = Connection::load(&state.db, audience.connection_id).await?;
    let outcome = authed(state, &conn, |client| sync_with(state, &conn, &audience, client)).await;
    let error = outcome.as_ref().err().map(|e| e.message(platform));
    match &outcome {
        Err(AdError::Limited(secs)) => {
            let secs = secs.unwrap_or(HOLD_SECS).clamp(60, 6 * 3600);
            sqlx::query("update ad_connections set held_until = now() + make_interval(secs => $2) where id = $1").bind(conn.id).bind(secs as f64).execute(&state.db).await?;
        }
        Err(AdError::Gone) => {
            reset(&state.db, conn.id, Some(audience.id)).await?;
            return Ok(());
        }
        _ => {}
    }
    sqlx::query("update ad_audiences set tried_at = now(), claimed_at = null, error = $2 where id = $1").bind(audience.id).bind(error).execute(&state.db).await?;
    Ok(())
}

async fn sync_with(state: &AppState, conn: &Connection, audience: &Audience, client: Client) -> Result<(), AdError> {
    let db = &state.db;
    let platform = conn.platform();
    let account = conn.ad_account_id.as_str();
    if account.is_empty() {
        return Err(AdError::Failed(format!("Pick a {} ad account on the integrations page.", platform.label())));
    }
    // Read again, since a try before a renewed token may have made it.
    let mut external: String = sqlx::query_scalar("select external_id from ad_audiences where id = $1").bind(audience.id).fetch_one(db).await?;
    if external.is_empty() {
        external = client.create(account, &audience.name).await?;
        sqlx::query("update ad_audiences set external_id = $2 where id = $1").bind(audience.id).bind(&external).execute(db).await?;
    }
    if platform != Platform::Google || settle(db, &client, account, audience.id).await? {
        let wanted = wanted(db, platform, audience.project_id, audience.segment_id).await?;
        let sent: Vec<Sent> = sqlx::query_as("select lead_id, email_hash, phone_hash, state = 'added' as confirmed from ad_audience_members where audience_id = $1")
            .bind(audience.id)
            .fetch_all(db)
            .await?;
        let (remove, add) = diff(&sent, &wanted);
        if platform == Platform::Google {
            send_job(db, &client, account, audience.id, &external, remove, add).await?;
        } else {
            send_batches(db, &client, audience.id, &external, &remove, &add).await?;
        }
    }
    let size = client.size(account, &external).await?;
    sqlx::query("update ad_audiences set size = coalesce($2, size), synced_at = now() where id = $1").bind(audience.id).bind(size).execute(db).await?;
    Ok(())
}

fn lead_ids(members: &[Member]) -> Vec<Uuid> {
    members.iter().map(|m| m.lead_id).collect()
}

/// Saves the leads as sent but not confirmed, before they're sent, so any the platform takes before an error still come out
/// once they're no longer wanted.
async fn mark_adding(db: &PgPool, audience_id: Uuid, members: &[Member]) -> sqlx::Result<()> {
    let emails: Vec<&str> = members.iter().map(|m| m.email_hash.as_str()).collect();
    let phones: Vec<&str> = members.iter().map(|m| m.phone_hash.as_str()).collect();
    sqlx::query(
        "insert into ad_audience_members (audience_id, lead_id, email_hash, phone_hash)
         select $1, * from unnest($2::uuid[], $3::text[], $4::text[])
         on conflict (audience_id, lead_id) do update set email_hash = excluded.email_hash, phone_hash = excluded.phone_hash, state = 'adding', job = '',
           added_at = now()",
    )
    .bind(audience_id)
    .bind(lead_ids(members))
    .bind(&emails)
    .bind(&phones)
    .execute(db)
    .await?;
    Ok(())
}

/// LinkedIn and Meta answer for each batch, so a removal is forgotten and an addition confirmed as soon as it's taken.
async fn send_batches(db: &PgPool, client: &Client, audience_id: Uuid, external: &str, remove: &[Member], add: &[Member]) -> Result<(), AdError> {
    let size = client.platform.batch();
    // Out first, so a lead who opted out is never in an audience longer than a failed sync makes them.
    for batch in remove.chunks(size) {
        client.change(external, batch, false).await?;
        sqlx::query("delete from ad_audience_members where audience_id = $1 and lead_id = any($2)").bind(audience_id).bind(lead_ids(batch)).execute(db).await?;
    }
    for batch in add.chunks(size) {
        mark_adding(db, audience_id, batch).await?;
        client.change(external, batch, true).await?;
        sqlx::query("update ad_audience_members set state = 'added' where audience_id = $1 and lead_id = any($2)").bind(audience_id).bind(lead_ids(batch)).execute(db).await?;
    }
    Ok(())
}

/// Google applies a job a while after it's run, and turns down a second job on a list while one runs. So a sync sends one
/// job, of the removals while there are any, else of the additions, and leaves it to a later sync to confirm (`settle`).
async fn send_job(db: &PgPool, client: &Client, account: &str, audience_id: Uuid, list: &str, remove: Vec<Member>, add: Vec<Member>) -> Result<(), AdError> {
    let (members, adding) = if remove.is_empty() { (add, true) } else { (remove, false) };
    if members.is_empty() {
        return Ok(());
    }
    if adding {
        mark_adding(db, audience_id, &members).await?;
    }
    let job = google::submit(client, account, list, &members, adding).await?;
    sqlx::query("update ad_audience_members set job = $3, state = case when $4 then 'adding' else 'removing' end where audience_id = $1 and lead_id = any($2)")
        .bind(audience_id)
        .bind(lead_ids(&members))
        .bind(&job)
        .bind(adding)
        .execute(db)
        .await?;
    Ok(())
}

/// Reads how the audience's Google job went. Done, its additions are confirmed and its removals forgotten. Failed, they're
/// left for the next sync to send again, and the page says so. Returns whether this sync can send more, which it can't while
/// the job runs.
async fn settle(db: &PgPool, client: &Client, account: &str, audience_id: Uuid) -> Result<bool, AdError> {
    let job: Option<String> = sqlx::query_scalar("select job from ad_audience_members where audience_id = $1 and job <> '' limit 1").bind(audience_id).fetch_optional(db).await?;
    let Some(job) = job else { return Ok(true) };
    match google::job(client, account, &job).await? {
        google::Job::Running => Ok(false),
        google::Job::Done => {
            let mut tx = db.begin().await?;
            sqlx::query("delete from ad_audience_members where audience_id = $1 and job = $2 and state = 'removing'").bind(audience_id).bind(&job).execute(&mut *tx).await?;
            sqlx::query("update ad_audience_members set state = 'added', job = '' where audience_id = $1 and job = $2").bind(audience_id).bind(&job).execute(&mut *tx).await?;
            tx.commit().await?;
            Ok(true)
        }
        google::Job::Failed(reason) => {
            sqlx::query("update ad_audience_members set job = '' where audience_id = $1 and job = $2").bind(audience_id).bind(&job).execute(db).await?;
            Err(AdError::Failed(format!("Google Ads couldn't apply Splee's last change to this list ({reason}). Splee sends it again within the hour.")))
        }
    }
}

/// Syncs every audience that's due, oldest first: each hourly, and a new one on the next tick. Connections that need a
/// reconnect, have no ad account or were asked to slow down wait. Returns how many it synced.
pub async fn tick(state: &AppState) -> sqlx::Result<usize> {
    let mut synced = 0;
    loop {
        let claimed: Option<Uuid> = sqlx::query_scalar(
            "update ad_audiences set claimed_at = now() where id = (
               select a.id from ad_audiences a join ad_connections c on c.id = a.connection_id
               where not c.needs_reconnect and c.ad_account_id <> '' and (c.held_until is null or c.held_until <= now())
                 and (a.tried_at is null or a.tried_at <= now() - interval '1 hour') and (a.claimed_at is null or a.claimed_at <= now() - interval '30 minutes')
               order by a.tried_at nulls first, a.created_at limit 1 for update of a skip locked)
             returning id",
        )
        .fetch_optional(&state.db)
        .await?;
        let Some(id) = claimed else { return Ok(synced) };
        sync(state, id).await?;
        synced += 1;
    }
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = tick(&state).await {
                tracing::error!(error = ?e, "ad audiences: tick failed");
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}
