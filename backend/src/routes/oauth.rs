//! Splee as the OAuth 2.1 authorization server of its own MCP server, so ChatGPT and Claude.ai connect with a sign-in instead
//! of an API key. It follows https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization: protected resource
//! metadata (RFC 9728) and authorization server metadata (RFC 8414) under /.well-known, which the frontend proxies from the
//! public origin, apps that register (RFC 7591) or name themselves by the URL of a client ID metadata document, the
//! authorization code flow with S256 PKCE and the `iss` response parameter (RFC 9207), tokens bound to the MCP server's URL
//! (RFC 8707), refresh tokens that rotate, and revocation (RFC 7009). The consent page is the frontend's /oauth/authorize.

use axum::extract::rejection::{FormRejection, JsonRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::auth::Auth;
use crate::config::Config;
use crate::crypto::{random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::models::Ts;
use crate::state::AppState;

const ACCESS_SECONDS: i64 = 3600;
const REFRESH_DAYS: i64 = 30;
const CODE_MINUTES: i64 = 5;
/// How long a client ID metadata document is trusted before it's read again.
const METADATA_MINUTES: i64 = 60;
const AUTH_METHODS: &[&str] = &["none", "client_secret_post", "client_secret_basic"];

pub fn router() -> Router<AppState> {
    Router::new()
        // Clients try RFC 9728's path for a resource at /api/mcp first, then the root one.
        .route("/.well-known/oauth-protected-resource", get(resource_metadata))
        .route("/.well-known/oauth-protected-resource/api/mcp", get(resource_metadata))
        .route("/.well-known/oauth-authorization-server", get(server_metadata))
        .route("/oauth/register", post(register))
        .route("/oauth/authorize", get(check).post(decide))
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
        .route("/organization/connected-apps", get(list_apps))
        .route("/organization/connected-apps/{id}", delete(disconnect))
}

fn issuer(cfg: &Config) -> String {
    cfg.public_url.trim_end_matches('/').to_string()
}

/// The MCP server's URL, which tokens are bound to.
pub fn resource(cfg: &Config) -> String {
    format!("{}/api/mcp", issuer(cfg))
}

pub fn resource_metadata_url(cfg: &Config) -> String {
    format!("{}/.well-known/oauth-protected-resource/api/mcp", issuer(cfg))
}

async fn resource_metadata(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "resource": resource(&state.cfg),
        "authorization_servers": [issuer(&state.cfg)],
        "bearer_methods_supported": ["header"],
        "resource_name": "Splee",
    }))
}

async fn server_metadata(State(state): State<AppState>) -> Json<Value> {
    let base = issuer(&state.cfg);
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/api/oauth/token"),
        "registration_endpoint": format!("{base}/api/oauth/register"),
        "revocation_endpoint": format!("{base}/api/oauth/revoke"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": AUTH_METHODS,
        "revocation_endpoint_auth_methods_supported": AUTH_METHODS,
        "client_id_metadata_document_supported": true,
        "authorization_response_iss_parameter_supported": true,
    }))
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], Json(json!({ "error": error, "error_description": description }))).into_response()
}

/// https anywhere, or http back to this computer.
fn valid_redirect(uri: &str) -> bool {
    let Ok(url) = Url::parse(uri) else { return false };
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    let scheme_ok = (url.scheme() == "https" && url.host_str().is_some()) || (url.scheme() == "http" && local);
    scheme_ok && uri.len() <= 2000 && url.fragment().is_none() && url.username().is_empty() && url.password().is_none()
}

/// Redirect URIs match exactly, except that a loopback one may name any port (RFC 8252 7.3). Claude Code's metadata document
/// lists http://localhost/callback and then signs in on whatever port it got.
fn registered(redirect_uris: &[String], uri: &str) -> bool {
    let without_port = |u: &str| {
        let mut url = Url::parse(u).ok().filter(|url| url.scheme() == "http")?;
        url.set_port(None).ok()?;
        Some(url)
    };
    redirect_uris.iter().any(|r| r == uri || (is_local(r) && without_port(r).is_some_and(|r| Some(r) == without_port(uri))))
}

fn is_local(uri: &str) -> bool {
    Url::parse(uri).is_ok_and(|u| u.scheme() == "http")
}

fn host(uri: &str) -> String {
    Url::parse(uri).map(|u| format!("{}{}", u.host_str().unwrap_or_default(), u.port().map(|p| format!(":{p}")).unwrap_or_default())).unwrap_or_default()
}

/// Scheme and host compare without case, which parsing takes care of, and a trailing slash doesn't matter.
fn same_resource(given: &str, ours: &str) -> bool {
    match (Url::parse(given), Url::parse(ours)) {
        (Ok(a), Ok(b)) => a.fragment().is_none() && a.as_str().trim_end_matches('/') == b.as_str().trim_end_matches('/'),
        _ => false,
    }
}

fn clip(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

#[derive(FromRow)]
struct Client {
    id: Uuid,
    client_id: String,
    name: String,
    redirect_uris: Vec<String>,
    secret_hash: Option<String>,
    fetched_at: Option<Ts>,
}

/// A client_id that is an https URL with a path names a client ID metadata document. Comparing with the parsed URL's own
/// spelling refuses dot segments and other ways to write one URL two ways.
fn metadata_url(client_id: &str) -> Option<Url> {
    let url = Url::parse(client_id).ok()?;
    let ok = url.scheme() == "https" && url.path() != "/" && url.fragment().is_none() && url.username().is_empty() && url.as_str() == client_id;
    ok.then_some(url)
}

async fn saved_client(db: &PgPool, client_id: &str) -> sqlx::Result<Option<Client>> {
    sqlx::query_as("select * from oauth_clients where client_id = $1").bind(client_id).fetch_optional(db).await
}

/// The app behind a client_id, reading its metadata document again when the saved copy is older than METADATA_MINUTES.
/// A refusal is AppError::BadRequest with what the consent page should say.
async fn find_client(db: &PgPool, client_id: &str) -> AppResult<Client> {
    let saved = saved_client(db, client_id).await?;
    let Some(url) = metadata_url(client_id) else {
        return saved.ok_or_else(|| AppError::BadRequest("Splee doesn't know this app. Start connecting it again from the app.".into()));
    };
    if let Some(client) = saved.as_ref()
        && client.fetched_at.is_some_and(|at| Utc::now() - at < Duration::minutes(METADATA_MINUTES))
    {
        return Ok(saved.expect("checked above"));
    }
    match fetch_metadata(&url).await {
        Ok((name, redirect_uris)) => Ok(sqlx::query_as(
            "insert into oauth_clients (client_id, name, redirect_uris, fetched_at) values ($1, $2, $3, now())
             on conflict (client_id) do update set name = excluded.name, redirect_uris = excluded.redirect_uris, fetched_at = now()
             returning *",
        )
        .bind(client_id)
        .bind(name)
        .bind(redirect_uris)
        .fetch_one(db)
        .await?),
        Err(e) => saved.ok_or_else(|| AppError::BadRequest(format!("Couldn't read this app's details from {url}: {e}"))),
    }
}

async fn fetch_metadata(url: &Url) -> anyhow::Result<(String, Vec<String>)> {
    // The URL comes from whoever starts a sign-in, so it may only reach public addresses.
    let client = hooks::public_client(url).await?;
    let request = client.get(url.clone()).header(header::ACCEPT, "application/json").timeout(std::time::Duration::from_secs(5));
    let mut res = request.send().await?.error_for_status()?;
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await? {
        body.extend_from_slice(&chunk);
        anyhow::ensure!(body.len() <= 64 * 1024, "the document is over 64 KB");
    }
    read_metadata(url.as_str(), &serde_json::from_slice(&body)?)
}

/// The name and the redirect URIs Splee accepts from a client ID metadata document.
fn read_metadata(client_id: &str, doc: &Value) -> anyhow::Result<(String, Vec<String>)> {
    anyhow::ensure!(doc["client_id"] == client_id, "its client_id isn't its own URL");
    let name = doc["client_name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| anyhow::anyhow!("it has no client_name"))?;
    // The document is public, so a secret in it would be no secret.
    let method = doc["token_endpoint_auth_method"].as_str();
    anyhow::ensure!(!matches!(method, Some("client_secret_post" | "client_secret_basic")), "it asks for a client secret");
    let redirect_uris: Vec<String> =
        doc["redirect_uris"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|u| valid_redirect(u)).map(String::from).take(20).collect();
    anyhow::ensure!(!redirect_uris.is_empty(), "it has no https or localhost redirect_uris");
    Ok((clip(name, 100), redirect_uris))
}

#[derive(Deserialize)]
struct Registration {
    redirect_uris: Option<Vec<String>>,
    client_name: Option<String>,
    token_endpoint_auth_method: Option<String>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
}

/// Dynamic client registration (RFC 7591), which the 2026-07-28 revision keeps for apps that don't publish a metadata document.
async fn register(State(state): State<AppState>, body: Result<Json<Registration>, JsonRejection>) -> Response {
    let Ok(Json(reg)) = body else { return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata", "Send the metadata as JSON") };
    let redirect_uris = reg.redirect_uris.unwrap_or_default();
    if redirect_uris.is_empty() || redirect_uris.len() > 10 || !redirect_uris.iter().all(|u| valid_redirect(u)) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_redirect_uri", "Send 1 to 10 redirect_uris, each https or http://localhost");
    }
    // RFC 7591 makes client_secret_basic the default.
    let method = reg.token_endpoint_auth_method.unwrap_or_else(|| "client_secret_basic".into());
    let grant_types = reg.grant_types.unwrap_or_else(|| vec!["authorization_code".into()]);
    let response_types = reg.response_types.unwrap_or_else(|| vec!["code".into()]);
    let known_grants = grant_types.iter().all(|g| g == "authorization_code" || g == "refresh_token");
    if !AUTH_METHODS.contains(&method.as_str()) || !known_grants || response_types.iter().any(|r| r != "code") {
        let text = "Splee supports token_endpoint_auth_method none, client_secret_post or client_secret_basic, the authorization_code and \
                    refresh_token grants and response type code";
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata", text);
    }
    let name = clip(reg.client_name.as_deref().unwrap_or_default(), 100);
    let name = if name.is_empty() { "Unnamed app".to_string() } else { name };
    let client_id = Uuid::new_v4().to_string();
    let secret = (method != "none").then(random_token);
    let saved = sqlx::query("insert into oauth_clients (client_id, name, redirect_uris, secret_hash) values ($1, $2, $3, $4)")
        .bind(&client_id)
        .bind(&name)
        .bind(&redirect_uris)
        .bind(secret.as_deref().map(sha256_hex))
        .execute(&state.db)
        .await;
    if let Err(e) = saved {
        return AppError::from(e).into_response();
    }
    let mut body = json!({
        "client_id": client_id,
        "client_id_issued_at": Utc::now().timestamp(),
        "client_name": name,
        "redirect_uris": redirect_uris,
        "token_endpoint_auth_method": method,
        "grant_types": grant_types,
        "response_types": response_types,
    });
    if let Some(secret) = secret {
        body["client_secret"] = json!(secret);
        body["client_secret_expires_at"] = json!(0);
    }
    (StatusCode::CREATED, [(header::CACHE_CONTROL, "no-store")], Json(body)).into_response()
}

#[derive(Deserialize)]
struct AuthorizeParams {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    resource: Option<String>,
}

struct Valid {
    client: Client,
    redirect_uri: String,
    challenge: String,
    resource: String,
}

/// Why an authorization request stops. Until the app and its redirect URI check out, sending the browser there could make Splee
/// an open redirect, so those refusals are shown on the consent page. Later ones go back to the app as OAuth errors.
enum Refusal {
    Page(String),
    Redirect(String),
    Failed(AppError),
}

impl From<AppError> for Refusal {
    fn from(e: AppError) -> Self {
        match e {
            AppError::BadRequest(text) => Refusal::Page(text),
            other => Refusal::Failed(other),
        }
    }
}

/// The app's redirect URI with the answer, the state it sent and `iss`, which tells the app it was Splee that answered (RFC 9207).
fn back(cfg: &Config, redirect_uri: &str, state: Option<&str>, answer: &[(&str, &str)]) -> String {
    let mut url = Url::parse(redirect_uri).expect("redirect URIs are checked when they're saved");
    {
        let mut query = url.query_pairs_mut();
        query.extend_pairs(answer);
        if let Some(state) = state {
            query.append_pair("state", state);
        }
        query.append_pair("iss", &issuer(cfg));
    }
    url.into()
}

async fn validate(state: &AppState, p: &AuthorizeParams) -> Result<Valid, Refusal> {
    let client = find_client(&state.db, p.client_id.as_deref().unwrap_or_default()).await?;
    let redirect_uri = match &p.redirect_uri {
        Some(uri) if registered(&client.redirect_uris, uri) => uri.clone(),
        None if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
        _ => return Err(Refusal::Page("This app asked to send you to an address it didn't register, so Splee stopped here.".into())),
    };
    let refuse = |error: &str, description: &str| {
        Refusal::Redirect(back(&state.cfg, &redirect_uri, p.state.as_deref(), &[("error", error), ("error_description", description)]))
    };
    if p.response_type.as_deref() != Some("code") {
        return Err(refuse("unsupported_response_type", "Splee only supports response_type=code"));
    }
    let challenge = p.code_challenge.clone().unwrap_or_default();
    let base64url = challenge.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if p.code_challenge_method.as_deref() != Some("S256") || challenge.len() != 43 || !base64url {
        return Err(refuse("invalid_request", "Send a code_challenge with code_challenge_method=S256"));
    }
    let ours = resource(&state.cfg);
    // Clients from before resource indicators leave it out, and the MCP server is the only resource there is.
    if p.resource.as_deref().is_some_and(|r| !same_resource(r, &ours)) {
        return Err(refuse("invalid_target", "Splee only issues tokens for its MCP server"));
    }
    Ok(Valid { client, redirect_uri, challenge, resource: ours })
}

/// What the consent page shows, or where to send the browser for a request that went wrong.
async fn check(State(state): State<AppState>, _auth: Auth, Query(p): Query<AuthorizeParams>) -> AppResult<Json<Value>> {
    match validate(&state, &p).await {
        Ok(v) => Ok(Json(json!({
            "client_name": v.client.name,
            "client_host": metadata_url(&v.client.client_id).map(|u| host(u.as_str())),
            "redirect_host": host(&v.redirect_uri),
            "local": is_local(&v.redirect_uri),
        }))),
        Err(Refusal::Redirect(url)) => Ok(Json(json!({ "redirect": url }))),
        Err(Refusal::Page(text)) => Err(AppError::BadRequest(text)),
        Err(Refusal::Failed(e)) => Err(e),
    }
}

#[derive(Deserialize)]
struct Decision {
    #[serde(flatten)]
    params: AuthorizeParams,
    allow: bool,
    /// One of the user's organizations. The session's by default.
    org_id: Option<Uuid>,
}

async fn decide(State(state): State<AppState>, auth: Auth, Json(d): Json<Decision>) -> AppResult<Json<Value>> {
    let v = match validate(&state, &d.params).await {
        Ok(v) => v,
        Err(Refusal::Redirect(url)) => return Ok(Json(json!({ "redirect": url }))),
        Err(Refusal::Page(text)) => return Err(AppError::BadRequest(text)),
        Err(Refusal::Failed(e)) => return Err(e),
    };
    let app_state = d.params.state.as_deref();
    if !d.allow {
        let denied = [("error", "access_denied"), ("error_description", "The user didn't allow it")];
        return Ok(Json(json!({ "redirect": back(&state.cfg, &v.redirect_uri, app_state, &denied) })));
    }
    let org_id = d.org_id.unwrap_or(auth.org_id);
    let member: bool = sqlx::query_scalar("select exists (select 1 from memberships where user_id = $1 and org_id = $2)")
        .bind(auth.user_id)
        .bind(org_id)
        .fetch_one(&state.db)
        .await?;
    if !member {
        return Err(AppError::Forbidden("You're not in that organization".into()));
    }
    let code = random_token();
    sqlx::query(
        "insert into oauth_codes (code_hash, client_id, user_id, org_id, redirect_uri, code_challenge, resource, expires_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(sha256_hex(&code))
    .bind(v.client.id)
    .bind(auth.user_id)
    .bind(org_id)
    .bind(&v.redirect_uri)
    .bind(&v.challenge)
    .bind(&v.resource)
    .bind(Utc::now() + Duration::minutes(CODE_MINUTES))
    .execute(&state.db)
    .await?;
    Ok(Json(json!({ "redirect": back(&state.cfg, &v.redirect_uri, app_state, &[("code", &code)]) })))
}

/// The token and revocation endpoints' form.
#[derive(Deserialize)]
struct TokenParams {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
    resource: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    token: Option<String>,
}

/// The app from client_secret_basic, or from client_id and client_secret in the form. An app registered with a secret has to send it.
async fn authenticate_client(db: &PgPool, headers: &HeaderMap, p: &TokenParams) -> Result<Client, Response> {
    let basic = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Basic "));
    // RFC 6749 form-encodes both halves, which changes nothing for Splee's ids and secrets: hex digits and dashes.
    let (client_id, secret) = match basic.and_then(|b| STANDARD.decode(b.trim()).ok()).and_then(|b| String::from_utf8(b).ok()) {
        Some(pair) => match pair.split_once(':') {
            Some((id, secret)) => (Some(id.to_string()), Some(secret.to_string())),
            None => (None, None),
        },
        None => (p.client_id.clone(), p.client_secret.clone()),
    };
    let refused = || oauth_error(StatusCode::UNAUTHORIZED, "invalid_client", "Unknown client_id or wrong client_secret");
    let client = match saved_client(db, client_id.as_deref().unwrap_or_default()).await {
        Ok(Some(client)) => client,
        Ok(None) => return Err(refused()),
        Err(e) => return Err(AppError::from(e).into_response()),
    };
    match (&client.secret_hash, secret) {
        (Some(hash), Some(secret)) if *hash == sha256_hex(&secret) => Ok(client),
        (Some(_), _) => Err(refused()),
        (None, _) => Ok(client),
    }
}

fn form_error(_: FormRejection) -> Response {
    oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "Send the parameters as application/x-www-form-urlencoded")
}

/// An OAuth error code and description for the client.
type Grant = Result<Value, (&'static str, &'static str)>;

async fn token(State(state): State<AppState>, headers: HeaderMap, form: Result<Form<TokenParams>, FormRejection>) -> Response {
    let Form(p) = match form {
        Ok(form) => form,
        Err(e) => return form_error(e),
    };
    let client = match authenticate_client(&state.db, &headers, &p).await {
        Ok(client) => client,
        Err(res) => return res,
    };
    let outcome = match p.grant_type.as_deref() {
        Some("authorization_code") => exchange_code(&state, &client, &p).await,
        Some("refresh_token") => refresh(&state, &client, &p).await,
        _ => Ok(Err(("unsupported_grant_type", "Splee supports authorization_code and refresh_token"))),
    };
    match outcome {
        Ok(Ok(tokens)) => ([(header::CACHE_CONTROL, "no-store")], Json(tokens)).into_response(),
        Ok(Err((error, description))) => oauth_error(StatusCode::BAD_REQUEST, error, description),
        Err(e) => e.into_response(),
    }
}

#[derive(FromRow)]
struct Code {
    client_id: Uuid,
    user_id: Uuid,
    org_id: Uuid,
    redirect_uri: String,
    code_challenge: String,
    resource: String,
    grant_id: Option<Uuid>,
    expires_at: Ts,
}

fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    let unreserved = verifier.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b));
    (43..=128).contains(&verifier.len()) && unreserved && URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == challenge
}

async fn exchange_code(state: &AppState, client: &Client, p: &TokenParams) -> AppResult<Grant> {
    let mut tx = state.db.begin().await?;
    let code: Option<Code> = sqlx::query_as("select * from oauth_codes where code_hash = $1 for update")
        .bind(sha256_hex(p.code.as_deref().unwrap_or_default()))
        .fetch_optional(&mut *tx)
        .await?;
    let Some(code) = code.filter(|c| c.client_id == client.id) else { return Ok(Err(("invalid_grant", "The code is wrong or expired"))) };
    if p.redirect_uri.as_deref().is_some_and(|uri| uri != code.redirect_uri) {
        return Ok(Err(("invalid_grant", "redirect_uri isn't the one the code was issued for")));
    }
    if !pkce_matches(p.code_verifier.as_deref().unwrap_or_default(), &code.code_challenge) {
        return Ok(Err(("invalid_grant", "code_verifier doesn't match the code_challenge")));
    }
    // Checked after PKCE, so only someone holding the verifier can end the grant by replaying the code.
    if let Some(grant) = code.grant_id {
        // A code used twice was likely stolen, so the tokens from its first use stop working too.
        revoke_grant(&mut tx, grant).await?;
        tx.commit().await?;
        return Ok(Err(("invalid_grant", "The code was already used")));
    }
    if code.expires_at < Utc::now() {
        return Ok(Err(("invalid_grant", "The code is wrong or expired")));
    }
    if p.resource.as_deref().is_some_and(|r| !same_resource(r, &code.resource)) {
        return Ok(Err(("invalid_target", "resource isn't the one the code was issued for")));
    }
    let grant: Uuid = sqlx::query_scalar("insert into oauth_grants (client_id, user_id, org_id, resource) values ($1, $2, $3, $4) returning id")
        .bind(client.id)
        .bind(code.user_id)
        .bind(code.org_id)
        .bind(&code.resource)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("update oauth_codes set grant_id = $2 where code_hash = $1")
        .bind(sha256_hex(p.code.as_deref().unwrap_or_default()))
        .bind(grant)
        .execute(&mut *tx)
        .await?;
    let tokens = issue(&mut tx, grant).await?;
    tx.commit().await?;
    Ok(Ok(tokens))
}

#[derive(FromRow)]
struct RefreshToken {
    grant_id: Uuid,
    client_id: Uuid,
    resource: String,
    expires_at: Ts,
    used_at: Option<Ts>,
    revoked_at: Option<Ts>,
    member: bool,
}

/// Trades a refresh token for a new pair. Each one works once, and using one twice means two parties hold it, so the grant ends.
async fn refresh(state: &AppState, client: &Client, p: &TokenParams) -> AppResult<Grant> {
    let mut tx = state.db.begin().await?;
    let token: Option<RefreshToken> = sqlx::query_as(
        "select t.grant_id, g.client_id, g.resource, t.expires_at, t.used_at, g.revoked_at,
                exists (select 1 from memberships m where m.user_id = g.user_id and m.org_id = g.org_id) as member
         from oauth_tokens t join oauth_grants g on g.id = t.grant_id
         where t.token_hash = $1 and t.kind = 'refresh' for update of t",
    )
    .bind(sha256_hex(p.refresh_token.as_deref().unwrap_or_default()))
    .fetch_optional(&mut *tx)
    .await?;
    let invalid = Ok(Err(("invalid_grant", "The refresh token is wrong, expired or revoked")));
    let Some(token) = token.filter(|t| t.client_id == client.id && t.revoked_at.is_none()) else { return invalid };
    if token.used_at.is_some() {
        revoke_grant(&mut tx, token.grant_id).await?;
        tx.commit().await?;
        return invalid;
    }
    if token.expires_at < Utc::now() || !token.member {
        return invalid;
    }
    if p.resource.as_deref().is_some_and(|r| !same_resource(r, &token.resource)) {
        return Ok(Err(("invalid_target", "resource isn't the one the token was issued for")));
    }
    sqlx::query("update oauth_tokens set used_at = now() where token_hash = $1")
        .bind(sha256_hex(p.refresh_token.as_deref().unwrap_or_default()))
        .execute(&mut *tx)
        .await?;
    let tokens = issue(&mut tx, token.grant_id).await?;
    tx.commit().await?;
    Ok(Ok(tokens))
}

async fn issue(db: &mut PgConnection, grant: Uuid) -> AppResult<Value> {
    let access = format!("splee_at_{}", random_token());
    let refresh = format!("splee_rt_{}", random_token());
    sqlx::query("insert into oauth_tokens (token_hash, grant_id, kind, expires_at) values ($1, $3, 'access', $4), ($2, $3, 'refresh', $5)")
        .bind(sha256_hex(&access))
        .bind(sha256_hex(&refresh))
        .bind(grant)
        .bind(Utc::now() + Duration::seconds(ACCESS_SECONDS))
        .bind(Utc::now() + Duration::days(REFRESH_DAYS))
        .execute(db)
        .await?;
    Ok(json!({ "access_token": access, "token_type": "Bearer", "expires_in": ACCESS_SECONDS, "refresh_token": refresh }))
}

async fn revoke_grant(db: &mut PgConnection, grant: Uuid) -> sqlx::Result<()> {
    sqlx::query("update oauth_grants set revoked_at = now() where id = $1 and revoked_at is null").bind(grant).execute(db).await.map(|_| ())
}

/// RFC 7009. Either token ends the whole grant, since an app revokes one when it disconnects. Unknown tokens get 200 too.
async fn revoke(State(state): State<AppState>, headers: HeaderMap, form: Result<Form<TokenParams>, FormRejection>) -> Response {
    let Form(p) = match form {
        Ok(form) => form,
        Err(e) => return form_error(e),
    };
    let client = match authenticate_client(&state.db, &headers, &p).await {
        Ok(client) => client,
        Err(res) => return res,
    };
    let Some(token) = p.token else { return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "Send the token to revoke") };
    let revoked = sqlx::query(
        "update oauth_grants g set revoked_at = now() from oauth_tokens t
         where t.token_hash = $1 and t.grant_id = g.id and g.client_id = $2 and g.revoked_at is null",
    )
    .bind(sha256_hex(&token))
    .bind(client.id)
    .execute(&state.db)
    .await;
    match revoked {
        Ok(_) => StatusCode::OK.into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// The organization an access token acts for, when it's live, was issued for `resource`, its grant isn't revoked and the user who
/// allowed it is still in the organization. A token can then do what a member can, like an API key.
pub async fn token_org(db: &PgPool, token: &str, resource: &str) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar(
        "update oauth_grants g set last_used_at = now() from oauth_tokens t, memberships m
         where t.token_hash = $1 and t.kind = 'access' and t.expires_at > now() and g.id = t.grant_id and g.revoked_at is null
           and g.resource = $2 and m.user_id = g.user_id and m.org_id = g.org_id
         returning g.org_id",
    )
    .bind(sha256_hex(token))
    .bind(resource)
    .fetch_optional(db)
    .await
}

#[derive(Serialize, FromRow)]
struct ConnectedApp {
    id: Uuid,
    name: String,
    client_id: String,
    user_name: String,
    user_email: String,
    mine: bool,
    created_at: Ts,
    last_used_at: Option<Ts>,
}

/// Grants that can still get tokens: not revoked, with a refresh token that hasn't been used or run out.
async fn list_apps(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<ConnectedApp>>> {
    let apps = sqlx::query_as(
        "select g.id, c.name, c.client_id, u.name as user_name, u.email::text as user_email, g.user_id = $2 as mine, g.created_at, g.last_used_at
         from oauth_grants g join oauth_clients c on c.id = g.client_id join users u on u.id = g.user_id
         where g.org_id = $1 and g.revoked_at is null
           and exists (select 1 from oauth_tokens t where t.grant_id = g.id and t.kind = 'refresh' and t.used_at is null and t.expires_at > now())
         order by g.created_at desc",
    )
    .bind(auth.org_id)
    .bind(auth.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(apps))
}

/// Admins can disconnect any app, members the ones they connected.
async fn disconnect(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let revoked = sqlx::query(
        "update oauth_grants set revoked_at = now() where id = $1 and org_id = $2 and revoked_at is null and ($3 or user_id = $4)",
    )
    .bind(id)
    .bind(auth.org_id)
    .bind(auth.require_admin().is_ok())
    .bind(auth.user_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if revoked == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use reqwest::Url;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use sqlx::PgPool;
    use uuid::Uuid;

    use crate::config::Config;
    use crate::crypto::sha256_hex;
    use crate::state::AppState;

    const PUBLIC: &str = "http://localhost:3346";
    const MCP: &str = "http://localhost:3346/api/mcp";
    const REDIRECT: &str = "http://127.0.0.1:7777/callback";
    const VERIFIER: &str = "a-long-random-verifier-that-only-the-client-knows-0123456789";

    fn challenge(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }

    struct Server {
        base: String,
        client: reqwest::Client,
        db: PgPool,
        other_project: Uuid,
    }

    type Reply = (u16, Value);

    impl Server {
        /// Ana is the owner of Acme and signed in to it. Other is an organization she isn't in. Each has a project.
        async fn start(db: PgPool) -> Self {
            let ana: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('ana@acme.test', 'Ana', 'x') returning id")
                .fetch_one(&db)
                .await
                .unwrap();
            let mut projects = vec![];
            for name in ["Acme", "Other"] {
                let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap();
                let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id")
                    .bind(org)
                    .bind(format!("{name} project"))
                    .fetch_one(&db)
                    .await
                    .unwrap();
                projects.push(project);
                if name == "Acme" {
                    sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(ana).execute(&db).await.unwrap();
                    sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                        .bind(sha256_hex("session-ana"))
                        .bind(ana)
                        .bind(org)
                        .execute(&db)
                        .await
                        .unwrap();
                }
            }
            let cfg = Config {
                database_url: String::new(),
                app_secret: String::new(),
                public_url: format!("{PUBLIC}/"),
                unsubscribe_url: String::new(),
                bind_addr: String::new(),
                agent_model: String::new(),
                agent_concurrency: 0,
                sending_enabled: false,
                bird_bin: None,
                open_signup: true,
                public_agent: false,
                porkbun_api_url: String::new(),
                twilio_api_url: String::new(),
                bots_api_url: String::new(),
                outreach_api_url: String::new(),
                postmaster_api_url: String::new(),
                operator_emails: Vec::new(),
                google: None,
                microsoft: None,
                slack: None,
                tumblr: None,
                salesforce: None,
                zoho: None,
                meta: None,
                graph_api_url: String::new(),
                crm_api_url: String::new(),
                payments_api_url: String::new(),
                ipinfo_token: None,
            };
            let state = AppState { db: db.clone(), cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(16).0 };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", crate::routes::router()).with_state(state);
            tokio::spawn(async move { axum::serve(listener, app).await });
            Self { base, client: reqwest::Client::new(), db, other_project: projects[1] }
        }

        async fn reply(res: reqwest::RequestBuilder) -> Reply {
            let res = res.send().await.unwrap();
            (res.status().as_u16(), res.json().await.unwrap_or(Value::Null))
        }

        async fn get(&self, path: &str) -> Reply {
            Self::reply(self.client.get(format!("{}{path}", self.base)).header("cookie", "splee_session=session-ana")).await
        }

        async fn register(&self, body: Value) -> Reply {
            Self::reply(self.client.post(format!("{}/oauth/register", self.base)).json(&body)).await
        }

        async fn public_client(&self) -> String {
            let (status, body) = self.register(json!({ "client_name": "Test app", "redirect_uris": [REDIRECT], "token_endpoint_auth_method": "none" })).await;
            assert_eq!(status, 201, "{body}");
            body["client_id"].as_str().unwrap().to_string()
        }

        fn authorize_params(client_id: &str) -> Value {
            json!({ "response_type": "code", "client_id": client_id, "redirect_uri": REDIRECT, "state": "xyz",
                    "code_challenge": challenge(VERIFIER), "code_challenge_method": "S256", "resource": MCP })
        }

        async fn check(&self, params: &Value) -> Reply {
            let query: Vec<(String, String)> = params.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
            let url = Url::parse_with_params(&format!("{}/oauth/authorize", self.base), &query).unwrap();
            Self::reply(self.client.get(url).header("cookie", "splee_session=session-ana")).await
        }

        async fn decide(&self, params: &Value, allow: bool) -> Reply {
            let mut body = params.clone();
            body["allow"] = json!(allow);
            Self::reply(self.client.post(format!("{}/oauth/authorize", self.base)).json(&body).header("cookie", "splee_session=session-ana")).await
        }

        /// Allows the app and returns the query of the redirect back to it.
        async fn allow(&self, params: &Value) -> Vec<(String, String)> {
            let (status, body) = self.decide(params, true).await;
            assert_eq!(status, 200, "{body}");
            let url = Url::parse(body["redirect"].as_str().unwrap()).unwrap();
            assert!(url.as_str().starts_with(REDIRECT));
            url.query_pairs().into_owned().collect()
        }

        async fn code(&self, client_id: &str) -> String {
            let query = self.allow(&Self::authorize_params(client_id)).await;
            query.iter().find(|(k, _)| k == "code").unwrap().1.clone()
        }

        async fn token(&self, form: &[(&str, &str)]) -> Reply {
            Self::reply(self.client.post(format!("{}/oauth/token", self.base)).form(form)).await
        }

        async fn exchange(&self, client_id: &str, code: &str) -> Reply {
            let form = [("grant_type", "authorization_code"), ("code", code), ("redirect_uri", REDIRECT), ("code_verifier", VERIFIER),
                        ("client_id", client_id), ("resource", MCP)];
            self.token(&form).await
        }

        /// A new grant's access and refresh tokens.
        async fn connect(&self, client_id: &str) -> (String, String) {
            let (status, body) = self.exchange(client_id, &self.code(client_id).await).await;
            assert_eq!(status, 200, "{body}");
            (body["access_token"].as_str().unwrap().into(), body["refresh_token"].as_str().unwrap().into())
        }

        async fn refresh(&self, client_id: &str, refresh: &str) -> Reply {
            self.token(&[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", client_id), ("resource", MCP)]).await
        }

        async fn call(&self, token: &str, tool: &str, arguments: Value) -> Reply {
            let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": tool, "arguments": arguments } });
            Self::reply(self.client.post(format!("{}/mcp", self.base)).bearer_auth(token).json(&body)).await
        }

        async fn works(&self, token: &str) -> bool {
            let (status, body) = self.call(token, "list_projects", json!({})).await;
            status == 200 && body["result"]["content"][0]["text"].as_str().unwrap().contains("Acme project")
        }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn metadata_points_clients_at_splee(db: PgPool) {
        let server = Server::start(db).await;
        let (status, doc) = server.get("/.well-known/oauth-protected-resource/api/mcp").await;
        assert_eq!(status, 200);
        assert_eq!(doc["resource"], MCP);
        assert_eq!(doc["authorization_servers"], json!([PUBLIC]));
        assert_eq!(server.get("/.well-known/oauth-protected-resource").await.1, doc);

        let (status, doc) = server.get("/.well-known/oauth-authorization-server").await;
        assert_eq!(status, 200);
        assert_eq!(doc["issuer"], PUBLIC);
        assert_eq!(doc["authorization_endpoint"], format!("{PUBLIC}/oauth/authorize"));
        assert_eq!(doc["token_endpoint"], format!("{PUBLIC}/api/oauth/token"));
        assert_eq!(doc["registration_endpoint"], format!("{PUBLIC}/api/oauth/register"));
        assert_eq!(doc["revocation_endpoint"], format!("{PUBLIC}/api/oauth/revoke"));
        assert_eq!(doc["code_challenge_methods_supported"], json!(["S256"]));
        assert_eq!(doc["client_id_metadata_document_supported"], true);
        assert_eq!(doc["authorization_response_iss_parameter_supported"], true);

        // The MCP server's 401 says where the metadata is.
        let res = server.client.post(format!("{}/mcp", server.base)).json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).send().await.unwrap();
        assert_eq!(res.status(), 401);
        assert_eq!(res.headers()["www-authenticate"], format!("Bearer resource_metadata=\"{PUBLIC}/.well-known/oauth-protected-resource/api/mcp\""));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn registration_checks_redirect_uris(db: PgPool) {
        let server = Server::start(db).await;
        let (status, body) = server.register(json!({ "client_name": "Claude Code", "redirect_uris": ["http://localhost:51234/callback"],
            "token_endpoint_auth_method": "none", "grant_types": ["authorization_code", "refresh_token"] })).await;
        assert_eq!(status, 201, "{body}");
        assert!(Uuid::parse_str(body["client_id"].as_str().unwrap()).is_ok());
        assert!(body.get("client_secret").is_none());
        assert_eq!(body["grant_types"], json!(["authorization_code", "refresh_token"]));

        for uri in ["http://evil.example/cb", "javascript:alert(1)", "https://app.example/cb#frag", "cursor://callback", "not a url"] {
            let (status, body) = server.register(json!({ "redirect_uris": [uri], "token_endpoint_auth_method": "none" })).await;
            assert_eq!((status, body["error"].as_str()), (400, Some("invalid_redirect_uri")), "{uri}");
        }
        let (status, body) = server.register(json!({ "redirect_uris": [REDIRECT], "token_endpoint_auth_method": "private_key_jwt" })).await;
        assert_eq!((status, body["error"].as_str()), (400, Some("invalid_client_metadata")));
        let (status, _) = server.register(json!({ "redirect_uris": [], "token_endpoint_auth_method": "none" })).await;
        assert_eq!(status, 400);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn an_app_connects_and_acts_for_the_organization(db: PgPool) {
        let server = Server::start(db).await;
        let client_id = server.public_client().await;
        let params = Server::authorize_params(&client_id);

        // Signed out, the page sends people to sign in first.
        let url = Url::parse_with_params(&format!("{}/oauth/authorize", server.base), [("client_id", &client_id)]).unwrap();
        let res = server.client.get(url).send().await.unwrap();
        assert_eq!(res.status(), 401);

        let (status, body) = server.check(&params).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body, json!({ "client_name": "Test app", "client_host": null, "redirect_host": "127.0.0.1:7777", "local": true }));

        let query = server.allow(&params).await;
        let get = |key: &str| query.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        assert_eq!((get("state").as_deref(), get("iss").as_deref()), (Some("xyz"), Some(PUBLIC)));
        let (status, tokens) = server.exchange(&client_id, &get("code").unwrap()).await;
        assert_eq!(status, 200, "{tokens}");
        assert_eq!((tokens["token_type"].as_str(), tokens["expires_in"].as_i64()), (Some("Bearer"), Some(3600)));
        let access = tokens["access_token"].as_str().unwrap();
        let stored: i64 = sqlx::query_scalar("select count(*) from oauth_tokens where token_hash = $1").bind(sha256_hex(access)).fetch_one(&server.db).await.unwrap();
        assert_eq!(stored, 1, "tokens are stored hashed");

        let (status, body) = server.call(access, "list_projects", json!({})).await;
        assert_eq!(status, 200);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Acme project") && !text.contains("Other project"), "{text}");
        // Another organization's ids look like ids that don't exist.
        let (_, body) = server.call(access, "list_campaigns", json!({ "project_id": server.other_project })).await;
        assert_eq!(body["result"]["content"][0]["text"], "Not found");
        // The token is for the MCP server only, not the API.
        let res = server.client.get(format!("{}/v1/projects", server.base)).bearer_auth(access).send().await.unwrap();
        assert_eq!(res.status(), 401);

        let (_, apps) = server.get("/organization/connected-apps").await;
        assert_eq!((apps[0]["name"].as_str(), apps[0]["mine"].as_bool()), (Some("Test app"), Some(true)));
        assert!(apps[0]["last_used_at"].is_string());

        // Refreshing rotates the refresh token, and the old one then works never again.
        let refresh = tokens["refresh_token"].as_str().unwrap();
        let (status, rotated) = server.refresh(&client_id, refresh).await;
        assert_eq!(status, 200, "{rotated}");
        let (access2, refresh2) = (rotated["access_token"].as_str().unwrap(), rotated["refresh_token"].as_str().unwrap());
        assert_ne!(refresh2, refresh);
        assert!(server.works(access2).await);
        let (status, body) = server.refresh(&client_id, refresh).await;
        assert_eq!((status, body["error"].as_str()), (400, Some("invalid_grant")));
        // Reusing it means someone else has it, so the whole family goes.
        assert!(!server.works(access2).await);
        assert_eq!(server.refresh(&client_id, refresh2).await.0, 400);
        assert!(server.get("/organization/connected-apps").await.1.as_array().unwrap().is_empty());

        // Revoking a token disconnects the app.
        let (access, refresh) = server.connect(&client_id).await;
        assert!(server.works(&access).await);
        let res = server.client.post(format!("{}/oauth/revoke", server.base)).form(&[("token", &refresh), ("client_id", &client_id)]).send().await.unwrap();
        assert_eq!(res.status(), 200);
        assert!(!server.works(&access).await);
        assert_eq!(server.refresh(&client_id, &refresh).await.0, 400);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn bad_requests_are_refused(db: PgPool) {
        let server = Server::start(db).await;
        let client_id = server.public_client().await;
        let params = Server::authorize_params(&client_id);
        let with = |key: &str, value: &str| {
            let mut p = params.clone();
            p[key] = json!(value);
            p
        };

        // Without a known app and redirect URI there's nowhere safe to send the browser, so the page says what's wrong.
        let (status, body) = server.check(&with("client_id", "nobody")).await;
        assert_eq!((status, body["error"].as_str().unwrap().contains("doesn't know this app")), (400, true));
        let (status, body) = server.check(&with("redirect_uri", "https://evil.example/callback")).await;
        assert_eq!((status, body["error"].as_str().unwrap().contains("didn't register")), (400, true));
        assert_eq!(server.decide(&with("redirect_uri", "http://127.0.0.1:7777/other"), true).await.0, 400);

        // Later mistakes go back to the app as OAuth errors, with state and iss.
        for (key, value, error) in [
            ("code_challenge_method", "plain", "invalid_request"),
            ("code_challenge", "short", "invalid_request"),
            ("response_type", "token", "unsupported_response_type"),
            ("resource", "https://other.example/mcp", "invalid_target"),
        ] {
            let (status, body) = server.check(&with(key, value)).await;
            assert_eq!(status, 200);
            let url = Url::parse(body["redirect"].as_str().unwrap()).unwrap();
            let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
            assert!(url.as_str().starts_with(REDIRECT));
            assert!(query.contains(&("error".into(), error.into())), "{key}: {query:?}");
            assert!(query.contains(&("state".into(), "xyz".into())) && query.contains(&("iss".into(), PUBLIC.into())));
        }
        let (_, body) = server.decide(&params, false).await;
        assert!(body["redirect"].as_str().unwrap().contains("error=access_denied"));
        // A resource that differs only in case and a trailing slash is the same one.
        assert!(server.check(&with("resource", "HTTP://LOCALHOST:3346/api/mcp/")).await.1["client_name"].is_string());

        // A wrong verifier or redirect URI doesn't get tokens, and doesn't burn the code for the real client.
        let code = server.code(&client_id).await;
        let base = [("grant_type", "authorization_code"), ("code", code.as_str()), ("client_id", client_id.as_str())];
        let wrong_verifier = [&base[..], &[("code_verifier", "b-long-random-verifier-that-only-the-client-knows-0123456789")]].concat();
        assert_eq!(server.token(&wrong_verifier).await.1["error"], "invalid_grant");
        let no_verifier = server.token(&base).await;
        assert_eq!(no_verifier.1["error"], "invalid_grant");
        let wrong_redirect = [&base[..], &[("code_verifier", VERIFIER), ("redirect_uri", "http://127.0.0.1:7777/other")]].concat();
        assert_eq!(server.token(&wrong_redirect).await.1["error"], "invalid_grant");
        let wrong_resource = [&base[..], &[("code_verifier", VERIFIER), ("resource", "https://other.example/mcp")]].concat();
        assert_eq!(server.token(&wrong_resource).await.1["error"], "invalid_target");
        let other_client = server.public_client().await;
        assert_eq!(server.exchange(&other_client, &code).await.1["error"], "invalid_grant");
        let (status, tokens) = server.exchange(&client_id, &code).await;
        assert_eq!(status, 200, "{tokens}");
        let access = tokens["access_token"].as_str().unwrap();
        assert!(server.works(access).await);

        // A replay without the verifier is refused and leaves the client's tokens working.
        assert_eq!(server.token(&wrong_verifier).await.1["error"], "invalid_grant");
        assert_eq!(server.token(&base).await.1["error"], "invalid_grant");
        assert!(server.works(access).await);

        // A code used twice was likely stolen, so what the first use got is revoked.
        assert_eq!(server.exchange(&client_id, &code).await.1["error"], "invalid_grant");
        assert!(!server.works(access).await);

        // An expired code.
        let code = server.code(&client_id).await;
        sqlx::query("update oauth_codes set expires_at = now() - interval '1 second' where code_hash = $1").bind(sha256_hex(&code)).execute(&server.db).await.unwrap();
        assert_eq!(server.exchange(&client_id, &code).await.1["error"], "invalid_grant");

        assert_eq!(server.token(&[("grant_type", "password"), ("client_id", &client_id)]).await.1["error"], "unsupported_grant_type");
        assert_eq!(server.token(&[("grant_type", "authorization_code"), ("client_id", "nobody")]).await.0, 401);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn tokens_stop_working(db: PgPool) {
        let server = Server::start(db).await;
        let client_id = server.public_client().await;
        let set = |sql: &'static str, token: String| {
            let db = server.db.clone();
            async move { sqlx::query(sql).bind(sha256_hex(&token)).execute(&db).await.unwrap() }
        };

        let (access, refresh) = server.connect(&client_id).await;
        set("update oauth_tokens set expires_at = now() - interval '1 second' where token_hash = $1", access.clone()).await;
        let (status, body) = server.call(&access, "list_projects", json!({})).await;
        assert_eq!(status, 401, "{body}");
        // An expired access token is what refresh tokens are for.
        assert_eq!(server.refresh(&client_id, &refresh).await.0, 200);
        let (_, refresh) = server.connect(&client_id).await;
        set("update oauth_tokens set expires_at = now() - interval '1 second' where token_hash = $1", refresh.clone()).await;
        assert_eq!(server.refresh(&client_id, &refresh).await.1["error"], "invalid_grant");

        // A token for another resource, like one issued before PUBLIC_URL changed, is refused.
        let (access, _) = server.connect(&client_id).await;
        set("update oauth_grants set resource = 'https://old.example/api/mcp' where id = (select grant_id from oauth_tokens where token_hash = $1)", access.clone()).await;
        assert!(!server.works(&access).await);

        // So is one whose user left the organization.
        let (access, refresh) = server.connect(&client_id).await;
        assert!(server.works(&access).await);
        sqlx::query("delete from memberships").execute(&server.db).await.unwrap();
        assert!(!server.works(&access).await);
        assert_eq!(server.refresh(&client_id, &refresh).await.1["error"], "invalid_grant");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn disconnecting_an_app_signs_it_out(db: PgPool) {
        let server = Server::start(db).await;
        let client_id = server.public_client().await;
        let (access, _) = server.connect(&client_id).await;
        let (_, apps) = server.get("/organization/connected-apps").await;
        let id = apps[0]["id"].as_str().unwrap();
        let res = server.client.delete(format!("{}/organization/connected-apps/{id}", server.base)).header("cookie", "splee_session=session-ana").send().await.unwrap();
        assert_eq!(res.status(), 200);
        assert!(!server.works(&access).await);
        let res = server.client.delete(format!("{}/organization/connected-apps/{id}", server.base)).header("cookie", "splee_session=session-ana").send().await.unwrap();
        assert_eq!(res.status(), 404);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn confidential_clients_send_their_secret(db: PgPool) {
        let server = Server::start(db).await;
        // Without token_endpoint_auth_method, RFC 7591 means client_secret_basic.
        let (status, reg) = server.register(json!({ "client_name": "Web app", "redirect_uris": ["https://app.example/callback"] })).await;
        assert_eq!(status, 201);
        let (client_id, secret) = (reg["client_id"].as_str().unwrap(), reg["client_secret"].as_str().unwrap());
        assert_eq!(reg["token_endpoint_auth_method"], "client_secret_basic");

        let mut params = Server::authorize_params(client_id);
        params["redirect_uri"] = json!("https://app.example/callback");
        let (_, body) = server.check(&params).await;
        assert_eq!((body["redirect_host"].as_str(), body["local"].as_bool()), (Some("app.example"), Some(false)));
        let (_, body) = server.decide(&params, true).await;
        let url = Url::parse(body["redirect"].as_str().unwrap()).unwrap();
        let code = url.query_pairs().find(|(k, _)| k == "code").unwrap().1.into_owned();
        let form = [("grant_type", "authorization_code"), ("code", code.as_str()), ("code_verifier", VERIFIER)];

        let without = [&form[..], &[("client_id", client_id)]].concat();
        assert_eq!(server.token(&without).await.0, 401);
        let wrong = server.client.post(format!("{}/oauth/token", server.base)).basic_auth(client_id, Some("wrong")).form(&form);
        assert_eq!(Server::reply(wrong).await.0, 401);
        let right = server.client.post(format!("{}/oauth/token", server.base)).basic_auth(client_id, Some(secret)).form(&form);
        let (status, body) = Server::reply(right).await;
        assert_eq!(status, 200, "{body}");
    }

    #[test]
    fn client_id_metadata_documents() {
        let url = "https://chatgpt.com/oauth/client.json";
        assert!(super::metadata_url(url).is_some());
        for not_one in ["http://chatgpt.com/oauth/client.json", "https://chatgpt.com/", "https://chatgpt.com/a/../client.json", "b5a3c1d2-uuid"] {
            assert!(super::metadata_url(not_one).is_none(), "{not_one}");
        }
        let doc = json!({ "client_id": url, "client_name": " ChatGPT ", "token_endpoint_auth_method": "private_key_jwt",
            "redirect_uris": ["https://chatgpt.com/connector_platform_oauth_redirect", "http://evil.example/cb", "cursor://x"] });
        let (name, uris) = super::read_metadata(url, &doc).unwrap();
        assert_eq!((name.as_str(), uris), ("ChatGPT", vec!["https://chatgpt.com/connector_platform_oauth_redirect".to_string()]));

        let other = json!({ "client_id": "https://evil.example/client.json", "client_name": "ChatGPT", "redirect_uris": ["https://evil.example/cb"] });
        assert!(super::read_metadata(url, &other).is_err());
        let secret = json!({ "client_id": url, "client_name": "X", "redirect_uris": ["https://a.example/cb"], "token_endpoint_auth_method": "client_secret_post" });
        assert!(super::read_metadata(url, &secret).is_err());
        assert!(super::read_metadata(url, &json!({ "client_id": url, "client_name": "X", "redirect_uris": ["http://a.example/cb"] })).is_err());
        assert!(super::read_metadata(url, &json!({ "client_id": url, "redirect_uris": ["https://a.example/cb"] })).is_err());
    }

    #[test]
    fn loopback_redirects_may_pick_their_port() {
        let uris = ["http://localhost/callback".to_string(), "http://127.0.0.1/callback".to_string(), "https://app.example/cb".to_string()];
        for ok in ["http://localhost:51234/callback", "http://127.0.0.1:3118/callback", "http://localhost/callback", "https://app.example/cb"] {
            assert!(super::registered(&uris, ok), "{ok}");
        }
        for not_ok in ["http://localhost:51234/other", "http://evil.example:80/callback", "https://localhost:51234/callback", "https://app.example:8443/cb"] {
            assert!(!super::registered(&uris, not_ok), "{not_ok}");
        }
    }

    #[test]
    fn pkce_needs_the_verifier_behind_the_challenge() {
        assert!(super::pkce_matches(VERIFIER, &challenge(VERIFIER)));
        assert!(!super::pkce_matches("short", &challenge("short")));
        assert!(!super::pkce_matches(&format!("{VERIFIER}x"), &challenge(VERIFIER)));
    }
}
