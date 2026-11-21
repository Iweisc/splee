use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::post;
use reqwest::Method;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::*;
use crate::crypto::sha256_hex;

fn state(db: &PgPool) -> AppState {
    let provider = crate::mail::oauth::tests::FakeProvider { url: String::new(), hits: Arc::default(), forms: Arc::default() };
    let cfg = crate::config::Config { public_url: "https://splee.test".into(), ..crate::mail::oauth::tests::config(&provider) };
    AppState { db: db.clone(), cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(1).0, rate_limits: Default::default() }
}

async fn org(db: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap()
}

async fn project(db: &PgPool, org: Uuid) -> Uuid {
    sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Fieldnote') returning id").bind(org).fetch_one(db).await.unwrap()
}

async fn segment(db: &PgPool, project: Uuid) -> Uuid {
    sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Dentists') returning id").bind(project).fetch_one(db).await.unwrap()
}

/// A member signed in with the session "session-{name}".
async fn member(db: &PgPool, org: Uuid, name: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, $2, 'x') returning id")
        .bind(format!("{name}@splee.test"))
        .bind(name)
        .fetch_one(db)
        .await
        .unwrap();
    sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(org).bind(id).execute(db).await.unwrap();
    sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
        .bind(sha256_hex(&format!("session-{name}")))
        .bind(id)
        .bind(org)
        .execute(db)
        .await
        .unwrap();
    id
}

async fn company(db: &PgPool, domain: &str, city: &str, confidence: f32) {
    sqlx::query(
        "insert into directory_companies (domain, name, website, city, country_code, category, categories, confidence, source_id, release)
         values ($1, $1, 'https://' || $1, $2, 'GB', 'dental_clinic', '{dental_clinic}', $3, 'x', 'r1')",
    )
    .bind(domain)
    .bind(city)
    .bind(confidence)
    .execute(db)
    .await
    .unwrap();
}

/// Dentists in Leeds, owned by nobody, checked by the caller.
async fn saved(db: &PgPool, project: Uuid, segment: Option<Uuid>, chat_alerts: bool) -> SavedSearch {
    sqlx::query_as(
        "insert into saved_searches (project_id, name, category, city, chat_alerts, add_to_segment_id)
         values ($1, 'Dentists in Leeds', 'dental clinic', 'Leeds', $2, $3) returning *",
    )
    .bind(project)
    .bind(chat_alerts)
    .bind(segment)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn reload(db: &PgPool, id: Uuid) -> SavedSearch {
    sqlx::query_as("select * from saved_searches where id = $1").bind(id).fetch_one(db).await.unwrap()
}

async fn found(db: &PgPool, id: Uuid) -> Vec<(String, bool)> {
    sqlx::query_as("select domain, found_at is not null from saved_search_matches where search_id = $1 order by domain").bind(id).fetch_all(db).await.unwrap()
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_first_check_is_a_silent_baseline_and_a_new_company_is_found_once(db: PgPool) {
    let project = project(&db, org(&db, "Acme").await).await;
    company(&db, "smile.co.uk", "Leeds", 0.9).await;
    company(&db, "grin.co.uk", "LEEDS", 0.8).await;
    company(&db, "london.co.uk", "London", 0.9).await;
    let search = saved(&db, project, None, false).await;

    assert_eq!(check(&db, &search).await.unwrap(), None, "the baseline says nothing");
    assert_eq!(found(&db, search.id).await, [("grin.co.uk".to_string(), false), ("smile.co.uk".to_string(), false)]);
    let search = reload(&db, search.id).await;
    assert!(search.baseline_at.is_some());
    assert_eq!(check(&db, &search).await.unwrap(), None, "nothing new");

    company(&db, "beam.co.uk", "Leeds", 0.5).await;
    // A new Overture release rewrites the rows already there. They aren't new.
    sqlx::query("update directory_companies set release = 'r2', updated_at = now() where domain <> 'beam.co.uk'").execute(&db).await.unwrap();
    let news = check(&db, &search).await.unwrap().expect("the new company");
    assert_eq!((news.new, news.added.clone()), (1, None));
    assert_eq!(summary(&search, &news), "1 new company matches Dentists in Leeds.");
    let first: Ts = sqlx::query_scalar("select found_at from saved_search_matches where domain = 'beam.co.uk'").fetch_one(&db).await.unwrap();
    assert_eq!(first, news.at);
    assert_eq!(check(&db, &search).await.unwrap(), None, "found once");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn only_searches_due_are_checked_and_a_deleted_one_stops(db: PgPool) {
    let project = project(&db, org(&db, "Acme").await).await;
    let search = saved(&db, project, None, false).await;
    sqlx::query("update saved_searches set checked_at = now() - interval '2 hours' where id = $1").bind(search.id).execute(&db).await.unwrap();
    let due: Vec<Uuid> = claim(&db).await.unwrap().iter().map(|s| s.id).collect();
    assert_eq!(due, [search.id]);
    assert!(claim(&db).await.unwrap().is_empty(), "claimed for the next hour");

    company(&db, "smile.co.uk", "Leeds", 0.9).await;
    check(&db, &search).await.unwrap();
    assert_eq!(found(&db, search.id).await.len(), 1);
    sqlx::query("update saved_searches set checked_at = now() - interval '2 hours' where id = $1").bind(search.id).execute(&db).await.unwrap();
    sqlx::query("delete from saved_searches where id = $1").bind(search.id).execute(&db).await.unwrap();
    company(&db, "grin.co.uk", "Leeds", 0.9).await;
    assert!(claim(&db).await.unwrap().is_empty());
    tick(&state(&db)).await.unwrap();
    let left: i64 = sqlx::query_scalar("select count(*) from saved_search_matches").fetch_one(&db).await.unwrap();
    assert_eq!(left, 0, "its matches went with it, and nothing checks it");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_search_past_the_cap_is_not_watched(db: PgPool) {
    let project = project(&db, org(&db, "Acme").await).await;
    sqlx::query(
        "insert into directory_companies (domain, name, website, city, country_code, category, categories, source_id, release)
         select 'd' || n || '.co.uk', 'Dentist ' || n, 'https://d' || n || '.co.uk', 'Leeds', 'GB', 'dental_clinic', '{dental_clinic}', 'x', 'r'
         from generate_series(1, $1) n",
    )
    .bind(SEEN_CAP as i32 + 1)
    .execute(&db)
    .await
    .unwrap();
    let search = saved(&db, project, None, false).await;
    assert_eq!(check(&db, &search).await.unwrap(), None);
    let search = reload(&db, search.id).await;
    assert!(search.too_broad);
    assert!(found(&db, search.id).await.is_empty(), "nothing kept for a search that isn't watched");
    sqlx::query("update saved_searches set checked_at = null").execute(&db).await.unwrap();
    assert!(claim(&db).await.unwrap().is_empty());
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn new_matches_go_into_the_campaign_past_suppressions_and_up_to_the_cap(db: PgPool) {
    let org = org(&db, "Acme").await;
    let project = project(&db, org).await;
    let campaign = segment(&db, project).await;
    let search = saved(&db, project, Some(campaign), false).await;
    check(&db, &search).await.unwrap();
    let search = reload(&db, search.id).await;

    // The most reliable come first, so the cap's first 200 hold a do-not-contact one and one the project has.
    company(&db, "blocked.co.uk", "Leeds", 0.99).await;
    company(&db, "known.co.uk", "Leeds", 0.99).await;
    sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, 'blocked.co.uk', 'domain', 'manual')").bind(org).execute(&db).await.unwrap();
    sqlx::query("insert into companies (project_id, domain) values ($1, 'known.co.uk')").bind(project).execute(&db).await.unwrap();
    sqlx::query(
        "insert into directory_companies (domain, name, website, city, country_code, category, categories, confidence, source_id, release)
         select 'd' || lpad(n::text, 3, '0') || '.co.uk', 'Dentist ' || n, 'https://d' || n || '.co.uk', 'Leeds', 'GB', 'dental_clinic',
                '{dental_clinic}', 0.5, 'x', 'r'
         from generate_series(1, 205) n",
    )
    .execute(&db)
    .await
    .unwrap();

    let news = check(&db, &search).await.unwrap().unwrap();
    assert_eq!((news.new, news.added.clone()), (207, Some(("Dentists".to_string(), 198))));
    assert_eq!(summary(&search, &news), "207 new companies match Dentists in Leeds. Added 198 of the first 200 to Dentists.");
    let (added, last, blocked): (i64, Option<String>, i64) = sqlx::query_as(
        "select count(*) filter (where segment_id = $2), max(domain::text) filter (where segment_id = $2), count(*) filter (where domain = 'blocked.co.uk')
         from companies where project_id = $1",
    )
    .bind(project)
    .bind(campaign)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!((added, last.as_deref(), blocked), (198, Some("d198.co.uk"), 0));
    let flood: i64 = sqlx::query_scalar("select count(*) from saved_search_matches where search_id = $1 and found_at is not null").bind(search.id).fetch_one(&db).await.unwrap();
    assert_eq!(flood, 207, "the rest are still new matches, just not added");
}

/// A chat on this machine that keeps the bodies it's sent.
async fn chat(db: &PgPool, project: Uuid) -> Arc<Mutex<Vec<String>>> {
    let posts: Arc<Mutex<Vec<String>>> = Arc::default();
    let keep = posts.clone();
    let app = Router::new().route(
        "/chat",
        post(move |body: String| {
            let keep = keep.clone();
            async move { keep.lock().unwrap().push(body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    crate::hooks::TEST_RECEIVERS.lock().unwrap().push(port);
    tokio::spawn(async move { axum::serve(listener, app).await });
    sqlx::query("insert into chat_channels (project_id, url) values ($1, $2)").bind(project).bind(format!("http://127.0.0.1:{port}/chat")).execute(db).await.unwrap();
    posts
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_chat_hears_only_when_the_search_asks(db: PgPool) {
    let state = state(&db);
    let project = project(&db, org(&db, "Acme").await).await;
    let posts = chat(&db, project).await;
    let news = Found { at: "2026-11-21T09:00:00Z".parse().unwrap(), new: 3, added: None };

    announce(&state, &saved(&db, project, None, false).await, &news).await;
    assert!(posts.lock().unwrap().is_empty());
    let search = saved(&db, project, None, true).await;
    announce(&state, &search, &news).await;
    let posted = posts.lock().unwrap().clone();
    assert_eq!(posted.len(), 1);
    assert!(posted[0].contains("3 new companies match Dentists in Leeds."), "{}", posted[0]);
    let link = format!("https://splee.test/app/p/{project}/companies?tab=database&search={}&since=2026-11-21T09:00:00Z", search.id);
    assert!(posted[0].contains(&link), "{}", posted[0]);
}

/// The app's routes on a fake server.
struct Api {
    base: String,
    client: reqwest::Client,
}

impl Api {
    async fn start(state: AppState) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", crate::routes::router(&state)).with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self { base, client: reqwest::Client::new() }
    }

    /// As the member `who`, or with the API key `splee_{who}` when `who` starts with "key-".
    async fn send(&self, who: &str, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.client.request(method, format!("{}{path}", self.base));
        req = match who.strip_prefix("key-") {
            Some(key) => req.bearer_auth(format!("splee_{key}")),
            None => req.header("cookie", format!("splee_session=session-{who}")),
        };
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or(Value::Null))
    }
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn searches_are_saved_and_listed_per_project_and_another_org_cant_touch_them(db: PgPool) {
    let (acme, beta) = (org(&db, "Acme").await, org(&db, "Beta").await);
    let (mine, other) = (project(&db, acme).await, project(&db, acme).await);
    let theirs = project(&db, beta).await;
    let campaign = segment(&db, mine).await;
    let foreign_campaign = segment(&db, theirs).await;
    let ana = member(&db, acme, "ana").await;
    member(&db, beta, "eve").await;
    for (org, key) in [(acme, "acme"), (beta, "beta")] {
        sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'k', 'splee_', $2)")
            .bind(org)
            .bind(sha256_hex(&format!("splee_{key}")))
            .execute(&db)
            .await
            .unwrap();
    }
    company(&db, "smile.co.uk", "Leeds", 0.9).await;
    let api = Api::start(state(&db)).await;
    let searches = format!("/projects/{mine}/directory/searches");

    let body = json!({ "name": "  Dentists   in Leeds ", "category": "dental clinic", "city": "Leeds", "segment_id": campaign, "chat_alerts": true });
    let (status, saved) = api.send("ana", Method::POST, &searches, Some(body.clone())).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!((saved["name"].as_str(), saved["user_id"].as_str()), (Some("Dentists in Leeds"), Some(ana.to_string().as_str())));
    assert_eq!((saved["campaign_name"].as_str(), saved["new_matches"].as_i64(), saved["too_broad"].as_bool()), (Some("Dentists"), Some(0), Some(false)));
    assert!(saved["baseline_at"].is_string(), "saving records the baseline");
    let id = saved["id"].as_str().unwrap().to_string();
    assert_eq!(found(&db, id.parse().unwrap()).await, [("smile.co.uk".to_string(), false)]);

    for (bad, why) in [
        (json!({ "name": "Everything" }), "no filters"),
        (json!({ "name": "Nowhere", "country": "GBR" }), "a bad country"),
        (json!({ "name": "", "city": "Leeds" }), "no name"),
        (json!({ "name": "Theirs", "city": "Leeds", "segment_id": foreign_campaign }), "another project's campaign"),
    ] {
        assert_eq!(api.send("ana", Method::POST, &searches, Some(bad)).await.0, 400, "{why}");
    }
    let (_, listed) = api.send("ana", Method::GET, &searches, None).await;
    assert_eq!(listed.as_array().map(|l| l.len()), Some(1));
    let (_, elsewhere) = api.send("ana", Method::GET, &format!("/projects/{other}/directory/searches"), None).await;
    assert_eq!(elsewhere, json!([]), "searches belong to their project");

    // Another organization, by session or API key, gets what an id that doesn't exist gets.
    let one = format!("/directory/searches/{id}");
    for (who, method, path, body) in [
        ("eve", Method::GET, searches.clone(), None),
        ("eve", Method::POST, searches.clone(), Some(body.clone())),
        ("eve", Method::PATCH, one.clone(), Some(json!({ "name": "Mine now" }))),
        ("eve", Method::GET, format!("{one}/new"), None),
        ("eve", Method::DELETE, one.clone(), None),
        ("key-beta", Method::GET, format!("/v1/projects/{mine}/saved-searches"), None),
        ("key-beta", Method::GET, format!("/v1/saved-searches/{id}/new"), None),
        ("key-beta", Method::DELETE, format!("/v1/saved-searches/{id}"), None),
    ] {
        assert_eq!(api.send(who, method.clone(), &path, body).await.0, 404, "{who} {method} {path}");
    }
    let name: String = sqlx::query_scalar("select name from saved_searches where id = $1::uuid").bind(&id).fetch_one(&db).await.unwrap();
    assert_eq!(name, "Dentists in Leeds");

    // A new match shows in the list until its owner opens the search.
    company(&db, "beam.co.uk", "Leeds", 0.5).await;
    let search = reload(&db, id.parse().unwrap()).await;
    let news = check(&db, &search).await.unwrap().unwrap();
    let (_, listed) = api.send("ana", Method::GET, &searches, None).await;
    assert_eq!(listed[0]["new_matches"], 1);
    let (_, by_key) = api.send("key-acme", Method::GET, &format!("/v1/saved-searches/{id}/new"), None).await;
    assert_eq!((by_key["total"].as_i64(), by_key["items"][0]["domain"].as_str()), (Some(1), Some("beam.co.uk")), "{by_key}");
    let (_, opened) = api.send("ana", Method::GET, &format!("{one}/new"), None).await;
    assert_eq!(opened["since"], json!(news.at), "{opened}");
    assert_eq!(opened["items"][0]["found_at"], json!(news.at));
    let (_, listed) = api.send("ana", Method::GET, &searches, None).await;
    assert_eq!(listed[0]["new_matches"], 0, "opened");
    let (_, again) = api.send("ana", Method::GET, &format!("{one}/new?since={}", news.at.format("%Y-%m-%dT%H:%M:%SZ")), None).await;
    assert_eq!(again["total"], 1, "the alert's link still shows them");

    let (status, changed) = api.send("ana", Method::PATCH, &one, Some(json!({ "name": "Leeds dentists", "segment_id": null }))).await;
    assert_eq!((status, changed["name"].as_str(), changed["add_to_segment_id"].clone()), (200, Some("Leeds dentists"), Value::Null));
    assert_eq!(changed["chat_alerts"], true, "left as it was");

    // A key has no member behind it, so its search has no owner.
    let (status, by_key) = api.send("key-acme", Method::POST, &format!("/v1/projects/{mine}/saved-searches"), Some(json!({ "name": "Smiles", "q": "smile" }))).await;
    assert_eq!((status, by_key["user_id"].clone()), (200, Value::Null), "{by_key}");
    let (_, listed) = api.send("key-acme", Method::GET, &format!("/v1/projects/{mine}/saved-searches"), None).await;
    assert_eq!(listed.as_array().map(|l| l.len()), Some(2));
    assert_eq!(api.send("key-acme", Method::DELETE, &format!("/v1/saved-searches/{}", by_key["id"].as_str().unwrap()), None).await.0, 200);
    assert_eq!(api.send("ana", Method::DELETE, &one, None).await.0, 200);
    let (_, listed) = api.send("ana", Method::GET, &searches, None).await;
    assert_eq!(listed, json!([]));
}
