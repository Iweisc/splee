//! Sign in with Facebook on the integrations page, for the Facebook Pages the user manages and the Instagram business accounts
//! linked to them. Start returns Facebook's sign-in URL, and Facebook sends the browser back to the callback, which lists the
//! Pages with their tokens and redirects to the project's integrations page with how it went. The user then picks which ones
//! the agent may use, through routes::bots' pick. The user's own token is only kept for the length of the callback. The state
//! works like the Tumblr sign-in's (routes/tumblr.rs). The client is crate::meta.

use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::settings::load_or_create;
use crate::auth::{Auth, MaybeAuth};
use crate::crypto::{random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::mail::oauth;
use crate::meta::{self, Graph};
use crate::models::{ProjectSettings, Ts};
use crate::state::AppState;

const STATE_MINUTES: i64 = 10;
const START_AGAIN: &str = "This sign-in expired or was already used. Sign in with Facebook again.";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/social/oauth/meta/callback", get(callback))
        .route("/projects/{id}/integrations/meta/oauth", post(start))
        .route("/projects/{id}/integrations/meta", delete(disconnect))
}

fn redirect_uri(state: &AppState) -> String {
    format!("{}/api/social/oauth/meta/callback", state.cfg.public_url.trim_end_matches('/'))
}

async fn start(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    let app = state.cfg.meta.as_ref().ok_or_else(|| AppError::BadRequest("Facebook sign-in isn't set up on this Splee".into()))?;
    let nonce = random_token();
    sqlx::query("delete from social_oauth_states where expires_at < now()").execute(&state.db).await?;
    sqlx::query("insert into social_oauth_states (nonce_hash, user_id, org_id, project_id, platform, expires_at) values ($1, $2, $3, $4, 'meta', $5)")
        .bind(sha256_hex(&nonce))
        .bind(auth.user_id)
        .bind(auth.org_id)
        .bind(id)
        .bind(Utc::now() + Duration::minutes(STATE_MINUTES))
        .execute(&state.db)
        .await?;
    let signed = oauth::sign_state(&state.cfg.app_secret, oauth::META, &nonce, auth.user_id, auth.org_id);
    let mut url = Url::parse(&app.authorize_url).map_err(|e| AppError::Internal(e.into()))?;
    url.query_pairs_mut()
        .append_pair("client_id", &app.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect_uri(&state))
        .append_pair("scope", meta::SCOPE)
        .append_pair("state", &signed);
    Ok(Json(json!({ "url": url.as_str() })))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_reason: Option<String>,
    error_description: Option<String>,
}

/// Always redirects: to the project's integrations page with `meta=connected` or `meta_error`, or to the app when the state
/// doesn't say which project.
async fn callback(State(state): State<AppState>, MaybeAuth(auth): MaybeAuth, Query(q): Query<CallbackQuery>) -> Redirect {
    let (page, params) = match finish(&state, auth, q).await {
        Ok(project) => (format!("/app/p/{project}/integrations"), vec![("meta", "connected".to_string())]),
        Err((Some(project), error)) => (format!("/app/p/{project}/integrations"), vec![("meta_error", error)]),
        Err((None, error)) => ("/app".to_string(), vec![("meta_error", error)]),
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
        tracing::error!(error = %e, "Facebook sign-in failed");
        "Something went wrong saving the Facebook sign-in. Try again.".to_string()
    };
    let signed = q.state.as_deref().unwrap_or_default();
    let nonce = oauth::state_nonce(signed).ok_or((None, START_AGAIN.to_string()))?;
    // Deleted before anything else is checked, so a state works once whatever happens.
    let pending = sqlx::query_as::<_, Pending>(
        "delete from social_oauth_states where nonce_hash = $1 and platform = 'meta' returning user_id, org_id, project_id, expires_at",
    )
    .bind(sha256_hex(nonce))
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (None, internal(&e)))?
    .ok_or((None, START_AGAIN.to_string()))?;
    let project = pending.project_id;
    let fail = |e: String| (Some(project), e);
    if !oauth::verify_state(&state.cfg.app_secret, oauth::META, signed, pending.user_id, pending.org_id) || pending.expires_at <= Utc::now() {
        return Err(fail(START_AGAIN.into()));
    }
    if let Some(error) = q.error {
        return Err(fail(match q.error_reason.as_deref() {
            Some("user_denied") => "You didn't allow Splee into your Facebook Pages, so nothing was connected.".into(),
            _ => format!("Facebook couldn't sign you in: {}", q.error_description.unwrap_or(error)),
        }));
    }
    let auth = auth.ok_or_else(|| fail("Sign in to Splee, then sign in with Facebook again.".into()))?;
    if (auth.user_id, auth.org_id) != (pending.user_id, pending.org_id) {
        return Err(fail("You switched organizations while signing in. Sign in with Facebook again from this one.".into()));
    }
    // Checked again, since access to the project could have gone in the meantime.
    auth.project(&state.db, project).await.map_err(|e| (None, e.public_message()))?;
    let code = q.code.filter(|c| !c.is_empty()).ok_or_else(|| fail(START_AGAIN.into()))?;

    let app = state.cfg.meta.as_ref().ok_or_else(|| fail("Facebook sign-in isn't set up on this Splee anymore.".into()))?;
    let graph = |token: &str| Graph::new(&state.cfg.graph_api_url, token).map_err(|e| fail(e.message()));
    let user_token = graph("")?.exchange_code(app, &code, &redirect_uri(state)).await.map_err(|e| fail(e.message()))?;
    let user = graph(&user_token)?;
    let (user_id, name) = user.me().await.map_err(|e| fail(e.message()))?;
    let pages = user.pages().await.map_err(|e| fail(e.message()))?;
    if pages.is_empty() {
        return Err(fail("Your Facebook account doesn't manage a Page, or you didn't pick one while signing in. Sign in again and pick your Pages.".into()));
    }

    load_or_create(&state.db, project).await.map_err(|e| fail(internal(&e)))?;
    meta::save_pages(&state.db, &state.cfg.app_secret, project, &pages).await.map_err(|e| fail(internal(&e)))?;
    sqlx::query(
        "update project_settings set meta_user_id = $2, meta_name = $3, meta_held_until = null, facebook_error = null, facebook_failed_at = null,
           instagram_error = null, instagram_failed_at = null
         where project_id = $1",
    )
    .bind(project)
    .bind(&user_id)
    .bind(&name)
    .execute(&state.db)
    .await
    .map_err(|e| fail(internal(&e)))?;
    Ok(project)
}

/// Drops the sign-in and the Pages' tokens. The Pages, accounts and picks stay for when the user signs in again, and approved
/// posts wait for it.
async fn disconnect(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("update social_channels set token_enc = '' where project_id = $1 and platform in ('facebook', 'instagram')").bind(id).execute(&mut *tx).await?;
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set meta_user_id = '', meta_name = '', meta_held_until = null, facebook_error = null, facebook_failed_at = null,
           instagram_error = null, instagram_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(settings))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use sqlx::PgPool;

    use super::*;
    use crate::config::{Config, OAuthApp};
    use crate::crypto;
    use crate::mail::oauth::tests::{FakeProvider, config};
    use crate::meta::tests::{Calls, fake_graph};
    use crate::models::{SocialChannel, SocialPost};

    struct Server {
        base: String,
        client: reqwest::Client,
        db: PgPool,
        state: AppState,
        graph: Calls,
        /// Ana's project in Acme. Beta is her other organization, and Cy's org has its own project.
        acme: Uuid,
        theirs: Uuid,
    }

    impl Server {
        async fn start(db: PgPool) -> Self {
            let (graph_url, graph) = fake_graph().await;
            let other = FakeProvider::start().await;
            let app = OAuthApp {
                client_id: "meta-app".into(),
                client_secret: "shh".into(),
                authorize_url: meta::AUTHORIZE_URL.into(),
                token_url: format!("{graph_url}/oauth/access_token"),
            };
            let cfg = Config { meta: Some(app), graph_api_url: graph_url, ..config(&other) };
            let state = AppState { db: db.clone(), cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(1).0 };
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
            let router = Router::new().nest("/api", crate::routes::router()).with_state(state.clone());
            tokio::spawn(async move { axum::serve(listener, router).await });
            let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
            Self { base, client, db, state, graph, acme: projects[0], theirs: projects[2] }
        }

        fn as_org(&self, request: reqwest::RequestBuilder, org: &str) -> reqwest::RequestBuilder {
            request.header("cookie", format!("splee_session=session-{org}"))
        }

        async fn authorize(&self, org: &str, project: Uuid) -> reqwest::Response {
            self.as_org(self.client.post(format!("{}/projects/{project}/integrations/meta/oauth", self.base)), org).send().await.unwrap()
        }

        async fn state(&self, org: &str, project: Uuid) -> String {
            let body: Value = self.authorize(org, project).await.json().await.unwrap();
            let q: HashMap<String, String> = Url::parse(body["url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect();
            assert_eq!((q["scope"].as_str(), q["response_type"].as_str(), q["client_id"].as_str()), (meta::SCOPE, "code", "meta-app"));
            assert_eq!(q["redirect_uri"], "http://localhost:3404/api/social/oauth/meta/callback");
            q["state"].clone()
        }

        /// Where the callback sent the browser, and its query.
        async fn callback(&self, org: &str, query: &[(&str, &str)]) -> (String, HashMap<String, String>) {
            let url = Url::parse_with_params(&format!("{}/social/oauth/meta/callback", self.base), query).unwrap();
            let res = self.as_org(self.client.get(url), org).send().await.unwrap();
            assert_eq!(res.status(), 303);
            let location = res.headers()["location"].to_str().unwrap().to_string();
            let url = Url::parse(&format!("http://x{location}")).unwrap();
            (url.path().to_string(), url.query_pairs().into_owned().collect())
        }

        async fn sign_in(&self, project: Uuid) {
            let state = self.state("acme", project).await;
            let (_, q) = self.callback("acme", &[("code", "the-code"), ("state", &state)]).await;
            assert_eq!(q.get("meta").map(String::as_str), Some("connected"), "{q:?}");
        }

        async fn channels(&self, project: Uuid) -> Vec<SocialChannel> {
            sqlx::query_as("select * from social_channels where project_id = $1 order by platform, chat_id").bind(project).fetch_all(&self.db).await.unwrap()
        }

        async fn pick(&self, org: &str, channel: Uuid) -> reqwest::Response {
            let request = self.client.patch(format!("{}/social-channels/{channel}", self.base)).json(&json!({ "picked": true }));
            self.as_org(request, org).send().await.unwrap()
        }

        async fn draft(&self, body: Value) -> reqwest::Response {
            self.as_org(self.client.post(format!("{}/projects/{}/social", self.base, self.acme)).json(&body), "acme").send().await.unwrap()
        }

        /// A post the draft call saved.
        async fn drafted(&self, body: Value) -> SocialPost {
            let res = self.draft(body).await;
            assert_eq!(res.status(), 200);
            let saved: Value = res.json().await.unwrap();
            self.post(saved["id"].as_str().unwrap().parse().unwrap()).await
        }

        async fn approve(&self, post: Uuid) -> reqwest::Response {
            let request = self.client.patch(format!("{}/social/{post}", self.base)).json(&json!({ "status": "approved" }));
            self.as_org(request, "acme").send().await.unwrap()
        }

        async fn post(&self, id: Uuid) -> SocialPost {
            sqlx::query_as("select * from social_posts where id = $1").bind(id).fetch_one(&self.db).await.unwrap()
        }

        fn posted(&self) -> Vec<String> {
            self.graph.lock().unwrap().iter().filter(|c| c.starts_with("POST")).cloned().collect()
        }

        async fn settings(&self) -> ProjectSettings {
            load_or_create(&self.db, self.acme).await.unwrap()
        }

        /// Ends the hour an account failure holds the accounts for.
        async fn unhold(&self) {
            sqlx::query("update project_settings set facebook_failed_at = null, instagram_failed_at = null, meta_held_until = null").execute(&self.db).await.unwrap();
        }
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn signing_in_lists_the_pages_and_instagram_accounts_with_their_tokens_encrypted(db: PgPool) {
        let s = Server::start(db).await;
        s.sign_in(s.acme).await;
        let traded: Vec<String> = s.graph.lock().unwrap().iter().filter(|c| c.contains("oauth/access_token")).cloned().collect();
        assert_eq!(traded, [
            "GET /oauth/access_token client_id=meta-app redirect_uri=http://localhost:3404/api/social/oauth/meta/callback code=the-code",
            "GET /oauth/access_token grant_type=fb_exchange_token client_id=meta-app fb_exchange_token=SHORT",
        ]);

        let settings = s.settings().await;
        assert!(settings.meta_connected && !settings.meta_reconnect);
        assert_eq!(settings.meta_name, "Ana Lee");
        let channels = s.channels(s.acme).await;
        let listed: Vec<String> = channels.iter().map(|c| format!("{} {} {} read={} send={} picked={}", c.platform, c.chat_id, c.label(), c.can_read, c.can_send, c.picked)).collect();
        assert_eq!(listed, [
            "facebook 111 Acme read=true send=true picked=false",
            "facebook 112 Acme Jobs read=false send=false picked=false",
            "instagram 1784 @acme.hq read=true send=true picked=false",
        ]);
        assert_eq!((channels[2].guild_id.as_str(), channels[2].guild_name.as_str()), ("111", "Acme"));
        // Each Page's token is stored encrypted, the Instagram account carries its Page's, and the user's own isn't kept.
        assert_eq!(crypto::decrypt("test-secret", &channels[0].token_enc).unwrap(), "GOOD");
        assert_eq!(crypto::decrypt("test-secret", &channels[2].token_enc).unwrap(), "GOOD");
        assert!(!channels[0].token_enc.contains("GOOD"));
        let stored: String = sqlx::query_scalar("select row_to_json(s)::text from project_settings s where project_id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
        assert!(!stored.contains("USER-LONG") && !stored.contains("SHORT"));
        // The channel list leaves the tokens out.
        let listed: Value = s.as_org(s.client.get(format!("{}/projects/{}/social-channels", s.base, s.acme)), "acme").send().await.unwrap().json().await.unwrap();
        assert!(listed[0].get("token_enc").is_none() && listed.to_string().contains("acme.hq"));

        // A Page the user's role doesn't let them use can't be picked. The others can, and a second sign-in keeps the picks.
        assert_eq!(s.pick("acme", channels[1].id).await.status(), 400);
        assert_eq!(s.pick("acme", channels[0].id).await.status(), 200);
        s.sign_in(s.acme).await;
        assert!(s.channels(s.acme).await[0].picked);

        // Used once only.
        let state = s.state("acme", s.acme).await;
        s.callback("acme", &[("code", "the-code"), ("state", &state)]).await;
        assert_eq!(s.callback("acme", &[("code", "the-code"), ("state", &state)]).await.1["meta_error"], START_AGAIN);

        // Disconnecting drops the tokens and keeps the picks.
        let res = s.as_org(s.client.delete(format!("{}/projects/{}/integrations/meta", s.base, s.acme)), "acme").send().await.unwrap();
        assert_eq!(res.status(), 200);
        let after = s.channels(s.acme).await;
        assert!(after.iter().all(|c| c.token_enc.is_empty()) && after[0].picked);
        assert!(!s.settings().await.meta_connected && !s.settings().await.meta_reconnect);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn other_orgs_cant_sign_in_pick_or_post_for_a_project(db: PgPool) {
        let s = Server::start(db).await;
        assert_eq!(s.authorize("acme", s.theirs).await.status(), 404);
        assert_eq!(s.callback("acme", &[("code", "the-code"), ("state", "made-up.state")]).await.1["meta_error"], START_AGAIN);
        let state = s.state("acme", s.acme).await;
        assert!(s.callback("beta", &[("code", "the-code"), ("state", &state)]).await.1["meta_error"].starts_with("You switched organizations"));
        // A Tumblr state can't finish a Facebook sign-in.
        let state = s.state("acme", s.acme).await;
        let (nonce, _) = state.split_once('.').unwrap();
        let user: Uuid = sqlx::query_scalar("select id from users where email = 'ana@acme.test'").fetch_one(&s.db).await.unwrap();
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
        let tumblr = oauth::sign_state("test-secret", oauth::TUMBLR, nonce, user, org);
        assert_eq!(s.callback("acme", &[("code", "the-code"), ("state", &tumblr)]).await.1["meta_error"], START_AGAIN);
        let state = s.state("acme", s.acme).await;
        let denied = [("error", "access_denied"), ("error_reason", "user_denied"), ("state", &state)];
        assert!(s.callback("acme", &denied).await.1["meta_error"].starts_with("You didn't allow Splee"));
        assert!(!s.graph.lock().unwrap().iter().any(|c| c.contains("access_token")), "no code was traded");

        s.sign_in(s.acme).await;
        let page = s.channels(s.acme).await[0].id;
        // Cy's org can't pick Acme's Page, and its own project can't post to it.
        assert_eq!(s.pick("other", page).await.status(), 404);
        assert_eq!(s.pick("acme", page).await.status(), 200);
        let request = s.client.post(format!("{}/projects/{}/social", s.base, s.theirs)).json(&json!({ "platform": "facebook", "body": "Hi", "channel_id": page }));
        assert_eq!(s.as_org(request, "other").send().await.unwrap().status(), 400);
        assert!(s.channels(s.theirs).await.is_empty());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn posts_and_replies_go_out_once_approved_and_instagram_waits_for_its_picture(db: PgPool) {
        let s = Server::start(db).await;
        s.sign_in(s.acme).await;
        let channels = s.channels(s.acme).await;
        let (page, ig) = (channels[0].id, channels[2].id);
        for c in [page, ig] {
            assert_eq!(s.pick("acme", c).await.status(), 200);
        }
        let draft = |platform: &str, channel: Uuid, extra: Value| {
            let mut body = json!({ "platform": platform, "body": "Our warmup report is out", "channel_id": channel });
            body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            body
        };
        // An Instagram post needs a picture, and a link has to be one anyone can open.
        assert_eq!(s.draft(draft("instagram", ig, json!({}))).await.status(), 400);
        assert_eq!(s.draft(draft("facebook", page, json!({ "link": "javascript:alert(1)" }))).await.status(), 400);
        let fb = s.drafted(draft("facebook", page, json!({ "link": "https://acme.test/report" }))).await;
        let photo = s.drafted(draft("instagram", ig, json!({ "image_url": "https://acme.test/team.jpg" }))).await;
        assert_eq!((fb.status.as_str(), photo.image_url.as_str()), ("draft", "https://acme.test/team.jpg"));

        // Drafts never go out.
        crate::social::tick(&s.state).await.unwrap();
        assert!(s.posted().is_empty());
        assert_eq!(s.approve(fb.id).await.status(), 200);
        assert_eq!(s.approve(photo.id).await.status(), 200);
        crate::social::tick(&s.state).await.unwrap();
        let fb = s.post(fb.id).await;
        assert_eq!((fb.status.as_str(), fb.url.as_str(), fb.account.as_str()), ("posted", "https://www.facebook.com/acmehq/posts/111_5001", "111"));
        let photo = s.post(photo.id).await;
        assert_eq!((photo.status.as_str(), photo.url.as_str(), photo.account.as_str()), ("posted", "https://www.instagram.com/p/P904001/", "1784"));
        assert_eq!(s.posted(), [
            "POST /111/feed message=Our warmup report is out link=https://acme.test/report",
            "POST /1784/media image_url=https://acme.test/team.jpg caption=Our warmup report is out",
            "POST /1784/media_publish creation_id=4001",
        ]);

        // Replies answer comments the agent read, on the Page or account they're on.
        let replies = [
            ("facebook", "https://www.facebook.com/111_222?comment_id=222_333", "It works with Gmail"),
            ("instagram", "https://www.instagram.com/p/DCx9q/c/1790/?account=1784", "From $9 a month"),
        ];
        let mut ids = vec![];
        for (platform, link, body) in replies {
            let channel = if platform == "facebook" { page } else { ig };
            let id: Uuid = sqlx::query_scalar(
                "insert into social_posts (project_id, platform, in_reply_to, body, status, channel_id) values ($1, $2, $3, $4, 'approved', $5) returning id",
            )
            .bind(s.acme)
            .bind(platform)
            .bind(link)
            .bind(body)
            .bind(channel)
            .fetch_one(&s.db)
            .await
            .unwrap();
            ids.push(id);
        }
        crate::social::tick(&s.state).await.unwrap();
        assert_eq!(s.post(ids[0]).await.url, "https://www.facebook.com/acmehq/posts/222_6001");
        assert_eq!(s.post(ids[1]).await.url, "https://www.instagram.com/p/DCx9q/c/1790/");
        assert!(s.posted().ends_with(&["POST /222_333/comments message=It works with Gmail".into(), "POST /1790/replies message=From $9 a month".into()]));

        // A picture Instagram is still processing waits, and the next try publishes the same container.
        let slow = s.drafted(draft("instagram", ig, json!({ "image_url": "https://acme.test/slow.jpg", "status": "approved" }))).await;
        crate::social::tick(&s.state).await.unwrap();
        let waiting = s.post(slow.id).await;
        assert_eq!((waiting.status.as_str(), waiting.container_id.as_str()), ("approved", "4002"));
        assert!(s.settings().await.instagram_error.unwrap().contains("getting the picture ready"));
        s.unhold().await;
        crate::social::tick(&s.state).await.unwrap();
        assert_eq!(s.posted().iter().filter(|c| c.contains("slow.jpg")).count(), 1);
        assert_eq!(s.post(slow.id).await.status, "approved");
        // Approving it again after it failed keeps the container, which an earlier try may have published already.
        sqlx::query("update social_posts set status = 'failed' where id = $1").bind(slow.id).execute(&s.db).await.unwrap();
        assert_eq!(s.approve(slow.id).await.status(), 200);
        assert_eq!(s.post(slow.id).await.container_id, "4002");
        // One Instagram turned down is dropped, so approving the post again makes a new one.
        let broken = s.drafted(draft("instagram", ig, json!({ "image_url": "https://acme.test/broken.jpg", "status": "approved" }))).await;
        // Out of the way, since it would take the turn and wait again.
        sqlx::query("update social_posts set status = 'skipped' where id = $1").bind(slow.id).execute(&s.db).await.unwrap();
        s.unhold().await;
        crate::social::tick(&s.state).await.unwrap();
        let broken = s.post(broken.id).await;
        assert_eq!((broken.status.as_str(), broken.container_id.as_str()), ("failed", ""));
        // One whose publish errored may have gone out, so it's kept for the next try to check.
        let lost = s.drafted(draft("instagram", ig, json!({ "image_url": "https://acme.test/lost.jpg", "status": "approved" }))).await;
        s.unhold().await;
        crate::social::tick(&s.state).await.unwrap();
        assert_eq!(s.post(lost.id).await.container_id, "4004");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_revoked_token_signs_the_project_out_and_meta_usage_holds_posting(db: PgPool) {
        let s = Server::start(db).await;
        s.sign_in(s.acme).await;
        let channels = s.channels(s.acme).await;
        s.pick("acme", channels[0].id).await;
        let token = |t: &str| crypto::encrypt("test-secret", t);
        let approved = |body: &str| {
            sqlx::query_scalar::<_, Uuid>("insert into social_posts (project_id, platform, body, status, channel_id) values ($1, 'facebook', $2, 'approved', $3) returning id")
                .bind(s.acme)
                .bind(body.to_string())
                .bind(channels[0].id)
                .fetch_one(&s.db)
        };

        // At 95% of the Page's limit the post still goes out, and the next one waits the 12 minutes Meta said.
        sqlx::query("update social_channels set token_enc = $2 where id = $1").bind(channels[0].id).bind(token("BUSY")).execute(&s.db).await.unwrap();
        let first = approved("First").await.unwrap();
        let second = approved("Second").await.unwrap();
        crate::social::tick(&s.state).await.unwrap();
        assert_eq!(s.post(first).await.status, "posted");
        let wait: f64 = sqlx::query_scalar("select extract(epoch from meta_held_until - now())::float8 from project_settings where project_id = $1")
            .bind(s.acme)
            .fetch_one(&s.db)
            .await
            .unwrap();
        assert!((700.0..=720.0).contains(&wait), "{wait}");
        crate::social::tick(&s.state).await.unwrap();
        assert_eq!(s.post(second).await.status, "approved");
        assert_eq!(s.posted().len(), 1);

        // Error 190 signs the project out: the tokens go, approved posts go back to drafts and the card asks to sign in again.
        s.unhold().await;
        sqlx::query("update social_channels set token_enc = $2 where project_id = $1").bind(s.acme).bind(token("EXPIRED")).execute(&s.db).await.unwrap();
        crate::social::tick(&s.state).await.unwrap();
        assert_eq!(s.post(second).await.status, "draft");
        let settings = s.settings().await;
        assert!(!settings.meta_connected && settings.meta_reconnect);
        assert!(settings.facebook_error.unwrap().contains("Sign in with Facebook again"));
        assert!(s.channels(s.acme).await.iter().all(|c| c.token_enc.is_empty()));
        assert_eq!(s.approve(second).await.status(), 400);
        // Signing in again brings the Pages back as they were picked.
        s.sign_in(s.acme).await;
        let settings = s.settings().await;
        assert!(settings.meta_connected && !settings.meta_reconnect && settings.facebook_error.is_none());
        assert_eq!(s.approve(second).await.status(), 200);
    }
}
