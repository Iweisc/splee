//! Connect Tumblr on the integrations page. Start returns Tumblr's sign-in URL, and Tumblr sends the browser back to the
//! callback, which saves the tokens and the account's blogs and redirects to the project's integrations page with how it
//! went. The state works like the mailbox sign-in's (routes/mailbox_oauth.rs). The client and token renewal are in crate::tumblr.

use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::settings::load_or_create;
use crate::auth::{Auth, MaybeAuth};
use crate::crypto::{self, random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::mail::oauth;
use crate::models::{ProjectSettings, Ts};
use crate::state::AppState;
use crate::tumblr::{self, Tumblr};

const STATE_MINUTES: i64 = 10;
const START_AGAIN: &str = "This sign-in expired or was already used. Connect Tumblr again.";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/social/oauth/tumblr/callback", get(callback))
        .route("/projects/{id}/integrations/tumblr/oauth", post(start))
        .route("/projects/{id}/integrations/tumblr", put(pick_blog).delete(disconnect))
}

fn redirect_uri(state: &AppState) -> String {
    format!("{}/api/social/oauth/tumblr/callback", state.cfg.public_url.trim_end_matches('/'))
}

async fn start(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    let app = tumblr::app(&state.cfg).map_err(|_| AppError::BadRequest("Tumblr sign-in isn't set up on this Splee".into()))?;
    let nonce = random_token();
    sqlx::query("delete from social_oauth_states where expires_at < now()").execute(&state.db).await?;
    sqlx::query("insert into social_oauth_states (nonce_hash, user_id, org_id, project_id, platform, expires_at) values ($1, $2, $3, $4, 'tumblr', $5)")
        .bind(sha256_hex(&nonce))
        .bind(auth.user_id)
        .bind(auth.org_id)
        .bind(id)
        .bind(Utc::now() + Duration::minutes(STATE_MINUTES))
        .execute(&state.db)
        .await?;
    let signed = oauth::sign_state(&state.cfg.app_secret, oauth::TUMBLR, &nonce, auth.user_id, auth.org_id);
    let mut url = Url::parse(&app.authorize_url).map_err(|e| AppError::Internal(e.into()))?;
    url.query_pairs_mut()
        .append_pair("client_id", &app.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect_uri(&state))
        .append_pair("scope", tumblr::SCOPE)
        .append_pair("state", &signed);
    Ok(Json(json!({ "url": url.as_str() })))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Always redirects: to the project's integrations page with `tumblr=connected` or `tumblr_error`, or to the app when the
/// state doesn't say which project.
async fn callback(State(state): State<AppState>, MaybeAuth(auth): MaybeAuth, Query(q): Query<CallbackQuery>) -> Redirect {
    let (page, params) = match finish(&state, auth, q).await {
        Ok(project) => (format!("/app/p/{project}/integrations"), vec![("tumblr", "connected".to_string())]),
        Err((Some(project), error)) => (format!("/app/p/{project}/integrations"), vec![("tumblr_error", error)]),
        Err((None, error)) => ("/app".to_string(), vec![("tumblr_error", error)]),
    };
    let url = Url::parse_with_params("http://splee/", &params).expect("a fixed URL");
    Redirect::to(&format!("{page}?{}", url.query().unwrap_or_default()))
}

#[derive(sqlx::FromRow)]
struct Pending {
    user_id: Uuid,
    org_id: Uuid,
    project_id: Uuid,
    expires_at: Ts,
}

/// The project it connected, or what went wrong as the page shows it, with the project when the state named one.
async fn finish(state: &AppState, auth: Option<Auth>, q: CallbackQuery) -> Result<Uuid, (Option<Uuid>, String)> {
    let internal = |e: &dyn std::fmt::Display| {
        tracing::error!(error = %e, "Tumblr sign-in failed");
        "Something went wrong saving the Tumblr sign-in. Try again.".to_string()
    };
    let signed = q.state.as_deref().unwrap_or_default();
    let nonce = oauth::state_nonce(signed).ok_or((None, START_AGAIN.to_string()))?;
    // Deleted before anything else is checked, so a state works once whatever happens.
    let pending = sqlx::query_as::<_, Pending>(
        "delete from social_oauth_states where nonce_hash = $1 and platform = 'tumblr' returning user_id, org_id, project_id, expires_at",
    )
    .bind(sha256_hex(nonce))
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (None, internal(&e)))?
    .ok_or((None, START_AGAIN.to_string()))?;
    let project = pending.project_id;
    let fail = |e: String| (Some(project), e);
    if !oauth::verify_state(&state.cfg.app_secret, oauth::TUMBLR, signed, pending.user_id, pending.org_id) || pending.expires_at <= Utc::now() {
        return Err(fail(START_AGAIN.into()));
    }
    if let Some(error) = q.error {
        return Err(fail(match error.as_str() {
            "access_denied" => "You didn't allow Splee into the Tumblr account, so nothing was connected.".into(),
            _ => format!("Tumblr couldn't sign you in: {}", q.error_description.unwrap_or(error)),
        }));
    }
    let auth = auth.ok_or_else(|| fail("Sign in to Splee, then connect Tumblr again.".into()))?;
    if (auth.user_id, auth.org_id) != (pending.user_id, pending.org_id) {
        return Err(fail("You switched organizations while signing in. Connect Tumblr again from this one.".into()));
    }
    // Checked again, since access to the project could have gone in the meantime.
    auth.project(&state.db, project).await.map_err(|e| (None, e.public_message()))?;
    let code = q.code.filter(|c| !c.is_empty()).ok_or_else(|| fail(START_AGAIN.into()))?;

    let app = tumblr::app(&state.cfg).map_err(|e| fail(e.message()))?;
    let tokens = tumblr::exchange_code(app, &code, &redirect_uri(state)).await.map_err(|e| fail(e.to_string()))?;
    let refresh = tokens.refresh_token.as_deref().filter(|t| !t.is_empty()).ok_or_else(|| fail("Tumblr didn't let Splee stay signed in. Connect Tumblr again.".into()))?;
    let account = Tumblr::new(app, &tokens.access_token, "").map_err(|e| fail(e.message()))?.me().await.map_err(|e| fail(e.message()))?;
    let first = account.blogs.first().and_then(|b| b["name"].as_str()).ok_or_else(|| fail("That Tumblr account has no blog to post to.".into()))?;

    load_or_create(&state.db, project).await.map_err(|e| fail(internal(&e)))?;
    let secret = &state.cfg.app_secret;
    // Signing in again as the same account keeps the blog picked before.
    sqlx::query(
        "update project_settings set tumblr_token_enc = $2, tumblr_refresh_enc = $3, tumblr_expires_at = $4, tumblr_blogs = $6,
           tumblr_blog = case when tumblr_username = $5 and $6 @> jsonb_build_array(jsonb_build_object('name', tumblr_blog)) then tumblr_blog else $7 end,
           tumblr_username = $5, tumblr_error = null, tumblr_failed_at = null
         where project_id = $1",
    )
    .bind(project)
    .bind(crypto::encrypt(secret, &tokens.access_token))
    .bind(crypto::encrypt(secret, refresh))
    .bind(tokens.expires_at())
    .bind(&account.name)
    .bind(json!(account.blogs))
    .bind(first)
    .execute(&state.db)
    .await
    .map_err(|e| fail(internal(&e)))?;
    Ok(project)
}

#[derive(Deserialize)]
struct BlogBody {
    blog: String,
}

/// Picks which of the account's blogs posts.
async fn pick_blog(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<BlogBody>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set tumblr_blog = $2 where project_id = $1 and tumblr_blogs @> jsonb_build_array(jsonb_build_object('name', $2::text))
         returning *",
    )
    .bind(id)
    .bind(b.blog.trim())
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::BadRequest(format!("{} isn't one of the account's blogs", b.blog.trim())))?;
    Ok(Json(settings))
}

async fn disconnect(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set tumblr_token_enc = '', tumblr_refresh_enc = '', tumblr_expires_at = null, tumblr_username = '',
           tumblr_blogs = '[]', tumblr_blog = '', tumblr_error = null, tumblr_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use sqlx::PgPool;

    use super::*;
    use crate::config::Config;
    use crate::mail::oauth::tests::{FakeProvider, config};
    use crate::tumblr::tests::{Sent, fake_tumblr};

    struct Server {
        base: String,
        client: reqwest::Client,
        db: PgPool,
        sent: Sent,
        /// Ana's project in Acme. Beta is her other organization, and Cy's org has its own project.
        acme: Uuid,
        theirs: Uuid,
    }

    impl Server {
        async fn start(db: PgPool) -> Self {
            let (app, sent) = fake_tumblr().await;
            let other = FakeProvider::start().await;
            let state = AppState { db: db.clone(), cfg: Arc::new(Config { tumblr: Some(app), ..config(&other) }), events: tokio::sync::broadcast::channel(1).0 };
            let mut projects = vec![];
            for (email, orgs) in [("ana@acme.test", vec!["acme", "beta"]), ("cy@other.test", vec!["other"])] {
                let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, 'x', 'x') returning id").bind(email).fetch_one(&db).await.unwrap();
                for name in orgs {
                    let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap();
                    sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(user).execute(&db).await.unwrap();
                    sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                        .bind(sha256_hex(&format!("session-{name}")))
                        .bind(user)
                        .bind(org)
                        .execute(&db)
                        .await
                        .unwrap();
                    let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(&db).await.unwrap();
                    projects.push(project);
                }
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let router = Router::new().nest("/api", crate::routes::router()).with_state(state);
            tokio::spawn(async move { axum::serve(listener, router).await });
            let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
            Self { base, client, db, sent, acme: projects[0], theirs: projects[2] }
        }

        async fn authorize(&self, org: &str, project: Uuid) -> reqwest::Response {
            self.client
                .post(format!("{}/projects/{project}/integrations/tumblr/oauth", self.base))
                .header("cookie", format!("splee_session=session-{org}"))
                .send()
                .await
                .unwrap()
        }

        async fn state(&self, org: &str, project: Uuid) -> String {
            let body: Value = self.authorize(org, project).await.json().await.unwrap();
            let q: HashMap<String, String> = Url::parse(body["url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect();
            assert_eq!((q["scope"].as_str(), q["response_type"].as_str(), q["client_id"].as_str()), ("basic write offline_access", "code", "tumblr-client"));
            assert_eq!(q["redirect_uri"], "http://localhost:3404/api/social/oauth/tumblr/callback");
            q["state"].clone()
        }

        /// Where the callback sent the browser, and its query.
        async fn callback(&self, org: &str, state: &str) -> (String, HashMap<String, String>) {
            let url = Url::parse_with_params(&format!("{}/social/oauth/tumblr/callback", self.base), [("code", "the-code"), ("state", state)]).unwrap();
            let res = self.client.get(url).header("cookie", format!("splee_session=session-{org}")).send().await.unwrap();
            assert_eq!(res.status(), 303);
            let location = res.headers()["location"].to_str().unwrap().to_string();
            let url = Url::parse(&format!("http://x{location}")).unwrap();
            (url.path().to_string(), url.query_pairs().into_owned().collect())
        }

        async fn settings(&self, project: Uuid) -> ProjectSettings {
            load_or_create(&self.db, project).await.unwrap()
        }
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn signing_in_saves_the_account_once_per_state(db: PgPool) {
        let s = Server::start(db).await;
        let state = s.state("acme", s.acme).await;
        let (page, q) = s.callback("acme", &state).await;
        assert_eq!((page, q["tumblr"].as_str()), (format!("/app/p/{}/integrations", s.acme), "connected"));
        let form = s.sent.lock().unwrap()[0].clone();
        assert_eq!((form["grant_type"].as_str(), form["code"].as_str()), (Some("authorization_code"), Some("the-code")));
        assert_eq!((form["client_id"].as_str(), form["client_secret"].as_str()), (Some("tumblr-client"), Some("shh")));

        let settings = s.settings(s.acme).await;
        assert!(settings.tumblr_connected);
        assert_eq!((settings.tumblr_username.as_str(), settings.tumblr_blog.as_str()), ("acme", "acme"));
        assert_eq!(settings.tumblr_blogs.as_array().unwrap().len(), 2);
        let refresh: String = sqlx::query_scalar("select tumblr_refresh_enc from project_settings where project_id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
        assert_eq!(crypto::decrypt("test-secret", &refresh).unwrap(), "refresh-1");

        // Used once only, and nothing more is asked of Tumblr.
        assert_eq!(s.callback("acme", &state).await.1["tumblr_error"], START_AGAIN);
        assert_eq!(s.sent.lock().unwrap().len(), 1);

        // Picking a blog takes only the account's own.
        let pick = |blog: &str| s.client.put(format!("{}/projects/{}/integrations/tumblr", s.base, s.acme)).header("cookie", "splee_session=session-acme").json(&json!({ "blog": blog }));
        assert_eq!(pick("acme-labs").send().await.unwrap().status(), 200);
        assert_eq!(pick("someone-else").send().await.unwrap().status(), 400);
        assert_eq!(s.settings(s.acme).await.tumblr_blog, "acme-labs");
        // Signing in again as the same account keeps that pick.
        let state = s.state("acme", s.acme).await;
        s.callback("acme", &state).await;
        assert_eq!(s.settings(s.acme).await.tumblr_blog, "acme-labs");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_callback_refuses_a_bad_foreign_or_expired_state_and_other_orgs_projects(db: PgPool) {
        let s = Server::start(db).await;
        assert_eq!(s.callback("acme", "made-up.state").await, ("/app".to_string(), HashMap::from([("tumblr_error".to_string(), START_AGAIN.to_string())])));
        // A sign-in can't start for another org's project.
        assert_eq!(s.authorize("acme", s.theirs).await.status(), 404);

        let state = s.state("acme", s.acme).await;
        let (nonce, _) = state.split_once('.').unwrap();
        assert_eq!(s.callback("acme", &format!("{nonce}.{}", "0".repeat(64))).await.1["tumblr_error"], START_AGAIN, "a forged signature");

        // A mailbox state can't finish a Tumblr sign-in.
        let state = s.state("acme", s.acme).await;
        let (nonce, _) = state.split_once('.').unwrap();
        let user: Uuid = sqlx::query_scalar("select id from users where email = 'ana@acme.test'").fetch_one(&s.db).await.unwrap();
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
        let mailbox = oauth::sign_state("test-secret", oauth::MAILBOX, nonce, user, org);
        assert_eq!(s.callback("acme", &mailbox).await.1["tumblr_error"], START_AGAIN);

        let state = s.state("acme", s.acme).await;
        assert!(s.callback("beta", &state).await.1["tumblr_error"].starts_with("You switched organizations"));

        let state = s.state("acme", s.acme).await;
        sqlx::query("update social_oauth_states set expires_at = now() - interval '1 second'").execute(&s.db).await.unwrap();
        assert_eq!(s.callback("acme", &state).await.1["tumblr_error"], START_AGAIN);

        assert!(s.sent.lock().unwrap().is_empty(), "no code was traded");
        assert!(!s.settings(s.acme).await.tumblr_connected);
    }
}
