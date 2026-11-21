//! What Zapier, Make and n8n call with an API key to connect: the key's organization for their connection test, and REST
//! hook subscriptions for their triggers (rest_hooks.rs). The integrations page lists a project's subscriptions and removes them.

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get};
use axum::{Json, Router};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

use super::api_v1::check_project;
use super::util_input::blank_as_none;
use crate::audit::{self, Target, Who};
use crate::auth::{Auth, KeyAuth};
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::rest_hooks::{self, EVENTS, LIVE, Subscription};
use crate::state::AppState;

/// Plenty for every zap and scenario an organization runs, and it bounds the posts one event makes.
const MAX_SUBSCRIPTIONS: i64 = 100;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me", get(me))
        .route("/v1/hooks", get(subscriptions).post(subscribe))
        .route("/v1/hooks/{id}", get(subscription).delete(unsubscribe))
        .route("/v1/hooks/samples/{event}", get(samples))
        .route("/projects/{id}/hook-subscriptions", get(list))
        .route("/hook-subscriptions/{id}", delete(remove))
}

/// For each tool's connection test and label. A key belongs to the organization, so it sees all of its projects.
async fn me(State(state): State<AppState>, auth: KeyAuth) -> AppResult<Json<Value>> {
    let (org_name,): (String,) = sqlx::query_as("select name from organizations where id = $1").bind(auth.org_id).fetch_one(&state.db).await?;
    let (key_name, prefix): (String, String) =
        sqlx::query_as("select name, prefix from api_keys where id = $1").bind(auth.key_id).fetch_one(&state.db).await?;
    let projects: Vec<(Uuid, String)> = sqlx::query_as("select id, name from projects where org_id = $1 order by created_at")
        .bind(auth.org_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(json!({
        "organization": {"id": auth.org_id, "name": org_name},
        "api_key": {"id": auth.key_id, "name": key_name, "prefix": prefix},
        "projects": projects.into_iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct SubscribeBody {
    #[serde(default)]
    event: String,
    #[serde(default)]
    target_url: String,
    /// None for every project of the organization.
    #[serde(default, deserialize_with = "blank_as_none")]
    project_id: Option<Uuid>,
}

fn known_event(event: &str) -> AppResult<()> {
    if EVENTS.contains(&event) {
        return Ok(());
    }
    Err(AppError::BadRequest(format!("event must be one of {}", EVENTS.join(", "))))
}

async fn subscribe(State(state): State<AppState>, auth: KeyAuth, Json(body): Json<SubscribeBody>) -> AppResult<Json<Subscription>> {
    known_event(&body.event)?;
    let url = body.target_url.trim();
    let parsed = Url::parse(url).ok().filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some());
    let Some(parsed) = parsed else { return Err(AppError::BadRequest("target_url must be a full URL starting with https://".into())) };
    // Checked now so a tool on a private network hears why at once, and again on every post.
    hooks::public_addrs(&parsed).await.map_err(|e| AppError::BadRequest(format!("Splee can't post to target_url: {e}")))?;
    if let Some(project_id) = body.project_id {
        check_project(&state.db, project_id, auth.org_id).await?;
    }
    let count: i64 =
        sqlx::query_scalar("select count(*) from hook_subscriptions where org_id = $1").bind(auth.org_id).fetch_one(&state.db).await?;
    if count >= MAX_SUBSCRIPTIONS {
        let limit = format!("An organization can have up to {MAX_SUBSCRIPTIONS} subscriptions. Turn off a zap or scenario you no longer use");
        return Err(AppError::BadRequest(limit));
    }
    let subscription = sqlx::query_as::<_, Subscription>(
        "insert into hook_subscriptions (org_id, api_key_id, project_id, event, target_url) values ($1, $2, $3, $4, $5) returning *",
    )
    .bind(auth.org_id)
    .bind(auth.key_id)
    .bind(body.project_id)
    .bind(&body.event)
    .bind(url)
    .fetch_one(&state.db)
    .await?;
    // Only the host: the rest of a Zapier or Make hook URL is as good as a password.
    let host = parsed.host_str().unwrap_or_default();
    let details = json!({ "event": body.event, "host": host, "project_id": body.project_id });
    audit::record(&state.db, &auth, "integration.connected", Target::new("hook_subscription", subscription.id, host), details).await;
    Ok(Json(subscription))
}

/// A subscription as the API shows it. `live` is false once a 410 Gone turned it off or its key was revoked, so events skip it.
#[derive(Serialize, FromRow)]
struct Listed {
    #[serde(flatten)]
    #[sqlx(flatten)]
    subscription: Subscription,
    live: bool,
}

/// The organization's subscriptions, oldest first, or with `project_id` the ones that project's events go to.
async fn subscriptions(State(state): State<AppState>, auth: KeyAuth, Query(q): Query<ProjectQuery>) -> AppResult<Json<Vec<Listed>>> {
    if let Some(project_id) = q.project_id {
        check_project(&state.db, project_id, auth.org_id).await?;
    }
    let rows = sqlx::query_as::<_, Listed>(&format!(
        "select s.*, ({LIVE}) as live from hook_subscriptions s
         where s.org_id = $1 and ($2::uuid is null or s.project_id is null or s.project_id = $2) order by s.created_at, s.id"
    ))
    .bind(auth.org_id)
    .bind(q.project_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

async fn subscription(State(state): State<AppState>, auth: KeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Listed>> {
    sqlx::query_as::<_, Listed>(&format!("select s.*, ({LIVE}) as live from hook_subscriptions s where s.id = $1 and s.org_id = $2"))
        .bind(id)
        .bind(auth.org_id)
        .fetch_optional(&state.db)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound)
}

/// Any key of the organization can remove its subscriptions, so a zap still turns off after its key was replaced.
async fn unsubscribe(State(state): State<AppState>, auth: KeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    delete_in(&state, (&auth).into(), id).await
}

async fn delete_in(state: &AppState, who: Who, id: Uuid) -> AppResult<Json<Value>> {
    let deleted: Option<(String, String)> = sqlx::query_as("delete from hook_subscriptions where id = $1 and org_id = $2 returning event, target_url")
        .bind(id)
        .bind(who.org_id())
        .fetch_optional(&state.db)
        .await?;
    let Some((event, url)) = deleted else { return Err(AppError::NotFound) };
    let host = Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    let details = json!({ "event": event, "host": host });
    audit::record(&state.db, who, "integration.disconnected", Target::new("hook_subscription", id, &host), details).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ProjectQuery {
    #[serde(default, deserialize_with = "blank_as_none")]
    project_id: Option<Uuid>,
}

async fn samples(
    State(state): State<AppState>,
    auth: KeyAuth,
    Path(event): Path<String>,
    Query(q): Query<ProjectQuery>,
) -> AppResult<Json<Vec<Value>>> {
    known_event(&event)?;
    if let Some(project_id) = q.project_id {
        check_project(&state.db, project_id, auth.org_id).await?;
    }
    Ok(Json(rest_hooks::samples(&state.db, &state.cfg.public_url, auth.org_id, q.project_id, &event).await?))
}

/// A subscription on the integrations page, with the key that made it.
#[derive(Serialize, FromRow)]
struct Row {
    #[serde(flatten)]
    #[sqlx(flatten)]
    subscription: Subscription,
    key_name: String,
}

/// The project's live subscriptions and the organization's for every project, oldest first.
async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Row>>> {
    auth.project(&state.db, id).await?;
    let rows = sqlx::query_as::<_, Row>(
        "select s.*, k.name as key_name from hook_subscriptions s join api_keys k on k.id = s.api_key_id
         where s.org_id = $1 and (s.project_id is null or s.project_id = $2) and s.disabled_at is null and k.revoked_at is null
         order by s.created_at",
    )
    .bind(auth.org_id)
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    delete_in(&state, (&auth).into(), id).await
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::Router;
    use axum::http::{HeaderMap, StatusCode, Uri};
    use hmac::{Hmac, KeyInit, Mac};
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use uuid::Uuid;

    use crate::auth::Auth;
    use crate::crypto::sha256_hex;
    use crate::error::AppError;
    use crate::hooks;
    use crate::models::Lead;
    use crate::state::AppState;

    const KEY_A: &str = "splee_hooks_key_a";
    const KEY_B: &str = "splee_hooks_key_b";

    struct Api {
        base: String,
        client: reqwest::Client,
    }

    impl Api {
        async fn start(state: AppState) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", super::router().merge(super::super::api_v1::router(&state))).with_state(state);
            tokio::spawn(async move { axum::serve(listener, app).await });
            Self { base, client: reqwest::Client::new() }
        }

        async fn send(&self, method: reqwest::Method, key: &str, path: &str, body: Option<Value>) -> (u16, Value) {
            let mut req = self.client.request(method, format!("{}{path}", self.base)).bearer_auth(key);
            if let Some(body) = body {
                req = req.json(&body);
            }
            let res = req.send().await.unwrap();
            let status = res.status().as_u16();
            (status, res.json().await.unwrap_or(Value::Null))
        }

        async fn get(&self, key: &str, path: &str) -> (u16, Value) {
            self.send(reqwest::Method::GET, key, path, None).await
        }

        async fn post(&self, key: &str, path: &str, body: Value) -> (u16, Value) {
            self.send(reqwest::Method::POST, key, path, Some(body)).await
        }

        async fn patch(&self, key: &str, path: &str, body: Value) -> (u16, Value) {
            self.send(reqwest::Method::PATCH, key, path, Some(body)).await
        }

        async fn delete(&self, key: &str, path: &str) -> (u16, Value) {
            self.send(reqwest::Method::DELETE, key, path, None).await
        }
    }

    /// What a receiver got: the path, the headers and the raw body.
    type Got = Arc<Mutex<Vec<(String, HeaderMap, String)>>>;

    /// A stand-in for Zapier on this machine. It answers 410 Gone on /gone, like Zapier for a deleted zap, and 200 elsewhere.
    async fn receiver() -> (String, Got) {
        let got: Got = Arc::default();
        let keep = got.clone();
        let app = Router::new().fallback(move |uri: Uri, headers: HeaderMap, body: String| {
            let keep = keep.clone();
            async move {
                keep.lock().unwrap().push((uri.path().to_string(), headers, body));
                if uri.path() == "/gone" { StatusCode::GONE } else { StatusCode::OK }
            }
        });
        let port = hooks::serve_test_receiver(app).await;
        (format!("http://127.0.0.1:{port}"), got)
    }

    /// Waits for the receiver to have `n` posts, or a few seconds.
    async fn wait_for(got: &Got, n: usize) {
        for _ in 0..100 {
            if got.lock().unwrap().len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    struct Org {
        id: Uuid,
        project: Uuid,
        segment: Uuid,
    }

    /// Two organizations, each with a project, a campaign and a key: Acme has KEY_A and Other has KEY_B.
    async fn seed(db: &PgPool) -> (Org, Org) {
        let mut orgs = vec![];
        for (name, key) in [("Acme", KEY_A), ("Other", KEY_B)] {
            let id: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id")
                .bind(id)
                .bind(format!("{name} project"))
                .fetch_one(db)
                .await
                .unwrap();
            let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Dentists') returning id")
                .bind(project)
                .fetch_one(db)
                .await
                .unwrap();
            sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'Zapier', 'splee_hooks_', $2)")
                .bind(id)
                .bind(sha256_hex(key))
                .execute(db)
                .await
                .unwrap();
            orgs.push(Org { id, project, segment });
        }
        let b = orgs.pop().unwrap();
        (orgs.pop().unwrap(), b)
    }

    async fn lead(db: &PgPool, project: Uuid, email: &str) -> Lead {
        sqlx::query_as("insert into leads (project_id, full_name, email, status) values ($1, 'Sarah Chen', $2, 'hot') returning *")
            .bind(project)
            .bind(email)
            .fetch_one(db)
            .await
            .unwrap()
    }

    fn state(db: &PgPool) -> AppState {
        crate::autopilot::tests::state(db.clone())
    }

    fn signature(secret: &str, timestamp: &str, body: &str) -> String {
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("{timestamp}.{body}").as_bytes());
        format!("v1={}", hex::encode(mac.finalize().into_bytes()))
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn subscribe_unsubscribe_and_samples(db: PgPool) {
        let (a, b) = seed(&db).await;
        let api = Api::start(state(&db)).await;
        let (url, _) = receiver().await;

        let (status, me) = api.get(KEY_A, "/v1/me").await;
        assert_eq!(status, 200, "{me}");
        assert_eq!(me["organization"], json!({"id": a.id, "name": "Acme"}));
        assert_eq!(me["api_key"]["name"], "Zapier");
        assert_eq!(me["projects"], json!([{"id": a.project, "name": "Acme project"}]));
        assert_eq!(api.get("splee_wrong", "/v1/me").await.0, 401);

        let hook = |event: &str, target: &str, project: Option<Uuid>| json!({"event": event, "target_url": target, "project_id": project});
        let (status, refused) = api.post(KEY_A, "/v1/hooks", hook("lead_deleted", &url, None)).await;
        assert_eq!((status, refused["error"].as_str().unwrap().starts_with("event must be one of hot_lead")), (400, true));
        assert_eq!(api.post(KEY_A, "/v1/hooks", hook("hot_lead", "not a url", None)).await.0, 400);
        let (status, private) = api.post(KEY_A, "/v1/hooks", hook("hot_lead", "http://10.1.2.3/hook", None)).await;
        assert_eq!((status, private["error"].as_str().unwrap().contains("is not a public address")), (400, true), "{private}");
        assert_eq!(api.post(KEY_A, "/v1/hooks", hook("hot_lead", &url, Some(b.project))).await.0, 404, "another organization's project");

        let blank = json!({"event": "hot_lead", "target_url": format!("{url}/every"), "project_id": ""});
        assert_eq!(api.post(KEY_A, "/v1/hooks", blank).await.1["project_id"], Value::Null, "Make sends an empty field as \"\"");
        let (status, made) = api.post(KEY_A, "/v1/hooks", hook("deal.won", &format!("{url}/zap"), Some(a.project))).await;
        assert_eq!(status, 200, "{made}");
        assert_eq!((made["event"].as_str(), made["project_id"].as_str()), (Some("deal.won"), Some(a.project.to_string().as_str())));
        let id = made["id"].as_str().unwrap().to_string();

        // Listing shows the organization's, and with project_id the ones its events go to. One turned off by a 410 isn't live.
        let gone = api.post(KEY_A, "/v1/hooks", json!({"event": "hot_lead", "target_url": format!("{url}/gone")})).await.1;
        let gone = Uuid::parse_str(gone["id"].as_str().unwrap()).unwrap();
        sqlx::query("update hook_subscriptions set disabled_at = now() where id = $1").bind(gone).execute(&db).await.unwrap();
        let (status, listed) = api.get(KEY_A, &format!("/v1/hooks?project_id={}", a.project)).await;
        assert_eq!(status, 200, "{listed}");
        let shown: Vec<(&str, bool)> = listed.as_array().unwrap().iter().map(|h| (h["target_url"].as_str().unwrap(), h["live"] == true)).collect();
        assert_eq!(shown, [(format!("{url}/every").as_str(), true), (format!("{url}/zap").as_str(), true), (format!("{url}/gone").as_str(), false)]);
        assert_eq!(api.get(KEY_A, &format!("/v1/hooks?project_id={}", b.project)).await.0, 404);
        assert_eq!(api.get(KEY_B, "/v1/hooks").await, (200, json!([])));
        let (status, one) = api.get(KEY_A, &format!("/v1/hooks/{id}")).await;
        assert_eq!((status, one["event"].as_str(), one["live"].as_bool()), (200, Some("deal.won"), Some(true)));
        assert_eq!(api.get(KEY_B, &format!("/v1/hooks/{id}")).await.0, 404, "another organization's subscription");

        assert_eq!(api.delete(KEY_B, &format!("/v1/hooks/{id}")).await.0, 404, "another organization's subscription");
        assert_eq!(api.delete(KEY_A, &format!("/v1/hooks/{id}")).await, (200, json!({"ok": true})));
        assert_eq!(api.delete(KEY_A, &format!("/v1/hooks/{id}")).await.0, 404);

        // Nothing happened yet, so each event shows one example.
        let (status, examples) = api.get(KEY_A, "/v1/hooks/samples/deal.won").await;
        assert_eq!(status, 200);
        assert_eq!(examples.as_array().unwrap().len(), 1);
        assert_eq!((examples[0]["event"].as_str(), examples[0]["deal"]["status"].as_str()), (Some("deal.won"), Some("won")));
        assert_eq!(api.get(KEY_A, "/v1/hooks/samples/lead_added").await.1[0]["lead"]["full_name"], "Sarah Chen");
        assert_eq!(api.get(KEY_A, "/v1/hooks/samples/nothing").await.0, 400);
        assert_eq!(api.get(KEY_A, &format!("/v1/hooks/samples/hot_lead?project_id={}", b.project)).await.0, 404);

        // Then the real ones: new leads, and what was sent, the newest first. Another organization's never show.
        let real = lead(&db, a.project, "ana@acme.example").await;
        lead(&db, b.project, "bo@other.example").await;
        let (_, added) = api.get(KEY_A, "/v1/hooks/samples/lead_added").await;
        assert_eq!(added.as_array().unwrap().iter().map(|p| p["lead"]["id"].clone()).collect::<Vec<_>>(), [json!(real.id)]);
        for n in 0..2 {
            hooks::log_delivery(&db, a.project, "hot_lead", "https://hooks.zapier.com/1", &json!({"event": "hot_lead", "n": n}), &Ok(())).await.unwrap();
        }
        hooks::log_delivery(&db, b.project, "hot_lead", "https://hooks.zapier.com/2", &json!({"event": "hot_lead", "n": 9}), &Ok(())).await.unwrap();
        let (_, hot) = api.get(KEY_A, &format!("/v1/hooks/samples/hot_lead?project_id={}", a.project)).await;
        assert_eq!(hot, json!([{"event": "hot_lead", "n": 1}, {"event": "hot_lead", "n": 0}]));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn events_reach_only_their_subscriptions_signed(db: PgPool) {
        let (a, _) = seed(&db).await;
        let st = state(&db);
        let api = Api::start(st.clone()).await;
        let (url, got) = receiver().await;
        let subscribe = |key: &'static str, event: &'static str, path: &'static str, project: Option<Uuid>| {
            api.post(key, "/v1/hooks", json!({"event": event, "target_url": format!("{url}{path}"), "project_id": project}))
        };
        assert_eq!(subscribe(KEY_A, "hot_lead", "/acme", Some(a.project)).await.0, 200);
        assert_eq!(subscribe(KEY_A, "lead_replied", "/replies", None).await.0, 200);
        // Every project of Other, which mustn't hear about Acme's.
        assert_eq!(subscribe(KEY_B, "hot_lead", "/other", None).await.0, 200);

        let sarah = lead(&db, a.project, "sarah@northwind.example").await;
        let reply = json!({"id": Uuid::new_v4(), "subject": "Re: hi", "body": "Tuesday?", "classification": "hot", "reason": "Wants a call"});
        hooks::send_lead_event(&st, a.project, "hot_lead", &sarah, Some(("reply", reply))).await;
        wait_for(&got, 1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let posts = got.lock().unwrap().clone();
        assert_eq!(posts.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["/acme"]);
        let (_, headers, body) = &posts[0];
        let secret: String =
            sqlx::query_scalar("select webhook_secret from project_settings where project_id = $1").bind(a.project).fetch_one(&db).await.unwrap();
        let timestamp = headers["splee-timestamp"].to_str().unwrap();
        assert_eq!(headers["splee-signature"].to_str().unwrap(), signature(&secret, timestamp, body));
        let payload: Value = serde_json::from_str(body).unwrap();
        assert_eq!((payload["event"].as_str(), payload["lead"]["email"].as_str()), (Some("hot_lead"), Some("sarah@northwind.example")));
        assert_eq!((payload["project"]["name"].as_str(), payload["reply"]["body"].as_str()), (Some("Acme project"), Some("Tuesday?")));
        let logged: Vec<String> =
            sqlx::query_scalar("select url from webhook_deliveries where project_id = $1").bind(a.project).fetch_all(&db).await.unwrap();
        assert_eq!(logged, [format!("{url}/acme")]);

        // A subscription for every project hears about each, and one whose key was revoked hears nothing.
        let other_project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Second') returning id").bind(a.id).fetch_one(&db).await.unwrap();
        let ben = lead(&db, other_project, "ben@second.example").await;
        hooks::send_lead_event(&st, other_project, "lead_replied", &ben, None).await;
        wait_for(&got, 2).await;
        assert_eq!(got.lock().unwrap()[1].0, "/replies");
        sqlx::query("update api_keys set revoked_at = now() where org_id = $1").bind(a.id).execute(&db).await.unwrap();
        hooks::send_lead_event(&st, a.project, "hot_lead", &sarah, None).await;
        hooks::send_lead_event(&st, other_project, "lead_replied", &ben, None).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(got.lock().unwrap().len(), 2);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn erasing_leads_posts_only_their_ids_in_order(db: PgPool) {
        let (a, _) = seed(&db).await;
        let api = Api::start(state(&db)).await;
        let (url, got) = receiver().await;
        let hook = json!({"event": "lead_erased", "target_url": format!("{url}/zap"), "project_id": a.project});
        assert_eq!(api.post(KEY_A, "/v1/hooks", hook).await.0, 200);
        // The project's own webhook endpoint, sending this event only.
        sqlx::query(
            "insert into project_settings (project_id, webhook_events) values ($1, '{lead_erased}')
             on conflict (project_id) do update set webhook_events = excluded.webhook_events",
        )
        .bind(a.project)
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("insert into webhook_endpoints (project_id, url) values ($1, $2)")
            .bind(a.project)
            .bind(format!("{url}/endpoint"))
            .execute(&db)
            .await
            .unwrap();
        let mut sarah = lead(&db, a.project, "sarah@northwind.example").await;
        sqlx::query("update leads set segment_id = $2 where id = $1").bind(sarah.id).bind(a.segment).execute(&db).await.unwrap();
        sarah.segment_id = Some(a.segment);
        let ben = lead(&db, a.project, "ben@northwind.example").await;

        super::super::leads::erase(&db, a.id, &[sarah.clone(), ben.clone()], true).await.unwrap();
        wait_for(&got, 4).await;
        let posts = got.lock().unwrap().clone();
        for path in ["/zap", "/endpoint"] {
            let sent: Vec<Value> = posts.iter().filter(|p| p.0 == path).map(|p| serde_json::from_str(&p.2).unwrap()).collect();
            let leads: Vec<(&str, &Value, &Value)> = sent.iter().map(|p| (p["event"].as_str().unwrap(), &p["lead"], &p["forgotten"])).collect();
            let (sarah_ids, ben_ids) = (json!({"id": sarah.id, "campaign_id": a.segment}), json!({"id": ben.id, "campaign_id": null}));
            assert_eq!(leads, [("lead_erased", &sarah_ids, &json!(true)), ("lead_erased", &ben_ids, &json!(true))], "{path}");
        }
        assert!(posts.iter().all(|p| !p.2.contains("northwind") && !p.2.contains("Sarah")), "nothing that names them");

        // Before anything was sent, a trigger's setup screen gets an example of the same shape.
        let (status, samples) = api.get(KEY_B, "/v1/hooks/samples/lead_erased").await;
        assert_eq!((status, samples[0]["event"].as_str()), (200, Some("lead_erased")), "{samples}");
        let mut keys: Vec<&String> = samples[0]["lead"].as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["campaign_id", "id"]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_410_turns_the_subscription_off(db: PgPool) {
        let (a, _) = seed(&db).await;
        let st = state(&db);
        let api = Api::start(st.clone()).await;
        let (url, got) = receiver().await;
        let (_, gone) = api.post(KEY_A, "/v1/hooks", json!({"event": "meeting_booked", "target_url": format!("{url}/gone")})).await;
        let (_, kept) = api.post(KEY_A, "/v1/hooks", json!({"event": "meeting_booked", "target_url": format!("{url}/kept")})).await;
        let sarah = lead(&db, a.project, "sarah@northwind.example").await;

        hooks::send_lead_event(&st, a.project, "meeting_booked", &sarah, None).await;
        wait_for(&got, 2).await;
        let off = || async {
            for _ in 0..100 {
                let off: Vec<(Uuid, bool, Option<String>)> =
                    sqlx::query_as("select id, disabled_at is not null, error from hook_subscriptions order by created_at").fetch_all(&db).await.unwrap();
                if off[0].1 {
                    return off;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            panic!("never turned off");
        };
        let off = off().await;
        assert_eq!(off[0].0.to_string(), gone["id"].as_str().unwrap());
        assert_eq!(off[0].2.as_deref(), Some("410 Gone"));
        assert_eq!((off[1].0.to_string(), off[1].1), (kept["id"].as_str().unwrap().to_string(), false));

        hooks::send_lead_event(&st, a.project, "meeting_booked", &sarah, None).await;
        wait_for(&got, 3).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(got.lock().unwrap().iter().map(|p| p.0.as_str()).filter(|p| *p == "/gone").count(), 1);
        let owner = Auth { user_id: Uuid::new_v4(), org_id: a.id, role: "owner".into(), token_hash: String::new() };
        let listed = super::list(axum::extract::State(st), owner, axum::extract::Path(a.project)).await.unwrap().0;
        assert_eq!(listed.iter().map(|r| r.subscription.target_url.as_str()).collect::<Vec<_>>(), [format!("{url}/kept")]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn new_leads_are_posted_once(db: PgPool) {
        let (a, b) = seed(&db).await;
        let st = state(&db);
        let api = Api::start(st.clone()).await;
        let (url, got) = receiver().await;
        let added = json!({"event": "lead_added", "target_url": format!("{url}/added"), "project_id": a.project});
        let (status, _) = api.post(KEY_A, "/v1/hooks", added).await;
        assert_eq!(status, 200);
        sqlx::query("update hook_subscriptions set cursor_at = now() - interval '10 minutes'").execute(&db).await.unwrap();
        let add = |project: Uuid, email: &'static str, ago: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, email, created_at) values ($1, $2, now() - $3::interval) returning id")
                .bind(project)
                .bind(email)
                .bind(ago)
                .fetch_one(&db)
        };
        add(a.project, "before@acme.example", "20 minutes").await.unwrap();
        let new = add(a.project, "new@acme.example", "5 minutes").await.unwrap();
        add(b.project, "other@other.example", "5 minutes").await.unwrap();
        // Still being saved, as far as the sweep knows, so it waits for the next one.
        let fresh = add(a.project, "fresh@acme.example", "0 seconds").await.unwrap();

        crate::rest_hooks::post_new_leads(&st).await.unwrap();
        crate::rest_hooks::post_new_leads(&st).await.unwrap();
        let posts = got.lock().unwrap().clone();
        let ids: Vec<Value> = posts.iter().map(|p| serde_json::from_str::<Value>(&p.2).unwrap()["lead"]["id"].clone()).collect();
        assert_eq!(ids, [json!(new)]);
        assert_eq!(serde_json::from_str::<Value>(&posts[0].2).unwrap()["event"], "lead_added");
        assert!(posts[0].1.contains_key("splee-signature"));

        sqlx::query("update leads set created_at = now() - interval '1 minute' where id = $1").bind(fresh).execute(&db).await.unwrap();
        crate::rest_hooks::post_new_leads(&st).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&got.lock().unwrap()[1].2).unwrap()["lead"]["id"], json!(fresh));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_actions_zapier_make_and_n8n_call(db: PgPool) {
        let (a, b) = seed(&db).await;
        let api = Api::start(state(&db)).await;
        let upsert = format!("/v1/projects/{}/leads/upsert", a.project);

        let (status, made) = api.post(KEY_A, &upsert, json!({"email": "Sarah@Northwind.example", "full_name": "Sarah Chen", "title": "Founder"})).await;
        assert_eq!(status, 200, "{made}");
        assert_eq!(
            (made["created"].as_bool(), made["email"].as_str(), made["first_name"].as_str()),
            (Some(true), Some("sarah@northwind.example"), Some("Sarah"))
        );
        let (status, updated) =
            api.post(KEY_A, &upsert, json!({"email": "sarah@northwind.example", "title": "CEO", "segment_id": a.segment})).await;
        assert_eq!(status, 200, "{updated}");
        assert_eq!((updated["created"].as_bool(), updated["id"].clone()), (Some(false), made["id"].clone()));
        assert_eq!((updated["title"].as_str(), updated["full_name"].as_str()), (Some("CEO"), Some("Sarah Chen")), "empty fields keep theirs");
        assert_eq!(updated["segment_id"], json!(a.segment));
        // A new country is saved and reads the phone number sent with it.
        let (_, moved) = api.post(KEY_A, &upsert, json!({"email": "sarah@northwind.example", "country_code": "gb", "phone": "07700 900123"})).await;
        assert_eq!((moved["country_code"].as_str(), moved["phone"].as_str()), (Some("GB"), Some("+447700900123")));
        let error = |(status, body): (u16, Value)| (status, body["error"].as_str().unwrap_or_default().to_string());
        assert_eq!(error(api.post(KEY_A, &upsert, json!({"full_name": "No Email"})).await), (400, "Send the lead's email".into()));
        assert_eq!(error(api.post(KEY_A, &upsert, json!({"email": "not-an-email"})).await), (400, "Invalid email: not-an-email".into()));
        let foreign = json!({"email": "x@y.example", "segment_id": b.segment});
        assert_eq!(error(api.post(KEY_A, &upsert, foreign).await), (400, "That campaign isn't part of this project, or it's archived".into()));
        assert_eq!(api.post(KEY_A, &format!("/v1/projects/{}/leads/upsert", b.project), json!({"email": "x@y.example"})).await.0, 404);

        // Do not contact, then adding that address is refused.
        assert_eq!(api.post(KEY_A, "/v1/suppressions", json!({"values": ["stop@north.example"]})).await, (200, json!({"added": 1})));
        assert_eq!(error(api.post(KEY_A, &upsert, json!({"email": "stop@north.example"})).await), (400, "Suppressed: stop@north.example".into()));
        assert_eq!(api.post(KEY_A, "/v1/suppressions", json!({"values": ["nope"]})).await.0, 400);
        let other_list: i64 = sqlx::query_scalar("select count(*) from suppressions where org_id = $1").bind(b.id).fetch_one(&db).await.unwrap();
        assert_eq!(other_list, 0);

        // Find by email, then set the status and the owner.
        let (status, found) = api.get(KEY_A, "/v1/leads?email=SARAH@northwind.example").await;
        assert_eq!((status, found[0]["id"].clone()), (200, made["id"].clone()));
        assert_eq!(api.get(KEY_B, "/v1/leads?email=sarah@northwind.example").await, (200, json!([])));
        assert_eq!(api.get(KEY_A, "/v1/leads?email=").await.0, 400);
        let (_, here) = api.get(KEY_A, &format!("/v1/leads?email=sarah@northwind.example&project_id={}", a.project)).await;
        assert_eq!(here.as_array().unwrap().len(), 1);
        assert_eq!(api.get(KEY_A, &format!("/v1/leads?email=sarah@northwind.example&project_id={}", b.project)).await, (200, json!([])));
        assert_eq!(api.get(KEY_A, "/v1/leads?email=sarah@northwind.example&project_id=").await.1.as_array().unwrap().len(), 1, "blank is none");
        let lead = format!("/v1/leads/{}", made["id"].as_str().unwrap());
        assert_eq!(api.patch(KEY_A, &lead, json!({"status": "meeting_booked"})).await.1["status"], "meeting_booked");
        assert_eq!(api.patch(KEY_A, &lead, json!({"status": "keen"})).await.0, 400);
        assert_eq!(api.patch(KEY_B, &lead, json!({"status": "hot"})).await.0, 404);
        let user: Uuid =
            sqlx::query_scalar("insert into users (email, password_hash) values ('alex@acme.example', 'x') returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(a.id).bind(user).execute(&db).await.unwrap();
        let (status, owned) = api.post(KEY_A, &format!("{lead}/assign"), json!({"owner": "alex@acme.example"})).await;
        assert_eq!((status, owned["owner_id"].clone()), (200, json!(user)), "{owned}");
        assert_eq!(error(api.post(KEY_A, &format!("{lead}/assign"), json!({"owner": "who@else.example"})).await).0, 400);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_integrations_page_lists_and_removes_its_own(db: PgPool) {
        let (a, b) = seed(&db).await;
        let st = state(&db);
        let key = |org: Uuid| sqlx::query_scalar::<_, Uuid>("select id from api_keys where org_id = $1").bind(org).fetch_one(&db);
        let (key_a, key_b) = (key(a.id).await.unwrap(), key(b.id).await.unwrap());
        let second: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Second') returning id").bind(a.id).fetch_one(&db).await.unwrap();
        let add = |org: Uuid, key: Uuid, project: Option<Uuid>, url: &'static str| {
            sqlx::query_scalar::<_, Uuid>(
                "insert into hook_subscriptions (org_id, api_key_id, project_id, event, target_url) values ($1, $2, $3, 'hot_lead', $4) returning id",
            )
            .bind(org)
            .bind(key)
            .bind(project)
            .bind(url)
            .fetch_one(&db)
        };
        add(a.id, key_a, Some(a.project), "https://hooks.zapier.com/this").await.unwrap();
        add(a.id, key_a, None, "https://hook.eu1.make.com/every").await.unwrap();
        add(a.id, key_a, Some(second), "https://hooks.zapier.com/second").await.unwrap();
        let theirs = add(b.id, key_b, None, "https://hooks.zapier.com/theirs").await.unwrap();
        let owner = |org: Uuid| Auth { user_id: Uuid::new_v4(), org_id: org, role: "owner".into(), token_hash: String::new() };
        let listed = super::list(axum::extract::State(st.clone()), owner(a.id), axum::extract::Path(a.project)).await.unwrap().0;
        let urls: Vec<&str> = listed.iter().map(|r| r.subscription.target_url.as_str()).collect();
        assert_eq!(urls, ["https://hooks.zapier.com/this", "https://hook.eu1.make.com/every"]);
        assert_eq!(listed[0].key_name, "Zapier");
        let refused = super::list(axum::extract::State(st.clone()), owner(a.id), axum::extract::Path(b.project)).await;
        assert!(matches!(refused, Err(AppError::NotFound)));

        let refused = super::remove(axum::extract::State(st.clone()), owner(a.id), axum::extract::Path(theirs)).await;
        assert!(matches!(refused, Err(AppError::NotFound)));
        let removed = super::remove(axum::extract::State(st.clone()), owner(a.id), axum::extract::Path(listed[0].subscription.id)).await.unwrap();
        assert_eq!(removed.0, json!({"ok": true}));
        let left: i64 = sqlx::query_scalar("select count(*) from hook_subscriptions").fetch_one(&db).await.unwrap();
        assert_eq!(left, 3);
    }
}
