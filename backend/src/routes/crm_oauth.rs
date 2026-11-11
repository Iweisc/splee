//! Signing in to Salesforce, Zoho CRM and Dynamics 365 Sales on the integrations page, testing any connected CRM, and picking
//! the status booked leads get in Salesforce. Start returns the CRM's sign-in URL, and the CRM sends the browser back to the
//! callback, which saves the connection and redirects to the project's integrations page with how it went. The state works
//! like Tumblr's (routes/tumblr.rs). The tokens are in crm::signin, and disconnecting is DELETE /integrations/{crm}.

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
use crate::crm::{self, Crm, signin};
use crate::crypto::{self, random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::mail::oauth;
use crate::models::{ProjectSettings, Ts};
use crate::state::AppState;

const STATE_MINUTES: i64 = 10;

fn start_again(crm: Crm) -> String {
    format!("This sign-in expired or was already used. Connect {} again.", crm.label())
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/crm/oauth/callback", get(callback))
        .route("/projects/{id}/integrations/{crm}/oauth", post(start))
        .route("/projects/{id}/integrations/{crm}/test", post(test))
        .route("/projects/{id}/integrations/{crm}/status", put(pick_status))
}

fn signs_in(name: &str) -> AppResult<Crm> {
    Crm::parse(name).filter(|crm| crm.signs_in()).ok_or(AppError::NotFound)
}

#[derive(Deserialize)]
struct StartBody {
    #[serde(default)]
    sandbox: bool,
    /// Dynamics 365's org address.
    #[serde(default)]
    org_url: String,
}

async fn start(State(state): State<AppState>, auth: Auth, Path((id, name)): Path<(Uuid, String)>, Json(body): Json<StartBody>) -> AppResult<Json<Value>> {
    let crm = signs_in(&name)?;
    auth.project(&state.db, id).await?;
    let site = match crm {
        Crm::Dynamics => signin::org_root(&state.cfg, &body.org_url)
            .ok_or_else(|| AppError::BadRequest("Paste your Dynamics 365 address, like https://acme.crm.dynamics.com".into()))?,
        Crm::Salesforce if body.sandbox => signin::SANDBOX.into(),
        _ => String::new(),
    };
    let (nonce, verifier) = (random_token(), random_token());
    let signed = oauth::sign_state(&state.cfg.app_secret, oauth::CRM, &nonce, auth.user_id, auth.org_id);
    let url = signin::authorize_url(&state.cfg, crm, &site, &signed, &verifier).ok_or_else(|| AppError::BadRequest(signin::not_set_up(crm)))?;
    sqlx::query("delete from crm_oauth_states where expires_at < now()").execute(&state.db).await?;
    sqlx::query(
        "insert into crm_oauth_states (nonce_hash, user_id, org_id, project_id, crm, code_verifier_enc, site, expires_at) values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(sha256_hex(&nonce))
    .bind(auth.user_id)
    .bind(auth.org_id)
    .bind(id)
    .bind(crm.name())
    .bind(crypto::encrypt(&state.cfg.app_secret, &verifier))
    .bind(&site)
    .bind(Utc::now() + Duration::minutes(STATE_MINUTES))
    .execute(&state.db)
    .await?;
    Ok(Json(json!({ "url": url })))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    /// Zoho's accounts server for the account's data centre.
    #[serde(rename = "accounts-server")]
    accounts_server: Option<String>,
}

/// Always redirects: to the project's integrations page with `crm` set to the CRM it connected, or with `crm_error`, or to the
/// app when the state doesn't say which project.
async fn callback(State(state): State<AppState>, MaybeAuth(auth): MaybeAuth, Query(q): Query<CallbackQuery>) -> Redirect {
    let (page, params) = match finish(&state, auth, q).await {
        Ok((project, crm)) => (format!("/app/p/{project}/integrations"), vec![("crm", crm.name().to_string())]),
        Err((Some(project), error)) => (format!("/app/p/{project}/integrations"), vec![("crm_error", error)]),
        Err((None, error)) => ("/app".to_string(), vec![("crm_error", error)]),
    };
    let url = Url::parse_with_params("http://splee/", &params).expect("a fixed URL");
    Redirect::to(&format!("{page}?{}", url.query().unwrap_or_default()))
}

#[derive(sqlx::FromRow)]
struct Pending {
    user_id: Uuid,
    org_id: Uuid,
    project_id: Uuid,
    crm: String,
    code_verifier_enc: String,
    site: String,
    expires_at: Ts,
}

const UNKNOWN_STATE: &str = "This sign-in expired or was already used. Connect the CRM again.";

/// The project and CRM it connected, or what went wrong as the page shows it, with the project when the state named one.
async fn finish(state: &AppState, auth: Option<Auth>, q: CallbackQuery) -> Result<(Uuid, Crm), (Option<Uuid>, String)> {
    let internal = |e: &dyn std::fmt::Display| {
        tracing::error!(error = %e, "CRM sign-in failed");
        "Something went wrong saving the sign-in. Try again.".to_string()
    };
    let signed = q.state.as_deref().unwrap_or_default();
    let nonce = oauth::state_nonce(signed).ok_or((None, UNKNOWN_STATE.to_string()))?;
    // Deleted before anything else is checked, so a state works once whatever happens.
    let pending = sqlx::query_as::<_, Pending>(
        "delete from crm_oauth_states where nonce_hash = $1 returning user_id, org_id, project_id, crm, code_verifier_enc, site, expires_at",
    )
    .bind(sha256_hex(nonce))
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (None, internal(&e)))?
    .ok_or((None, UNKNOWN_STATE.to_string()))?;
    let project = pending.project_id;
    let fail = |e: String| (Some(project), e);
    let crm = Crm::parse(&pending.crm).ok_or_else(|| fail(UNKNOWN_STATE.into()))?;
    let label = crm.label();
    if !oauth::verify_state(&state.cfg.app_secret, oauth::CRM, signed, pending.user_id, pending.org_id) || pending.expires_at <= Utc::now() {
        return Err(fail(start_again(crm)));
    }
    if let Some(error) = q.error {
        return Err(fail(match error.as_str() {
            "access_denied" => format!("You didn't allow Splee into {label}, so nothing was connected."),
            _ => format!("{label} couldn't sign you in: {}", q.error_description.unwrap_or(error)),
        }));
    }
    let auth = auth.ok_or_else(|| fail(format!("Sign in to Splee, then connect {label} again.")))?;
    if (auth.user_id, auth.org_id) != (pending.user_id, pending.org_id) {
        return Err(fail(format!("You switched organizations while signing in. Connect {label} again from this one.")));
    }
    // Checked again, since access to the project could have gone in the meantime.
    auth.project(&state.db, project).await.map_err(|e| (None, e.public_message()))?;
    let code = q.code.filter(|c| !c.is_empty()).ok_or_else(|| fail(start_again(crm)))?;
    let verifier = crypto::decrypt(&state.cfg.app_secret, &pending.code_verifier_enc).map_err(|e| fail(internal(&e)))?;
    load_or_create(&state.db, project).await.map_err(|e| fail(internal(&e)))?;
    signin::connect(state, project, crm, &pending.site, q.accounts_server.as_deref(), &code, &verifier).await.map_err(fail)?;
    Ok((project, crm))
}

/// Checks a connected CRM still takes Splee's token or sign-in. How it went is saved like a sync, so a pass clears the last
/// failure, and the page reloads the settings either way.
async fn test(State(state): State<AppState>, auth: Auth, Path((id, name)): Path<(Uuid, String)>) -> AppResult<Json<ProjectSettings>> {
    let crm = Crm::parse(&name).ok_or(AppError::NotFound)?;
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    crm::test(&state, id, crm).await.map_err(|e| AppError::BadRequest(format!("The test didn't go through: {e:#}")))?;
    Ok(Json(load_or_create(&state.db, id).await?))
}

#[derive(Deserialize)]
struct StatusBody {
    status: String,
}

/// Picks the Lead Status booked leads get in Salesforce, from the org's picklist, or none to leave it alone.
async fn pick_status(State(state): State<AppState>, auth: Auth, Path((id, name)): Path<(Uuid, String)>, Json(b): Json<StatusBody>) -> AppResult<Json<ProjectSettings>> {
    if signs_in(&name)? != Crm::Salesforce {
        return Err(AppError::NotFound);
    }
    auth.project(&state.db, id).await?;
    let status = b.status.trim();
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set salesforce_booked_status = $2
         where project_id = $1 and ($2 = '' or salesforce_statuses @> jsonb_build_array(jsonb_build_object('value', $2::text)))
         returning *",
    )
    .bind(id)
    .bind(status)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::BadRequest(format!("{status} isn't one of the org's lead statuses. Test Salesforce to read them again.")))?;
    Ok(Json(settings))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use sqlx::PgPool;

    use super::*;
    use crate::crm::signin::tests::{Fake, LIVE, connect_fake};

    struct Server {
        base: String,
        client: reqwest::Client,
        db: PgPool,
        fake: Fake,
        state: AppState,
        /// Ana's project in Acme. Beta is her other organization, and Cy's org has its own project.
        acme: Uuid,
        theirs: Uuid,
    }

    impl Server {
        async fn start(db: PgPool) -> Self {
            let fake = Fake::start().await;
            let state = fake.state(db.clone());
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
            Self { base, client, db, fake, state, acme: projects[0], theirs: projects[2] }
        }

        fn call(&self, method: reqwest::Method, org: &str, path: &str) -> reqwest::RequestBuilder {
            self.client.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session=session-{org}"))
        }

        async fn start_sign_in(&self, org: &str, project: Uuid, crm: &str, body: Value) -> reqwest::Response {
            self.call(reqwest::Method::POST, org, &format!("/projects/{project}/integrations/{crm}/oauth")).json(&body).send().await.unwrap()
        }

        /// The sign-in page's query for a sign-in started in Acme.
        async fn authorize(&self, crm: &str, body: Value) -> HashMap<String, String> {
            let res = self.start_sign_in("acme", self.acme, crm, body).await;
            assert_eq!(res.status(), 200);
            let url: Value = res.json().await.unwrap();
            Url::parse(url["url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect()
        }

        /// Where the callback sent the browser, and its query.
        async fn callback(&self, org: &str, params: &[(&str, &str)]) -> (String, HashMap<String, String>) {
            let url = Url::parse_with_params(&format!("{}/crm/oauth/callback", self.base), params).unwrap();
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

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn signing_in_connects_each_crm_once_per_state(db: PgPool) {
        let s = Server::start(db).await;
        let page = format!("/app/p/{}/integrations", s.acme);

        let q = s.authorize("salesforce", json!({"sandbox": true})).await;
        assert_eq!((q["client_id"].as_str(), q["scope"].as_str()), ("client-123", "api refresh_token"));
        let back = s.callback("acme", &[("code", "sf-code"), ("state", &q["state"])]).await;
        assert_eq!(back, (page.clone(), HashMap::from([("crm".to_string(), "salesforce".to_string())])));
        let form = s.fake.token_requests()[0].body.clone();
        assert_eq!((form["grant_type"].as_str(), form["code"].as_str()), (Some("authorization_code"), Some("sf-code")));
        assert_eq!(oauth::pkce_challenge(form["code_verifier"].as_str().unwrap()), q["code_challenge"], "the verifier matches the challenge");
        assert_eq!(form["redirect_uri"].as_str(), Some(q["redirect_uri"].as_str()));
        let settings = s.settings(s.acme).await;
        assert!(settings.salesforce_connected && settings.salesforce_sandbox && !settings.salesforce_needs_reconnect);
        assert_eq!(settings.salesforce_site, "127.0.0.1");
        assert_eq!(settings.salesforce_statuses.as_array().unwrap().len(), 3, "the Lead Status picklist was read");
        let refresh: String = sqlx::query_scalar("select salesforce_refresh_enc from project_settings where project_id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
        assert_eq!(crypto::decrypt("test-secret", &refresh).unwrap(), "refresh-for-sf-code");
        // Used once only, and nothing more is asked of Salesforce.
        assert_eq!(s.callback("acme", &[("code", "sf-code"), ("state", &q["state"])]).await.1["crm_error"], UNKNOWN_STATE);
        assert_eq!(s.fake.token_requests().len(), 1);

        // Zoho names the data centre's accounts server, which the code is traded at.
        let q = s.authorize("zoho", json!({})).await;
        assert_eq!(q["access_type"], "offline");
        let accounts = s.fake.base.clone();
        let back = s.callback("acme", &[("code", "zoho-code"), ("state", &q["state"]), ("location", "eu"), ("accounts-server", &accounts)]).await;
        assert_eq!(back.1["crm"], "zoho");
        assert_eq!(s.fake.token_requests()[1].path, "/oauth/v2/token");
        let settings = s.settings(s.acme).await;
        assert!(settings.zoho_connected);
        assert_eq!(settings.zoho_site, "127.0.0.1");

        // Dynamics asks for the org's own scope.
        let org = format!("{}/main.aspx?appid=1", s.fake.base);
        let q = s.authorize("dynamics", json!({"org_url": org})).await;
        assert_eq!(q["scope"], format!("{}/user_impersonation offline_access", s.fake.base));
        assert_eq!(s.callback("acme", &[("code", "ms-code"), ("state", &q["state"])]).await.1["crm"], "dynamics");
        assert!(s.settings(s.acme).await.dynamics_connected);

        // The new sign-ins sync.
        let lead = crate::models::Lead { email: Some("ana@acme.test".into()), full_name: "Ana Lima".into(), ..crate::twilio::tests::test_lead() };
        crate::crm::sync(&s.state, s.acme, &lead, "Hot.").await;
        assert_eq!((s.fake.records("sf:Lead").len(), s.fake.records("zoho:Lead").len(), s.fake.records("d:lead").len()), (1, 1, 1));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_callback_refuses_bad_states_other_orgs_and_unknown_zoho_servers(db: PgPool) {
        let s = Server::start(db).await;
        let error = |back: (String, HashMap<String, String>)| back.1.get("crm_error").cloned().unwrap_or_default();
        assert_eq!(s.callback("acme", &[("code", "c"), ("state", "made-up.state")]).await.0, "/app");

        // Another org's project, a CRM that doesn't sign in, and an org address that isn't Dynamics.
        assert_eq!(s.start_sign_in("acme", s.theirs, "salesforce", json!({})).await.status(), 404);
        assert_eq!(s.start_sign_in("acme", s.acme, "hubspot", json!({})).await.status(), 404);
        assert_eq!(s.start_sign_in("acme", s.acme, "dynamics", json!({"org_url": "https://evil.test"})).await.status(), 400);

        let q = s.authorize("salesforce", json!({})).await;
        let (nonce, _) = q["state"].split_once('.').unwrap();
        assert_eq!(error(s.callback("acme", &[("code", "c"), ("state", &format!("{nonce}.{}", "0".repeat(64)))]).await), start_again(Crm::Salesforce), "forged");

        let q = s.authorize("salesforce", json!({})).await;
        assert!(error(s.callback("beta", &[("code", "c"), ("state", &q["state"])]).await).starts_with("You switched organizations"));

        let q = s.authorize("salesforce", json!({})).await;
        assert_eq!(error(s.callback("acme", &[("error", "access_denied"), ("state", &q["state"])]).await), "You didn't allow Splee into Salesforce, so nothing was connected.");

        let q = s.authorize("salesforce", json!({})).await;
        sqlx::query("update crm_oauth_states set expires_at = now() - interval '1 second'").execute(&s.db).await.unwrap();
        assert_eq!(error(s.callback("acme", &[("code", "c"), ("state", &q["state"])]).await), start_again(Crm::Salesforce));

        // A mailbox state can't finish a CRM sign-in.
        let q = s.authorize("zoho", json!({})).await;
        let (nonce, _) = q["state"].split_once('.').unwrap();
        let user: Uuid = sqlx::query_scalar("select id from users where email = 'ana@acme.test'").fetch_one(&s.db).await.unwrap();
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
        let mailbox = oauth::sign_state("test-secret", oauth::MAILBOX, nonce, user, org);
        assert_eq!(error(s.callback("acme", &[("code", "c"), ("state", &mailbox)]).await), start_again(Crm::Zoho));

        // The client secret only goes to Zoho's own accounts servers.
        let q = s.authorize("zoho", json!({})).await;
        let back = s.callback("acme", &[("code", "c"), ("state", &q["state"]), ("accounts-server", "https://accounts.evil.test")]).await;
        assert!(error(back).starts_with("Zoho sent the sign-in back from a server Splee doesn't know"));

        assert!(s.fake.token_requests().is_empty(), "no code was traded");
        let settings = s.settings(s.acme).await;
        assert!(!settings.salesforce_connected && !settings.zoho_connected);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn sign_in_crms_take_no_pasted_token_and_test_and_pick_statuses_per_project(db: PgPool) {
        let s = Server::start(db).await;
        let path = |project: Uuid, rest: &str| format!("/projects/{project}/integrations/salesforce{rest}");
        let res = s.call(reqwest::Method::PUT, "acme", &path(s.acme, "")).json(&json!({"token": "pasted"})).send().await.unwrap();
        assert_eq!(res.status(), 400);
        assert_eq!(res.json::<Value>().await.unwrap()["error"], "Connect Salesforce by signing in");

        load_or_create(&s.db, s.acme).await.unwrap();
        connect_fake(&s.state, &s.fake, s.acme, Crm::Salesforce, LIVE, "good", 3600).await;
        let test = s.call(reqwest::Method::POST, "acme", &path(s.acme, "/test")).send().await.unwrap();
        assert_eq!(test.status(), 200);
        assert_eq!(test.json::<Value>().await.unwrap()["salesforce_statuses"].as_array().unwrap().len(), 3);

        let pick = |org: &str, project: Uuid, status: &str| s.call(reqwest::Method::PUT, org, &path(project, "/status")).json(&json!({"status": status})).send();
        assert_eq!(pick("acme", s.acme, "Meeting Set").await.unwrap().status(), 200);
        assert_eq!(pick("acme", s.acme, "Not in the org").await.unwrap().status(), 400);
        assert_eq!(pick("acme", s.theirs, "Meeting Set").await.unwrap().status(), 404);
        assert_eq!(s.settings(s.acme).await.salesforce_booked_status, "Meeting Set");
        assert_eq!(pick("acme", s.acme, "").await.unwrap().status(), 200, "none leaves the status alone");

        // Testing a revoked sign-in says so, and the page then shows it needs reconnecting.
        connect_fake(&s.state, &s.fake, s.acme, Crm::Salesforce, "stale", "revoked", -10).await;
        let test = s.call(reqwest::Method::POST, "acme", &path(s.acme, "/test")).send().await.unwrap();
        assert_eq!(test.status(), 400);
        assert!(test.json::<Value>().await.unwrap()["error"].as_str().unwrap().contains("Salesforce signed Splee out"));
        assert!(s.settings(s.acme).await.salesforce_needs_reconnect);
        assert_eq!(s.call(reqwest::Method::POST, "acme", &path(s.theirs, "/test")).send().await.unwrap().status(), 404);

        // Disconnecting goes through the usual route and forgets it.
        let gone = s.call(reqwest::Method::DELETE, "acme", &path(s.acme, "")).send().await.unwrap();
        assert_eq!(gone.status(), 200);
        let settings = s.settings(s.acme).await;
        assert!(!settings.salesforce_connected && !settings.salesforce_needs_reconnect && settings.salesforce_booked_status.is_empty());
    }
}
