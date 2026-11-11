//! Salesforce, Zoho CRM and Dynamics 365 Sales, which a project connects by signing in on the integrations page
//! (routes/crm_oauth.rs) with the authorization code flow, through the operator's Salesforce connected app, Zoho client or
//! Microsoft app. {crm}_token_enc holds a Connection, which only changes on reconnect, so alerts match on it like any CRM's.
//! The refresh token and the access token, renewed here when it has under a minute left or the CRM turns it down, have their
//! own columns. The CRM's module gets a Session as its token.
//!
//! Docs:
//! https://help.salesforce.com/s/articleView?id=xcloud.remoteaccess_oauth_web_server_flow.htm
//! https://help.salesforce.com/s/articleView?id=xcloud.remoteaccess_oauth_refresh_token_flow.htm
//! https://help.salesforce.com/s/articleView?id=xcloud.remoteaccess_revoke_token.htm
//! https://www.zoho.com/crm/developer/docs/api/v8/auth-request.html
//! https://www.zoho.com/crm/developer/docs/api/v8/access-refresh.html
//! https://www.zoho.com/crm/developer/docs/api/v8/refresh.html
//! https://www.zoho.com/crm/developer/docs/api/v8/multi-dc.html
//! https://www.zoho.com/accounts/protocol/oauth/revoke-refresh-token.html
//! https://learn.microsoft.com/en-us/power-apps/developer/data-platform/webapi/authenticate-web-api
//! https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-auth-code-flow

use anyhow::anyhow;
use chrono::Utc;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::{Crm, Unauthorized, client, salesforce, send, site_root};
use crate::config::{Config, OAuthApp};
use crate::crypto;
use crate::mail::oauth::{TokenError, pkce_challenge, token_request};
use crate::models::Ts;
use crate::state::AppState;

pub const SALESFORCE_AUTHORIZE: &str = "https://login.salesforce.com/services/oauth2/authorize";
pub const SALESFORCE_TOKEN: &str = "https://login.salesforce.com/services/oauth2/token";
/// refresh_token is what gets a refresh token back.
const SALESFORCE_SCOPE: &str = "api refresh_token";
/// What crm_oauth_states.site holds for a Salesforce sandbox, which signs in at test.salesforce.com.
pub const SANDBOX: &str = "sandbox";
/// Sign-in starts at the US accounts server for every data centre, and Zoho sends the browser on to the account's own.
pub const ZOHO_AUTHORIZE: &str = "https://accounts.zoho.com/oauth/v2/auth";
pub const ZOHO_TOKEN: &str = "https://accounts.zoho.com/oauth/v2/token";
/// Zoho's data centres' accounts servers. The callback names the account's, and its code and refresh token only work there.
const ZOHO_ACCOUNTS: [&str; 7] = [
    "https://accounts.zoho.com",
    "https://accounts.zoho.eu",
    "https://accounts.zoho.in",
    "https://accounts.zoho.com.au",
    "https://accounts.zoho.jp",
    "https://accounts.zohocloud.ca",
    "https://accounts.zoho.sa",
];
const ZOHO_SCOPE: &str = "ZohoCRM.modules.leads.ALL,ZohoCRM.modules.notes.CREATE";
/// Where Zoho's API is when the token answer doesn't say.
const ZOHO_API: &str = "https://www.zohoapis.com";

/// Where the account is, saved encrypted in {crm}_token_enc.
#[derive(Serialize, Deserialize)]
struct Connection {
    /// Salesforce's instance URL, Zoho's API domain or the Dynamics org, without a trailing slash.
    site: String,
    /// Where its tokens are renewed: the Salesforce login host it signed in at, Zoho's accounts server for its data centre, or
    /// Microsoft's.
    token_url: String,
}

/// What a CRM's module gets as its token: where the API is, an access token and, for Salesforce, the status booked leads get.
/// A line each, like the token of a CRM with Crm::account.
pub struct Session {
    pub site: String,
    pub access: String,
    pub booked_status: String,
}

impl Session {
    fn token(&self) -> String {
        format!("{}\n{}\n{}", self.site, self.access, self.booked_status)
    }

    pub fn parse(token: &str) -> anyhow::Result<Session> {
        let mut lines = token.splitn(3, '\n');
        let (Some(site), Some(access)) = (lines.next(), lines.next()) else { anyhow::bail!("the saved sign-in is unreadable, so reconnect it") };
        Ok(Session { site: site.into(), access: access.into(), booked_status: lines.next().unwrap_or_default().into() })
    }
}

pub fn app(cfg: &Config, crm: Crm) -> Option<&OAuthApp> {
    match crm {
        Crm::Salesforce => cfg.salesforce.as_ref(),
        Crm::Zoho => cfg.zoho.as_ref(),
        Crm::Dynamics => cfg.microsoft.as_ref(),
        _ => None,
    }
}

/// The CRMs this Splee has an app for, which the integrations page offers.
pub fn configured(cfg: &Config) -> Vec<&'static str> {
    Crm::ALL.iter().copied().filter(|crm| app(cfg, *crm).is_some()).map(Crm::name).collect()
}

pub fn not_set_up(crm: Crm) -> String {
    let var = match crm {
        Crm::Salesforce => "SALESFORCE",
        Crm::Zoho => "ZOHO",
        _ => "MICROSOFT",
    };
    format!("{} sign-in isn't set up on this Splee. Set {var}_CLIENT_ID and {var}_CLIENT_SECRET on the server.", crm.label())
}

fn signed_out(crm: Crm) -> String {
    format!("{0} signed Splee out, so new leads aren't reaching it. Reconnect {0}", crm.label())
}

pub fn redirect_uri(cfg: &Config) -> String {
    format!("{}/api/crm/oauth/callback", cfg.public_url.trim_end_matches('/'))
}

/// A sandbox org signs in at test.salesforce.com instead. Tests point the app at a fake, which serves both.
fn sandboxed(url: &str) -> String {
    url.replacen("https://login.salesforce.com/", "https://test.salesforce.com/", 1)
}

fn dynamics_scope(site: &str) -> String {
    format!("{site}/user_impersonation offline_access")
}

/// A Dynamics 365 org's address, like https://acme.crm4.dynamics.com, from whatever page of it was pasted. Only Dynamics'
/// own hosts are taken, since Splee sends the org's token there.
pub fn org_root(cfg: &Config, input: &str) -> Option<String> {
    let root = site_root(input)?;
    let site = root.as_str().trim_end_matches('/').to_string();
    if !cfg.crm_api_url.is_empty() && site == cfg.crm_api_url {
        return Some(site);
    }
    let host = root.host_str()?;
    let region = host.split_once('.').filter(|(org, _)| !org.is_empty()).and_then(|(_, rest)| rest.strip_suffix(".dynamics.com")?.strip_prefix("crm"));
    let dynamics = region.is_some_and(|r| r.chars().all(|c| c.is_ascii_digit()));
    (root.scheme() == "https" && root.port().is_none() && dynamics).then_some(site)
}

/// The accounts server Zoho's callback named, if it's one of Zoho's or the app's own, or the app's own when it named none.
fn zoho_accounts(app: &OAuthApp, named: Option<&str>) -> Option<String> {
    let own = Url::parse(&app.token_url).ok()?.origin().ascii_serialization();
    match named.map(|n| n.trim().trim_end_matches('/')) {
        None | Some("") => Some(own),
        Some(n) if n == own || ZOHO_ACCOUNTS.contains(&n) => Some(n.to_string()),
        _ => None,
    }
}

/// The CRM's sign-in page. `site` is what crm_oauth_states.site keeps: the Dynamics org, or SANDBOX for a Salesforce sandbox.
/// Zoho takes no PKCE, and only hands out a refresh token again for an account that already allowed the app with
/// prompt=consent.
pub fn authorize_url(cfg: &Config, crm: Crm, site: &str, state: &str, verifier: &str) -> Option<String> {
    let app = app(cfg, crm)?;
    let authorize = if crm == Crm::Salesforce && site == SANDBOX { sandboxed(&app.authorize_url) } else { app.authorize_url.clone() };
    let mut url = Url::parse(&authorize).ok()?;
    let mut query = url.query_pairs_mut();
    query.append_pair("client_id", &app.client_id).append_pair("response_type", "code").append_pair("redirect_uri", &redirect_uri(cfg)).append_pair("state", state);
    if crm == Crm::Zoho {
        query.append_pair("scope", ZOHO_SCOPE).append_pair("access_type", "offline").append_pair("prompt", "consent");
    } else {
        let scope = if crm == Crm::Salesforce { SALESFORCE_SCOPE.to_string() } else { dynamics_scope(site) };
        query.append_pair("scope", &scope).append_pair("code_challenge", &pkce_challenge(verifier)).append_pair("code_challenge_method", "S256");
    }
    if crm == Crm::Dynamics {
        query.append_pair("prompt", "select_account");
    }
    drop(query);
    Some(url.into())
}

/// Trades the callback's code for tokens, checks the CRM lets Splee in, and saves the connection. `kept` is the state row's
/// site, and `accounts` the accounts server Zoho's callback named. Errors are as the integrations page shows them.
pub async fn connect(state: &AppState, project_id: Uuid, crm: Crm, kept: &str, accounts: Option<&str>, code: &str, verifier: &str) -> Result<(), String> {
    let label = crm.label();
    let app = app(&state.cfg, crm).ok_or_else(|| not_set_up(crm))?;
    let sandbox = crm == Crm::Salesforce && kept == SANDBOX;
    let token_url = match crm {
        Crm::Salesforce if sandbox => sandboxed(&app.token_url),
        Crm::Zoho => {
            let accounts = zoho_accounts(app, accounts).ok_or("Zoho sent the sign-in back from a server Splee doesn't know. Connect Zoho CRM again.")?;
            format!("{accounts}/oauth/v2/token")
        }
        _ => app.token_url.clone(),
    };
    let app = OAuthApp { token_url: token_url.clone(), ..app.clone() };
    let redirect = redirect_uri(&state.cfg);
    let mut form = vec![("grant_type", "authorization_code"), ("code", code), ("redirect_uri", redirect.as_str())];
    if crm != Crm::Zoho {
        form.push(("code_verifier", verifier));
    }
    let tokens = token_request(label, &app, &form).await.map_err(|e| match e {
        TokenError::Revoked(_) => format!("This sign-in expired. Connect {label} again."),
        TokenError::Unavailable(_, e) => format!("Couldn't reach {label}: {e}. Try again."),
        TokenError::Refused(_, e) => format!("{label} turned the sign-in down: {e}"),
    })?;
    let refresh = tokens.refresh_token.as_deref().filter(|t| !t.is_empty()).ok_or_else(|| format!("{label} didn't let Splee stay signed in. Connect {label} again."))?;
    let site = match crm {
        Crm::Salesforce => tokens.instance_url.clone().ok_or("Salesforce didn't say where the org is. Connect Salesforce again.")?,
        Crm::Zoho => tokens.api_domain.clone().unwrap_or_else(|| ZOHO_API.into()),
        _ => kept.to_string(),
    };
    let site = site.trim_end_matches('/').to_string();
    let session = Session { site: site.clone(), access: tokens.access_token.clone(), booked_status: String::new() };
    let statuses = verify(crm, &session.token()).await.map_err(|e| format!("{label} didn't let Splee in: {e:#}"))?;

    // Zoho's API domain names no data centre a person knows, so the page shows the accounts server's.
    let shown = if crm == Crm::Zoho { &token_url } else { &site };
    let shown = Url::parse(shown).ok().and_then(|u| Some(u.host_str()?.trim_start_matches("accounts.").to_string())).unwrap_or_default();
    let secret = &state.cfg.app_secret;
    let connection = serde_json::to_string(&Connection { site, token_url }).expect("a struct of strings serializes");
    let internal = |e: sqlx::Error| {
        tracing::error!(error = %e, crm = crm.name(), "couldn't save a CRM sign-in");
        format!("Something went wrong saving the {label} sign-in. Try again.")
    };
    // The names come from Crm::name, never from input.
    let n = crm.name();
    sqlx::query(&format!(
        "update project_settings set {n}_token_enc = $2, {n}_refresh_enc = $3, {n}_access_enc = $4, {n}_expires_at = $5, {n}_site = $6,
           {n}_needs_reconnect = false, {n}_error = null, {n}_failed_at = null
         where project_id = $1"
    ))
    .bind(project_id)
    .bind(crypto::encrypt(secret, &connection))
    .bind(crypto::encrypt(secret, refresh))
    .bind(crypto::encrypt(secret, &tokens.access_token))
    .bind(tokens.expires_at())
    .bind(shown)
    .execute(&state.db)
    .await
    .map_err(internal)?;
    if let Some(statuses) = statuses {
        save_statuses(&state.db, project_id, &statuses, Some(sandbox)).await.map_err(internal)?;
    }
    Ok(())
}

/// Checks the session works. Salesforce's check reads the Lead Status picklist, which comes back to be saved.
async fn verify(crm: Crm, token: &str) -> anyhow::Result<Option<Value>> {
    if crm == Crm::Salesforce {
        return salesforce::statuses(token).await.map(Some);
    }
    crm.check(token).await.map(|_| None)
}

/// Keeps the status booked leads get while the org still has it.
async fn save_statuses(db: &PgPool, project_id: Uuid, statuses: &Value, sandbox: Option<bool>) -> sqlx::Result<()> {
    sqlx::query(
        "update project_settings set salesforce_statuses = $2, salesforce_sandbox = coalesce($3, salesforce_sandbox),
           salesforce_booked_status = case when $2 @> jsonb_build_array(jsonb_build_object('value', salesforce_booked_status)) then salesforce_booked_status else '' end
         where project_id = $1",
    )
    .bind(project_id)
    .bind(statuses)
    .bind(sandbox)
    .execute(db)
    .await?;
    Ok(())
}

/// A session with the stored access token while it has a minute left, else with a new one, and whether it's new. `rejected`
/// is an access token the CRM just turned down: one another sync saved since is used instead, and otherwise a new one is
/// fetched. Under a lock on the project and CRM, since a hot lead and a booking can sync at once, and Microsoft's renewal
/// replaces the refresh token.
async fn session(state: &AppState, project_id: Uuid, crm: Crm, rejected: Option<&str>) -> anyhow::Result<(Session, bool)> {
    let (n, label) = (crm.name(), crm.label());
    let mut tx = state.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('crm-token:' || $1::text || ':' || $2, 0))").bind(project_id).bind(n).execute(&mut *tx).await?;
    let booked = if crm == Crm::Salesforce { "salesforce_booked_status" } else { "''" };
    let sql = format!("select {n}_token_enc, {n}_refresh_enc, {n}_access_enc, {n}_expires_at, {n}_needs_reconnect, {booked} from project_settings where project_id = $1");
    let (connection_enc, refresh_enc, access_enc, expires_at, needs_reconnect, booked_status): (String, String, String, Option<Ts>, bool, String) =
        sqlx::query_as(&sql).bind(project_id).fetch_one(&mut *tx).await?;
    anyhow::ensure!(!connection_enc.is_empty(), "connect {label} first");
    // Nothing sends the CRM a refresh token it turned down, so a revoked sign-in isn't tried on every lead.
    anyhow::ensure!(!needs_reconnect, "{}", signed_out(crm));
    let secret = &state.cfg.app_secret;
    let connection: Connection = serde_json::from_str(&crypto::decrypt(secret, &connection_enc)?)?;
    let session = |access: String| Session { site: connection.site.clone(), access, booked_status: booked_status.clone() };
    if !access_enc.is_empty() && expires_at.is_some_and(|at| at > Utc::now() + chrono::Duration::seconds(60)) {
        let access = crypto::decrypt(secret, &access_enc)?;
        if rejected != Some(access.as_str()) {
            return Ok((session(access), false));
        }
    }
    let app = app(&state.cfg, crm).ok_or_else(|| anyhow!(not_set_up(crm)))?;
    let app = OAuthApp { token_url: connection.token_url.clone(), ..app.clone() };
    let refresh = crypto::decrypt(secret, &refresh_enc)?;
    let scope = dynamics_scope(&connection.site);
    let mut form = vec![("grant_type", "refresh_token"), ("refresh_token", refresh.as_str())];
    // Microsoft hands out a token for the scopes asked for here, and the first one asked for without them.
    if crm == Crm::Dynamics {
        form.push(("scope", &scope));
    }
    match token_request(label, &app, &form).await {
        Ok(tokens) => {
            // Matched on the old refresh token, so a reconnect since keeps its own.
            sqlx::query(&format!(
                "update project_settings set {n}_access_enc = $3, {n}_expires_at = $4, {n}_refresh_enc = coalesce($5, {n}_refresh_enc)
                 where project_id = $1 and {n}_refresh_enc = $2"
            ))
            .bind(project_id)
            .bind(&refresh_enc)
            .bind(crypto::encrypt(secret, &tokens.access_token))
            .bind(tokens.expires_at())
            .bind(tokens.refresh_token.as_deref().filter(|t| !t.is_empty()).map(|t| crypto::encrypt(secret, t)))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok((session(tokens.access_token), true))
        }
        Err(TokenError::Revoked(_)) => {
            tracing::warn!(project = %project_id, crm = n, "a CRM revoked its sign-in");
            sqlx::query(&format!(
                "update project_settings set {n}_needs_reconnect = true, {n}_access_enc = '', {n}_expires_at = null where project_id = $1 and {n}_refresh_enc = $2"
            ))
            .bind(project_id)
            .bind(&refresh_enc)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Err(anyhow!(signed_out(crm)))
        }
        Err(e) => Err(e.into()),
    }
}

/// Whether to try again with another access token: the CRM turned down one that wasn't new, since a token can stop working
/// before it runs out, like when Salesforce's session timeout is under an hour. A new one it turns down isn't renewed again.
fn renew_after<T>(renewed: bool, result: &anyhow::Result<T>) -> bool {
    !renewed && result.as_ref().is_err_and(|e| e.is::<Unauthorized>())
}

fn turned_down(crm: Crm, e: anyhow::Error) -> anyhow::Error {
    match e.is::<Unauthorized>() {
        true => anyhow!("{} turned down a new sign-in, so check the account can still use the API or reconnect it", crm.label()),
        false => e,
    }
}

pub async fn push(state: &AppState, project_id: Uuid, crm: Crm, email: &str, lead: &crate::models::Lead, note: &str) -> anyhow::Result<()> {
    let (first, renewed) = session(state, project_id, crm, None).await?;
    let result = crm.push(&first.token(), email, lead, note).await;
    if !renew_after(renewed, &result) {
        return result.map_err(|e| turned_down(crm, e));
    }
    let (next, _) = session(state, project_id, crm, Some(&first.access)).await?;
    crm.push(&next.token(), email, lead, note).await.map_err(|e| turned_down(crm, e))
}

/// For the Test button. Salesforce's Lead Status picklist is read again, in case the org changed it.
pub async fn test(state: &AppState, project_id: Uuid, crm: Crm) -> anyhow::Result<()> {
    let (first, renewed) = session(state, project_id, crm, None).await?;
    let mut result = verify(crm, &first.token()).await;
    if renew_after(renewed, &result) {
        let (next, _) = session(state, project_id, crm, Some(&first.access)).await?;
        result = verify(crm, &next.token()).await;
    }
    let statuses = result.map_err(|e| turned_down(crm, e))?;
    if let Some(statuses) = statuses {
        save_statuses(&state.db, project_id, &statuses, None).await?;
    }
    Ok(())
}

/// Signs Splee out where the CRM lets it, then forgets the sign-in. Revoking is best effort: the CRM may be down or have
/// revoked the token already, and disconnecting works either way.
pub async fn disconnect(db: &PgPool, cfg: &Config, project_id: Uuid, crm: Crm) -> sqlx::Result<()> {
    let n = crm.name();
    let sql = format!("select {n}_token_enc, {n}_refresh_enc from project_settings where project_id = $1");
    let (connection_enc, refresh_enc): (String, String) = sqlx::query_as(&sql).bind(project_id).fetch_optional(db).await?.unwrap_or_default();
    let connection = crypto::decrypt(&cfg.app_secret, &connection_enc).ok().and_then(|c| serde_json::from_str::<Connection>(&c).ok());
    let refresh = crypto::decrypt(&cfg.app_secret, &refresh_enc).unwrap_or_default();
    if let Some(connection) = connection.filter(|_| !refresh.is_empty())
        && let Err(e) = revoke(crm, &connection.token_url, &refresh).await
    {
        tracing::info!(error = %e, crm = n, "couldn't revoke a CRM sign-in");
    }
    let salesforce = if crm == Crm::Salesforce { ", salesforce_sandbox = false, salesforce_statuses = '[]', salesforce_booked_status = ''" } else { "" };
    sqlx::query(&format!(
        "update project_settings set {n}_token_enc = '', {n}_refresh_enc = '', {n}_access_enc = '', {n}_expires_at = null, {n}_needs_reconnect = false,
           {n}_site = '', {n}_error = null, {n}_failed_at = null{salesforce}
         where project_id = $1"
    ))
    .bind(project_id)
    .execute(db)
    .await?;
    Ok(())
}

async fn revoke(crm: Crm, token_url: &str, refresh: &str) -> anyhow::Result<()> {
    let client = client()?;
    match crm {
        Crm::Salesforce => {
            send(client.post(token_url.replace("/oauth2/token", "/oauth2/revoke")).form(&[("token", refresh)])).await?;
        }
        Crm::Zoho => {
            send(client.post(Url::parse_with_params(&format!("{token_url}/revoke"), [("token", refresh)])?)).await?;
        }
        // Microsoft can't revoke one app's refresh token. It stops working when the person removes the app's access, or after
        // 90 days unused.
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, Method, StatusCode, Uri};
    use serde_json::json;

    use super::*;
    use crate::mail::oauth::tests::{FakeProvider, config};
    use crate::models::Lead;
    use crate::twilio::tests::test_lead;

    /// Looks like a Salesforce session id, so it's split to keep secret scanners quiet.
    pub(crate) const LIVE: &str = concat!("00Dfake0000", "0000!AQ0AQFake", "AccessTokenForTests");

    /// One request the fake got. `body` is the JSON or the form, as an object.
    #[derive(Clone, Debug)]
    pub(crate) struct Seen {
        pub method: String,
        pub path: String,
        pub query: HashMap<String, String>,
        pub body: Value,
        pub token: String,
    }

    #[derive(Default)]
    pub(crate) struct World {
        pub requests: Vec<Seen>,
        /// Each with "kind" (sf:Lead, sf:Contact, sf:Task, zoho:Lead, zoho:Note, d:lead, d:annotation) and "id".
        pub records: Vec<Value>,
        /// The access tokens its APIs take.
        pub live: HashSet<String>,
        n: usize,
    }

    /// Salesforce under /salesforce, Zoho's accounts server under /oauth/v2 and its API under /zoho, Microsoft's token endpoint
    /// at /dynamics/token and a Dynamics org at the root. A token endpoint turns down the refresh token "revoked" the way
    /// each CRM does, hands out access-{n}, and for a code, the refresh token refresh-for-{code}. Microsoft replaces the
    /// refresh token each time.
    #[derive(Clone)]
    pub(crate) struct Fake {
        pub base: String,
        pub world: Arc<Mutex<World>>,
    }

    impl Fake {
        pub(crate) async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let fake = Fake { base: format!("http://{}", listener.local_addr().unwrap()), world: Arc::default() };
            fake.world.lock().unwrap().live.insert(LIVE.into());
            let app = Router::new().fallback(answer).with_state(fake.clone());
            tokio::spawn(async move { axum::serve(listener, app).await });
            fake
        }

        pub(crate) fn cfg(&self) -> Config {
            let app = |authorize: &str, token: &str| OAuthApp {
                client_id: "client-123".into(),
                client_secret: "shh".into(),
                authorize_url: format!("{}{authorize}", self.base),
                token_url: format!("{}{token}", self.base),
            };
            let unused = FakeProvider { url: self.base.clone(), hits: Arc::default(), forms: Arc::default() };
            Config {
                salesforce: Some(app("/salesforce/services/oauth2/authorize", "/salesforce/services/oauth2/token")),
                zoho: Some(app("/oauth/v2/auth", "/oauth/v2/token")),
                microsoft: Some(app("/dynamics/authorize", "/dynamics/token")),
                crm_api_url: self.base.clone(),
                ..config(&unused)
            }
        }

        pub(crate) fn state(&self, db: PgPool) -> AppState {
            AppState { db, cfg: Arc::new(self.cfg()), events: tokio::sync::broadcast::channel(1).0 }
        }

        /// Where each CRM's API and tokens are on the fake.
        pub(crate) fn connection(&self, crm: Crm) -> (String, String) {
            let b = &self.base;
            match crm {
                Crm::Salesforce => (format!("{b}/salesforce"), format!("{b}/salesforce/services/oauth2/token")),
                Crm::Zoho => (format!("{b}/zoho"), format!("{b}/oauth/v2/token")),
                _ => (b.clone(), format!("{b}/dynamics/token")),
            }
        }

        pub(crate) fn requests(&self) -> Vec<Seen> {
            self.world.lock().unwrap().requests.clone()
        }

        pub(crate) fn records(&self, kind: &str) -> Vec<Value> {
            self.world.lock().unwrap().records.iter().filter(|r| r["kind"] == kind).cloned().collect()
        }

        pub(crate) fn token_requests(&self) -> Vec<Seen> {
            self.requests().into_iter().filter(|r| r.path.ends_with("/token")).collect()
        }
    }

    fn form(raw: &[u8]) -> Value {
        let url = Url::parse(&format!("http://x/?{}", String::from_utf8_lossy(raw))).unwrap();
        url.query_pairs().map(|(k, v)| (k.into_owned(), Value::String(v.into_owned()))).collect()
    }

    /// The inside of the first quoted string after `after`, unescaped with `unescape`.
    fn quoted(text: &str, after: &str, unescape: fn(&str) -> (char, usize)) -> String {
        let start = text.find(after).unwrap() + after.len();
        let rest = &text[start..];
        let mut out = String::new();
        let mut i = 0;
        while i < rest.len() {
            let (c, used) = unescape(&rest[i..]);
            if used == 0 {
                break;
            }
            out.push(c);
            i += used;
        }
        out
    }

    fn soql_char(s: &str) -> (char, usize) {
        let mut chars = s.chars();
        match chars.next() {
            Some('\\') => {
                let c = chars.next().unwrap();
                (match c { 'n' => '\n', 'r' => '\r', 't' => '\t', c => c }, 1 + c.len_utf8())
            }
            Some('\'') | None => ('\0', 0),
            Some(c) => (c, c.len_utf8()),
        }
    }

    fn odata_char(s: &str) -> (char, usize) {
        match s.chars().next() {
            Some('\'') if s[1..].starts_with('\'') => ('\'', 2),
            Some('\'') | None => ('\0', 0),
            Some(c) => (c, c.len_utf8()),
        }
    }

    async fn answer(State(fake): State<Fake>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> (StatusCode, axum::Json<Value>) {
        let path = uri.path().to_string();
        let query: HashMap<String, String> = Url::parse(&format!("http://x{uri}")).unwrap().query_pairs().into_owned().collect();
        let form_body = headers.get("content-type").is_some_and(|c| c.to_str().unwrap_or_default().starts_with("application/x-www-form-urlencoded"));
        let body = if form_body { form(&body) } else { serde_json::from_slice(&body).unwrap_or_default() };
        let auth = headers.get("authorization").and_then(|a| a.to_str().ok()).unwrap_or_default();
        let token = auth.split_once(' ').map(|(_, t)| t.to_string()).unwrap_or_default();
        let mut world = fake.world.lock().unwrap();
        world.requests.push(Seen { method: method.to_string(), path: path.clone(), query: query.clone(), body: body.clone(), token: token.clone() });
        world.n += 1;
        let n = world.n;
        let ok = |v: Value| (StatusCode::OK, axum::Json(v));
        let b = &fake.base;

        if path.ends_with("/token") {
            let grant = body["grant_type"].as_str().unwrap_or_default();
            let refresh = body["refresh_token"].as_str().unwrap_or_default();
            let access = format!("access-{n}");
            world.live.insert(access.clone());
            let mut tokens = json!({"access_token": access, "token_type": "Bearer"});
            if grant == "authorization_code" {
                tokens["refresh_token"] = json!(format!("refresh-for-{}", body["code"].as_str().unwrap_or_default()));
            } else if refresh == "revoked" {
                return match path.as_str() {
                    // Zoho says no with a 200.
                    "/oauth/v2/token" => ok(json!({"error": "invalid_code"})),
                    _ => (StatusCode::BAD_REQUEST, axum::Json(json!({"error": "invalid_grant", "error_description": "expired access/refresh token"}))),
                };
            } else if path == "/dynamics/token" {
                tokens["refresh_token"] = json!(format!("rotated-{n}"));
            }
            match path.as_str() {
                "/salesforce/services/oauth2/token" => tokens["instance_url"] = json!(format!("{b}/salesforce")),
                "/oauth/v2/token" => {
                    tokens["api_domain"] = json!(format!("{b}/zoho"));
                    tokens["expires_in"] = json!(3600);
                }
                _ => tokens["expires_in"] = json!(3599),
            }
            return ok(tokens);
        }
        if path.ends_with("/revoke") {
            return ok(json!({"status": "success"}));
        }
        if !world.live.contains(&token) {
            return (StatusCode::UNAUTHORIZED, axum::Json(json!([{"message": "Session expired or invalid", "errorCode": "INVALID_SESSION_ID"}])));
        }
        let create = |world: &mut World, kind: &str, mut record: Value| {
            let id = format!("{kind}-{n}");
            record["kind"] = json!(kind);
            record["id"] = json!(id);
            world.records.push(record);
            id
        };
        let find = |world: &World, kind: &str, field: &str, value: &str| {
            world.records.iter().position(|r| r["kind"] == kind && r[field].as_str().is_some_and(|v| v.eq_ignore_ascii_case(value)) && r["IsConverted"] != true)
        };

        if let Some(rest) = path.strip_prefix("/salesforce/services/data/v66.0/") {
            return match (method.as_str(), rest) {
                ("GET", "sobjects/Lead/describe") => ok(json!({"fields": [
                    {"name": "Email", "picklistValues": []},
                    {"name": "Status", "picklistValues": [
                        {"value": "Open - Not Contacted", "label": "Open", "active": true},
                        {"value": "Working - Contacted", "label": "Working", "active": true},
                        {"value": "Old", "label": "Old", "active": false},
                        {"value": "Meeting Set", "label": "Meeting set", "active": true},
                    ]},
                ]})),
                ("GET", "query") => {
                    let q = &query["q"];
                    let object = q.split(" from ").nth(1).unwrap().split(' ').next().unwrap();
                    let email = quoted(q, "Email = '", soql_char);
                    let found = find(&world, &format!("sf:{object}"), "Email", &email);
                    ok(json!({"records": found.map(|i| vec![json!({"Id": world.records[i]["id"]})]).unwrap_or_default()}))
                }
                ("POST", object) => {
                    let id = create(&mut world, &format!("sf:{}", object.trim_start_matches("sobjects/")), body);
                    (StatusCode::CREATED, axum::Json(json!({"id": id, "success": true, "errors": []})))
                }
                ("PATCH", record) => {
                    let id = record.rsplit('/').next().unwrap();
                    let found = world.records.iter_mut().find(|r| r["id"] == id).unwrap();
                    found.as_object_mut().unwrap().extend(body.as_object().unwrap().clone());
                    (StatusCode::NO_CONTENT, axum::Json(Value::Null))
                }
                _ => (StatusCode::NOT_FOUND, axum::Json(json!([{"message": "no such resource"}]))),
            };
        }
        if let Some(rest) = path.strip_prefix("/zoho/crm/v8/") {
            return match (method.as_str(), rest) {
                ("GET", "Leads") => (StatusCode::NO_CONTENT, axum::Json(Value::Null)),
                ("POST", "Leads/upsert") => {
                    assert_eq!(body["duplicate_check_fields"], json!(["Email"]));
                    let record = body["data"][0].clone();
                    let (id, action) = match find(&world, "zoho:Lead", "Email", record["Email"].as_str().unwrap()) {
                        Some(i) => {
                            world.records[i].as_object_mut().unwrap().extend(record.as_object().unwrap().clone());
                            (world.records[i]["id"].as_str().unwrap().to_string(), "update")
                        }
                        None => (create(&mut world, "zoho:Lead", record), "insert"),
                    };
                    ok(json!({"data": [{"code": "SUCCESS", "status": "success", "action": action, "details": {"id": id}}]}))
                }
                ("POST", "Notes") => {
                    let id = create(&mut world, "zoho:Note", body["data"][0].clone());
                    (StatusCode::CREATED, axum::Json(json!({"data": [{"code": "SUCCESS", "status": "success", "details": {"id": id}}]})))
                }
                _ => (StatusCode::NOT_FOUND, axum::Json(json!({"code": "INVALID_URL_PATTERN", "message": "Please check if the URL trying to access is a correct one"}))),
            };
        }
        if let Some(rest) = path.strip_prefix("/api/data/v9.2/") {
            return match (method.as_str(), rest) {
                ("GET", "WhoAmI") => ok(json!({"UserId": "u-1"})),
                ("GET", "leads") => {
                    let email = quoted(&query["$filter"], "emailaddress1 eq '", odata_char);
                    let found = find(&world, "d:lead", "emailaddress1", &email);
                    ok(json!({"value": found.map(|i| vec![json!({"leadid": world.records[i]["id"]})]).unwrap_or_default()}))
                }
                ("POST", "leads") => {
                    assert_eq!(headers["prefer"], "return=representation");
                    let id = create(&mut world, "d:lead", body);
                    (StatusCode::CREATED, axum::Json(json!({"leadid": id})))
                }
                ("POST", "annotations") => {
                    create(&mut world, "d:annotation", body);
                    (StatusCode::NO_CONTENT, axum::Json(Value::Null))
                }
                ("PATCH", record) if record.starts_with("leads(") => {
                    assert_eq!(headers["if-match"], "*");
                    let id = record.trim_start_matches("leads(").trim_end_matches(')');
                    let found = world.records.iter_mut().find(|r| r["id"] == id).unwrap();
                    found.as_object_mut().unwrap().extend(body.as_object().unwrap().clone());
                    (StatusCode::NO_CONTENT, axum::Json(Value::Null))
                }
                _ => (StatusCode::NOT_FOUND, axum::Json(json!({"error": {"code": "0x80060888", "message": "Resource not found for the segment"}}))),
            };
        }
        (StatusCode::NOT_FOUND, axum::Json(json!({"message": "not on the fake"})))
    }

    pub(crate) async fn project(db: &PgPool, org: &str) -> Uuid {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(org).fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'P') returning id").bind(org).fetch_one(db).await.unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(db).await.unwrap();
        project
    }

    /// Connects `crm` on the fake with these tokens, as a sign-in would have, the access token running out in `seconds`.
    pub(crate) async fn connect_fake(state: &AppState, fake: &Fake, project: Uuid, crm: Crm, access: &str, refresh: &str, seconds: i64) {
        let (site, token_url) = fake.connection(crm);
        let secret = &state.cfg.app_secret;
        let connection = serde_json::to_string(&Connection { site, token_url }).unwrap();
        let n = crm.name();
        sqlx::query(&format!(
            "update project_settings set {n}_token_enc = $2, {n}_refresh_enc = $3, {n}_access_enc = $4, {n}_expires_at = now() + make_interval(secs => $5), {n}_site = 'fake'
             where project_id = $1"
        ))
        .bind(project)
        .bind(crypto::encrypt(secret, &connection))
        .bind(crypto::encrypt(secret, refresh))
        .bind(crypto::encrypt(secret, access))
        .bind(seconds as f64)
        .execute(&state.db)
        .await
        .unwrap();
    }

    fn lead(email: &str) -> Lead {
        Lead {
            full_name: "Sarah van Dijk".into(),
            title: "Head of Sales".into(),
            company_name: "Northwind".into(),
            company_domain: Some("northwind.test".into()),
            email: Some(email.into()),
            status: "hot".into(),
            ..test_lead()
        }
    }

    const SIGN_INS: [Crm; 3] = [Crm::Salesforce, Crm::Zoho, Crm::Dynamics];

    async fn column<T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin>(db: &PgPool, project: Uuid, column: &str) -> T {
        sqlx::query_scalar(&format!("select {column} from project_settings where project_id = $1")).bind(project).fetch_one(db).await.unwrap()
    }

    #[test]
    fn org_addresses_must_be_dynamics() {
        let cfg = Config { crm_api_url: "http://127.0.0.1:9".into(), ..Fake { base: String::new(), world: Arc::default() }.cfg() };
        let root = |input: &str| org_root(&cfg, input);
        assert_eq!(root("https://acme.crm.dynamics.com/main.aspx?appid=1"), Some("https://acme.crm.dynamics.com".into()));
        assert_eq!(root(" acme-eu.crm4.dynamics.com "), Some("https://acme-eu.crm4.dynamics.com".into()));
        assert_eq!(root("http://127.0.0.1:9/"), Some("http://127.0.0.1:9".into()), "the fake, in tests");
        for bad in ["http://acme.crm.dynamics.com", "https://acme.crm.dynamics.com.evil.test", "https://crm.dynamics.com", "https://acme.crmx.dynamics.com", "https://evil.test", "https://acme.crm.dynamics.com:8443"] {
            assert_eq!(root(bad), None, "{bad}");
        }
    }

    #[test]
    fn zoho_only_trades_codes_at_its_own_accounts_servers() {
        let app = OAuthApp { client_id: String::new(), client_secret: String::new(), authorize_url: ZOHO_AUTHORIZE.into(), token_url: ZOHO_TOKEN.into() };
        assert_eq!(zoho_accounts(&app, Some("https://accounts.zoho.eu")).as_deref(), Some("https://accounts.zoho.eu"));
        assert_eq!(zoho_accounts(&app, Some("https://accounts.zohocloud.ca/")).as_deref(), Some("https://accounts.zohocloud.ca"));
        assert_eq!(zoho_accounts(&app, None).as_deref(), Some("https://accounts.zoho.com"));
        assert_eq!(zoho_accounts(&app, Some("https://accounts.zoho.eu.evil.test")), None);
        assert_eq!(zoho_accounts(&app, Some("https://evil.test")), None);
    }

    #[test]
    fn sign_in_pages_ask_for_what_each_crm_needs() {
        let cfg = Config { salesforce: Some(OAuthApp { authorize_url: SALESFORCE_AUTHORIZE.into(), token_url: SALESFORCE_TOKEN.into(), ..Fake { base: String::new(), world: Arc::default() }.cfg().salesforce.unwrap() }), ..Fake { base: "https://fake.test".into(), world: Arc::default() }.cfg() };
        let q = |crm: Crm, site: &str| -> (String, HashMap<String, String>) {
            let url = Url::parse(&authorize_url(&cfg, crm, site, "nonce.sig", "a-verifier").unwrap()).unwrap();
            (format!("{}{}", url.host_str().unwrap(), url.path()), url.query_pairs().into_owned().collect())
        };
        let (page, sf) = q(Crm::Salesforce, SANDBOX);
        assert_eq!(page, "test.salesforce.com/services/oauth2/authorize");
        assert_eq!((sf["scope"].as_str(), sf["code_challenge_method"].as_str()), ("api refresh_token", "S256"));
        assert_eq!(sf["code_challenge"], pkce_challenge("a-verifier"));
        assert_eq!(sf["redirect_uri"], "http://localhost:3404/api/crm/oauth/callback");
        assert_eq!(q(Crm::Salesforce, "").0, "login.salesforce.com/services/oauth2/authorize");
        let (_, zoho) = q(Crm::Zoho, "");
        assert_eq!((zoho["scope"].as_str(), zoho["access_type"].as_str(), zoho["prompt"].as_str()), (ZOHO_SCOPE, "offline", "consent"));
        assert!(!zoho.contains_key("code_challenge"));
        let (_, dynamics) = q(Crm::Dynamics, "https://acme.crm4.dynamics.com");
        assert_eq!(dynamics["scope"], "https://acme.crm4.dynamics.com/user_impersonation offline_access");
        assert_eq!((dynamics["state"].as_str(), dynamics["prompt"].as_str()), ("nonce.sig", "select_account"));
        assert!(authorize_url(&Config { zoho: None, ..cfg.clone() }, Crm::Zoho, "", "s", "v").is_none());
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_crm_adds_a_lead_once_and_updates_it_by_email_with_the_note(db: PgPool) {
        let fake = Fake::start().await;
        let state = fake.state(db);
        let project = project(&state.db, "Acme").await;
        for crm in SIGN_INS {
            connect_fake(&state, &fake, project, crm, LIVE, "good", 3600).await;
        }
        let awkward = "O'Brien+crm@Acme.test";
        crate::crm::sync(&state, project, &lead(awkward), "Hot lead in Splee. They replied:\n\nTuesday works <3").await;
        let booked = Lead { status: "meeting_booked".into(), title: "VP Sales".into(), ..lead(awkward) };
        sqlx::query("update project_settings set salesforce_booked_status = 'Meeting Set' where project_id = $1").bind(project).execute(&state.db).await.unwrap();
        crate::crm::sync(&state, project, &booked, "Booked a meeting through Splee.").await;

        let one = |kind: &str| {
            let records = fake.records(kind);
            assert_eq!(records.len(), 1, "{kind}: {records:?}");
            records[0].clone()
        };
        // Salesforce: found again with an escaped SOQL string, with the plus intact.
        let sf = one("sf:Lead");
        assert_eq!((sf["Email"].as_str(), sf["LastName"].as_str(), sf["Company"].as_str()), (Some("o'brien+crm@acme.test"), Some("van Dijk"), Some("Northwind")));
        assert_eq!((sf["Title"].as_str(), sf["Status"].as_str()), (Some("VP Sales"), Some("Meeting Set")), "the booked lead got the picked status");
        let queries: Vec<String> = fake.requests().iter().filter_map(|r| r.query.get("q").cloned()).collect();
        assert!(queries.contains(&"select Id from Lead where Email = 'o\\'brien+crm@acme.test' and IsConverted = false order by LastModifiedDate desc limit 1".into()), "{queries:?}");
        assert!(fake.requests().iter().any(|r| r.method == "PATCH" && r.path.ends_with(&format!("sobjects/Lead/{}", sf["id"].as_str().unwrap()))));
        let tasks = fake.records("sf:Task");
        assert_eq!(tasks.len(), 2);
        assert_eq!((tasks[0]["WhoId"].as_str(), tasks[0]["Subject"].as_str()), (sf["id"].as_str(), Some("Hot lead in Splee. They replied:")));
        assert_eq!(tasks[0]["Description"], "Hot lead in Splee. They replied:\n\nTuesday works <3", "plain text, not HTML");
        assert_eq!(tasks[1]["ActivityDate"], Utc::now().date_naive().to_string());

        let zoho = one("zoho:Lead");
        assert_eq!((zoho["Email"].as_str(), zoho["Last_Name"].as_str(), zoho["Designation"].as_str()), (Some("o'brien+crm@acme.test"), Some("van Dijk"), Some("VP Sales")));
        let notes = fake.records("zoho:Note");
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0]["Parent_Id"], json!({"module": {"api_name": "Leads"}, "id": zoho["id"]}));
        assert_eq!(notes[0]["Note_Content"], "Hot lead in Splee. They replied:\n\nTuesday works <3");
        assert!(fake.requests().iter().filter(|r| r.path.starts_with("/zoho/")).all(|r| r.token == LIVE));

        let d = one("d:lead");
        assert_eq!((d["emailaddress1"].as_str(), d["lastname"].as_str(), d["jobtitle"].as_str()), (Some("o'brien+crm@acme.test"), Some("van Dijk"), Some("VP Sales")));
        let filters: Vec<String> = fake.requests().iter().filter_map(|r| r.query.get("$filter").cloned()).collect();
        assert_eq!(filters[0], "emailaddress1 eq 'o''brien+crm@acme.test'");
        let annotations = fake.records("d:annotation");
        assert_eq!(annotations.len(), 2);
        assert_eq!(annotations[1]["notetext"], "Booked a meeting through Splee.");
        assert_eq!(annotations[0]["objectid_lead@odata.bind"], format!("/leads({})", d["id"].as_str().unwrap()));

        for crm in SIGN_INS {
            assert_eq!(column::<Option<String>>(&state.db, project, &format!("{}_error", crm.name())).await, None, "{crm:?}");
        }
        assert!(fake.token_requests().is_empty(), "the live access tokens were used as they were");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn salesforce_updates_a_contact_with_the_address_instead_of_adding_a_lead(db: PgPool) {
        let fake = Fake::start().await;
        let state = fake.state(db);
        let project = project(&state.db, "Acme").await;
        connect_fake(&state, &fake, project, Crm::Salesforce, LIVE, "good", 3600).await;
        fake.world.lock().unwrap().records.extend([
            json!({"kind": "sf:Contact", "id": "003-sarah", "Email": "Sarah@Northwind.test", "LastName": "Old"}),
            json!({"kind": "sf:Lead", "id": "00Q-converted", "Email": "sam@northwind.test", "IsConverted": true}),
        ]);
        let booked = Lead { status: "meeting_booked".into(), ..lead("sarah@northwind.test") };
        sqlx::query("update project_settings set salesforce_booked_status = 'Meeting Set' where project_id = $1").bind(project).execute(&state.db).await.unwrap();
        crate::crm::sync(&state, project, &booked, "Booked.").await;
        let contact = &fake.records("sf:Contact")[0];
        assert_eq!((contact["LastName"].as_str(), contact["Title"].as_str(), contact.get("Status")), (Some("van Dijk"), Some("Head of Sales"), None));
        assert!(contact.get("Company").is_none(), "contacts have no company field");
        assert_eq!(fake.records("sf:Task")[0]["WhoId"], "003-sarah");
        assert_eq!(fake.records("sf:Lead").len(), 1, "no lead was added");

        // A converted lead's address gets a new lead.
        crate::crm::sync(&state, project, &lead("sam@northwind.test"), "Hot.").await;
        assert_eq!(fake.records("sf:Lead").len(), 2);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn access_tokens_are_renewed_when_they_run_out_or_are_turned_down(db: PgPool) {
        let fake = Fake::start().await;
        let state = fake.state(db);
        let project = project(&state.db, "Acme").await;
        // Salesforce's ran out, Zoho's is turned down before it runs out, and Microsoft replaces its refresh token.
        connect_fake(&state, &fake, project, Crm::Salesforce, LIVE, "sf-refresh", -10).await;
        connect_fake(&state, &fake, project, Crm::Zoho, "stale", "zoho-refresh", 3600).await;
        connect_fake(&state, &fake, project, Crm::Dynamics, "stale", "ms-refresh", 30).await;
        crate::crm::sync(&state, project, &lead("ana@acme.test"), "Hot.").await;

        let renewals = fake.token_requests();
        assert_eq!(renewals.len(), 3, "{renewals:?}");
        let by_path = |path: &str| renewals.iter().find(|r| r.path == path).unwrap().body.clone();
        let sf = by_path("/salesforce/services/oauth2/token");
        assert_eq!((sf["grant_type"].as_str(), sf["refresh_token"].as_str(), sf["client_secret"].as_str()), (Some("refresh_token"), Some("sf-refresh"), Some("shh")));
        assert_eq!(by_path("/oauth/v2/token")["refresh_token"], "zoho-refresh");
        let ms = by_path("/dynamics/token");
        assert_eq!(ms["scope"], format!("{}/user_impersonation offline_access", fake.base));
        for crm in SIGN_INS {
            assert_eq!(column::<Option<String>>(&state.db, project, &format!("{}_error", crm.name())).await, None, "{crm:?}");
        }
        assert_eq!((fake.records("sf:Lead").len(), fake.records("zoho:Lead").len(), fake.records("d:lead").len()), (1, 1, 1));
        let refresh: String = column(&state.db, project, "dynamics_refresh_enc").await;
        assert!(crypto::decrypt(&state.cfg.app_secret, &refresh).unwrap().starts_with("rotated-"), "Microsoft's new refresh token is kept");
        let refresh: String = column(&state.db, project, "salesforce_refresh_enc").await;
        assert_eq!(crypto::decrypt(&state.cfg.app_secret, &refresh).unwrap(), "sf-refresh", "Salesforce's stays");

        // The new tokens are reused until they run out.
        crate::crm::sync(&state, project, &lead("ana@acme.test"), "Hot again.").await;
        assert_eq!(fake.token_requests().len(), 3);
        assert_eq!((fake.records("sf:Lead").len(), fake.records("zoho:Note").len()), (1, 2));

        // A sync whose token was turned down uses the one another sync saved since, without renewing again.
        let saved = super::session(&state, project, Crm::Zoho, None).await.unwrap().0.access;
        let (reused, renewed) = super::session(&state, project, Crm::Zoho, Some("stale")).await.unwrap();
        assert_eq!((reused.access, renewed, fake.token_requests().len()), (saved.clone(), false, 3));
        // Once that one is turned down too, it gets a new one.
        let (fresh, renewed) = super::session(&state, project, Crm::Zoho, Some(&saved)).await.unwrap();
        assert!(renewed && fresh.access != saved);
        assert_eq!(fake.token_requests().len(), 4);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_revoked_sign_in_asks_for_a_reconnect_and_is_not_sent_again(db: PgPool) {
        let fake = Fake::start().await;
        let state = fake.state(db);
        let project = project(&state.db, "Acme").await;
        for crm in SIGN_INS {
            connect_fake(&state, &fake, project, crm, "stale", "revoked", -10).await;
        }
        crate::crm::sync(&state, project, &lead("ana@acme.test"), "Hot.").await;
        assert_eq!(fake.token_requests().len(), 3);
        for crm in SIGN_INS {
            let n = crm.name();
            let error: Option<String> = column(&state.db, project, &format!("{n}_error")).await;
            assert_eq!(error.as_deref(), Some(signed_out(crm).as_str()), "{crm:?}");
            assert!(column::<bool>(&state.db, project, &format!("{n}_needs_reconnect")).await, "{crm:?}");
            assert!(column::<Option<Ts>>(&state.db, project, &format!("{n}_failed_at")).await.is_some());
        }
        crate::crm::sync(&state, project, &lead("ana@acme.test"), "Hot.").await;
        assert_eq!(fake.token_requests().len(), 3, "a revoked refresh token isn't sent again");
        assert!(crate::crm::test(&state, project, Crm::Zoho).await.unwrap_err().to_string().starts_with("Zoho CRM signed Splee out"));
        assert!(fake.records("sf:Lead").is_empty());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn testing_salesforce_reads_the_lead_statuses_again(db: PgPool) {
        let fake = Fake::start().await;
        let state = fake.state(db);
        let project = project(&state.db, "Acme").await;
        connect_fake(&state, &fake, project, Crm::Salesforce, LIVE, "good", 3600).await;
        sqlx::query("update project_settings set salesforce_booked_status = 'Old', salesforce_error = 'it broke', salesforce_failed_at = now() where project_id = $1")
            .bind(project)
            .execute(&state.db)
            .await
            .unwrap();
        crate::crm::test(&state, project, Crm::Salesforce).await.unwrap();
        let statuses: Value = column(&state.db, project, "salesforce_statuses").await;
        let values: Vec<&str> = statuses.as_array().unwrap().iter().map(|s| s["value"].as_str().unwrap()).collect();
        assert_eq!(values, ["Open - Not Contacted", "Working - Contacted", "Meeting Set"], "inactive values are left out");
        assert_eq!(statuses[2]["label"], "Meeting set");
        assert_eq!(column::<String>(&state.db, project, "salesforce_booked_status").await, "", "a status the org stopped using is let go");
        assert_eq!(column::<Option<String>>(&state.db, project, "salesforce_error").await, None, "a pass clears the last failure");

        sqlx::query("update project_settings set salesforce_booked_status = 'Meeting Set' where project_id = $1").bind(project).execute(&state.db).await.unwrap();
        crate::crm::test(&state, project, Crm::Salesforce).await.unwrap();
        assert_eq!(column::<String>(&state.db, project, "salesforce_booked_status").await, "Meeting Set");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_project_syncs_with_its_own_sign_in(db: PgPool) {
        let fake = Fake::start().await;
        let state = fake.state(db);
        let (acme, other) = (project(&state.db, "Acme").await, project(&state.db, "Other").await);
        fake.world.lock().unwrap().live.insert("other-token".into());
        connect_fake(&state, &fake, acme, Crm::Zoho, LIVE, "acme-refresh", 3600).await;
        connect_fake(&state, &fake, other, Crm::Zoho, "other-token", "other-refresh", 3600).await;
        crate::crm::sync(&state, acme, &lead("ana@acme.test"), "Hot.").await;
        let tokens: HashSet<String> = fake.requests().into_iter().map(|r| r.token).collect();
        assert_eq!(tokens, HashSet::from([LIVE.to_string()]));
        // A project without the CRM sends it nothing.
        let third = project(&state.db, "Third").await;
        crate::crm::sync(&state, third, &lead("ana@acme.test"), "Hot.").await;
        assert_eq!(fake.requests().len(), 2);
        // Disconnecting one project leaves the other connected.
        disconnect(&state.db, &state.cfg, acme, Crm::Zoho).await.unwrap();
        assert!(!column::<bool>(&state.db, acme, "zoho_connected").await);
        assert!(column::<bool>(&state.db, other, "zoho_connected").await);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn disconnecting_revokes_where_the_crm_can_and_forgets_the_sign_in(db: PgPool) {
        let fake = Fake::start().await;
        let state = fake.state(db);
        let project = project(&state.db, "Acme").await;
        for crm in SIGN_INS {
            connect_fake(&state, &fake, project, crm, LIVE, &format!("{}-refresh", crm.name()), 3600).await;
            disconnect(&state.db, &state.cfg, project, crm).await.unwrap();
        }
        let revokes: Vec<Seen> = fake.requests().into_iter().filter(|r| r.path.ends_with("/revoke")).collect();
        assert_eq!(revokes.len(), 2, "Microsoft has nothing to revoke one app's token with");
        assert_eq!((revokes[0].path.as_str(), revokes[0].body["token"].as_str()), ("/salesforce/services/oauth2/revoke", Some("salesforce-refresh")));
        assert_eq!((revokes[1].path.as_str(), revokes[1].query["token"].as_str()), ("/oauth/v2/token/revoke", "zoho-refresh"));
        for crm in SIGN_INS {
            let n = crm.name();
            assert!(!column::<bool>(&state.db, project, &format!("{n}_connected")).await);
            assert_eq!(column::<String>(&state.db, project, &format!("{n}_refresh_enc")).await, "");
            assert_eq!(column::<String>(&state.db, project, &format!("{n}_site")).await, "");
        }
    }
}
