//! The organization's Google Maps key and monthly cap, searching local businesses for a project, and adding them to one of
//! its campaigns. See local_businesses.rs.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use super::imports::check_segment;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::local_businesses::{self, AddOutcome, Results, SearchQuery, Usage};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/local-businesses/key", get(usage).put(connect).delete(disconnect))
        .route("/local-businesses/cap", put(set_cap))
        .route("/projects/{id}/local-businesses", get(search))
        .route("/projects/{id}/local-businesses/add", post(add))
}

async fn usage(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Usage>> {
    Ok(Json(local_businesses::usage(&state.db, auth.org_id).await?))
}

#[derive(Deserialize)]
struct KeyBody {
    key: String,
}

/// The key is shared by the organization's projects and spends its Google account's money, so only admins set it.
async fn connect(State(state): State<AppState>, auth: Auth, Json(body): Json<KeyBody>) -> AppResult<Json<Usage>> {
    auth.require_admin()?;
    Ok(Json(local_businesses::connect(&state, auth.org_id, &body.key).await?))
}

/// Searches kept from the last 30 days stay, as they were paid for.
async fn disconnect(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Usage>> {
    auth.require_admin()?;
    sqlx::query("delete from places_keys where org_id = $1").bind(auth.org_id).execute(&state.db).await?;
    Ok(Json(local_businesses::usage(&state.db, auth.org_id).await?))
}

#[derive(Deserialize)]
struct CapBody {
    cap: i32,
}

async fn set_cap(State(state): State<AppState>, auth: Auth, Json(body): Json<CapBody>) -> AppResult<Json<Usage>> {
    auth.require_admin()?;
    if !(0..=1_000_000).contains(&body.cap) {
        return Err(AppError::BadRequest("Set a cap from 0 to 1,000,000 calls a month".into()));
    }
    let saved = sqlx::query("update places_keys set monthly_cap = $2 where org_id = $1").bind(auth.org_id).bind(body.cap).execute(&state.db).await?;
    if saved.rows_affected() == 0 {
        return Err(AppError::BadRequest("Connect a Google Maps key first".into()));
    }
    Ok(Json(local_businesses::usage(&state.db, auth.org_id).await?))
}

async fn search(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(mut q): Query<SearchQuery>) -> AppResult<Json<Results>> {
    auth.project(&state.db, id).await?;
    q.project_id = Some(id);
    Ok(Json(local_businesses::search(&state, auth.org_id, &q).await?))
}

#[derive(Deserialize)]
struct AddBody {
    segment_id: Option<Uuid>,
    #[serde(default)]
    place_ids: Vec<String>,
}

async fn add(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<AddBody>) -> AppResult<Json<AddOutcome>> {
    auth.project(&state.db, id).await?;
    let segment_id = body.segment_id.ok_or_else(|| AppError::BadRequest("Pick a campaign".into()))?;
    check_segment(&state, id, Some(segment_id)).await?;
    Ok(Json(local_businesses::add(&state, auth.org_id, id, segment_id, &body.place_ids).await?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use reqwest::Method;
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use uuid::Uuid;

    use crate::config::Config;
    use crate::crypto::sha256_hex;
    use crate::places::fake::{self as places, place};
    use crate::site_contacts::fake as sites;
    use crate::state::AppState;

    struct Seeded {
        base: String,
        http: reqwest::Client,
        google: places::Fake,
        sites: sites::Fake,
        db: PgPool,
        project: Uuid,
        segment: Uuid,
        beta_project: Uuid,
        beta_segment: Uuid,
    }

    impl Seeded {
        /// Acme and Beta, each with an owner signed in as session "acme" or "beta", a project and a Dentists campaign. Acme
        /// has a member signed in as "jo".
        async fn start(db: PgPool) -> Self {
            let (places_url, google) = places::start().await;
            let (sites_url, sites) = sites::start().await;
            let base_state = crate::autopilot::tests::state(db.clone());
            let cfg = Config { app_secret: "test-secret".into(), places_api_url: places_url, local_sites_url: sites_url, ..(*base_state.cfg).clone() };
            let state = AppState { cfg: Arc::new(cfg), ..base_state };
            let mut ids = vec![];
            for name in ["acme", "beta", "jo"] {
                let org: Uuid = if name == "jo" {
                    sqlx::query_scalar("select id from organizations where name = 'acme'").fetch_one(&db).await.unwrap()
                } else {
                    sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap()
                };
                let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ($1, 'x') returning id")
                    .bind(format!("{name}@{name}.example"))
                    .fetch_one(&db)
                    .await
                    .unwrap();
                let role = if name == "jo" { "member" } else { "owner" };
                sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, $3)").bind(org).bind(user).bind(role).execute(&db).await.unwrap();
                sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                    .bind(sha256_hex(name))
                    .bind(user)
                    .bind(org)
                    .execute(&db)
                    .await
                    .unwrap();
                if name == "jo" {
                    continue;
                }
                let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(&db).await.unwrap();
                let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Dentists') returning id")
                    .bind(project)
                    .fetch_one(&db)
                    .await
                    .unwrap();
                ids.push((project, segment));
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", super::router()).with_state(state);
            tokio::spawn(async move { axum::serve(listener, app).await });
            Self {
                base,
                http: reqwest::Client::new(),
                google,
                sites,
                db,
                project: ids[0].0,
                segment: ids[0].1,
                beta_project: ids[1].0,
                beta_segment: ids[1].1,
            }
        }

        async fn call(&self, session: &str, method: Method, path: &str, body: Value) -> (u16, Value) {
            let mut req = self.http.request(method.clone(), format!("{}{path}", self.base)).header("cookie", format!("splee_session={session}"));
            if method != Method::GET {
                req = req.json(&body);
            }
            let res = req.send().await.unwrap();
            let status = res.status().as_u16();
            (status, res.json().await.unwrap_or(Value::Null))
        }

        async fn search(&self, session: &str, project: Uuid, query: &str) -> (u16, Value) {
            self.call(session, Method::GET, &format!("/projects/{project}/local-businesses?{query}"), Value::Null).await
        }

        async fn connect(&self) {
            let (status, body) = self.call("acme", Method::PUT, "/local-businesses/key", json!({ "key": "good" })).await;
            assert_eq!(status, 200, "{body}");
        }

        fn dentists(&self, count: usize) {
            let mut places = self.google.places.lock().unwrap();
            for i in 0..count {
                places.push(place(&format!("p{i}"), &format!("Dental {i}"), &format!("https://www.dental{i}.example/"), 3.0 + (i % 3) as f64, i as i64));
            }
        }
    }

    fn names(body: &Value) -> Vec<&str> {
        body["items"].as_array().unwrap().iter().map(|b| b["name"].as_str().unwrap()).collect()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_key_is_checked_on_save_and_only_admins_set_it(db: PgPool) {
        let s = Seeded::start(db).await;
        let (status, body) = s.search("acme", s.project, "what=dentists&where=Austin").await;
        assert_eq!((status, body["error"].as_str()), (400, Some("Connect a Google Maps key under Integrations first")));

        let (status, body) = s.call("acme", Method::PUT, "/local-businesses/key", json!({ "key": "bad" })).await;
        assert_eq!(status, 400);
        assert!(body["error"].as_str().unwrap().starts_with("Google didn't accept this API key"), "{body}");
        let (_, usage) = s.call("acme", Method::GET, "/local-businesses/key", Value::Null).await;
        assert_eq!(usage, json!({ "connected": false, "calls": 0, "cap": 2000, "price_per_1000_cents": 3500, "cost_cents": 0 }));

        assert_eq!(s.call("jo", Method::PUT, "/local-businesses/key", json!({ "key": "good" })).await.0, 403);
        s.connect().await;
        let enc: String = sqlx::query_scalar("select key_enc from places_keys").fetch_one(&s.db).await.unwrap();
        assert_ne!(enc, "good", "the key is stored encrypted");
        let (_, usage) = s.call("acme", Method::GET, "/local-businesses/key", Value::Null).await;
        assert_eq!(usage["connected"], true);
        assert_eq!(usage["calls"], 0, "the key check is free and isn't counted");
        assert_eq!(s.call("beta", Method::GET, "/local-businesses/key", Value::Null).await.1["connected"], false);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn searches_page_to_sixty_and_a_repeat_comes_from_the_cache(db: PgPool) {
        let s = Seeded::start(db).await;
        s.connect().await;
        s.dentists(70);
        let (status, body) = s.search("acme", s.project, "what=dentists&where=Austin,%20TX").await;
        assert_eq!(status, 200, "{body}");
        assert_eq!((body["found"].as_u64(), body["calls"].as_i64(), body["usage"]["calls"].as_i64()), (Some(60), Some(3), Some(3)));
        assert_eq!(body["usage"]["cost_cents"], 11, "3 calls at $35 per 1,000, rounded up");
        let first = &body["items"][0];
        assert_eq!((first["name"].as_str(), first["domain"].as_str(), first["phone"].as_str()), (Some("Dental 0"), Some("dental0.example"), Some("+15125550100")));
        let tokens: Vec<Option<String>> = s.google.calls.lock().unwrap().iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(tokens, [None, Some("page-20".into()), Some("page-40".into())]);

        // The same search typed differently costs nothing.
        let (_, again) = s.search("acme", s.project, "what=Dentists&where=austin,%20%20tx").await;
        assert_eq!((again["found"].as_u64(), again["calls"].as_i64()), (Some(60), Some(0)));
        assert_eq!(s.google.calls(), 3);

        // Beta has no key, and its own search would never see Acme's cache.
        let (status, _) = s.search("beta", s.beta_project, "what=dentists&where=Austin,%20TX").await;
        assert_eq!(status, 400);
        assert_eq!(s.search("beta", s.project, "what=dentists&where=Austin").await.0, 404);
        // Kept for 30 days, then searched again.
        sqlx::query("update places_searches set searched_at = now() - interval '31 days'").execute(&s.db).await.unwrap();
        let (_, stale) = s.search("acme", s.project, "what=dentists&where=Austin,%20TX").await;
        assert_eq!(stale["calls"], 3);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_search_over_several_places_lists_each_business_once_and_filters_apply(db: PgPool) {
        let s = Seeded::start(db).await;
        s.connect().await;
        s.dentists(5);
        s.google.places.lock().unwrap()[4]["websiteUri"].take();
        let (_, body) = s.search("acme", s.project, "what=dentists&where=78701%3B%2078702%0A78701").await;
        assert_eq!((body["found"].as_u64(), body["calls"].as_i64()), (Some(5), Some(2)), "78701 twice is searched once, and both find the same five");
        let queries: Vec<String> = s.google.calls.lock().unwrap().iter().map(|(q, _)| q.clone()).collect();
        assert_eq!(queries, ["dentists in 78701", "dentists in 78702"]);

        // Ratings run 3, 4, 5, 3, 4 and reviews 0 to 4. Dental 4 has no website.
        let (_, body) = s.search("acme", s.project, "what=dentists&where=78701&min_rating=4").await;
        assert_eq!(names(&body), ["Dental 1", "Dental 2", "Dental 4"]);
        let (_, body) = s.search("acme", s.project, "what=dentists&where=78701&min_rating=4&min_reviews=2&with_website=true").await;
        assert_eq!(names(&body), ["Dental 2"]);
        assert_eq!(body["found"], 5);

        let places: Vec<String> = (0..21).map(|i| format!("{}", 78700 + i)).collect();
        let (status, body) = s.search("acme", s.project, &format!("what=dentists&where={}", places.join("%3B"))).await;
        assert_eq!((status, body["error"].as_str()), (400, Some("Split a search over at most 20 places")));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_monthly_cap_stops_searches(db: PgPool) {
        let s = Seeded::start(db).await;
        s.connect().await;
        s.dentists(70);
        assert_eq!(s.call("acme", Method::PUT, "/local-businesses/cap", json!({ "cap": 4 })).await.0, 200);
        let (_, body) = s.search("acme", s.project, "what=dentists&where=78701").await;
        assert_eq!(body["calls"], 3);
        // The second place gets one page before the cap, so it's shown but not kept.
        let (status, body) = s.search("acme", s.project, "what=dentists&where=78701%3B78702").await;
        assert_eq!((status, body["calls"].as_i64(), body["usage"]["calls"].as_i64()), (200, Some(1), Some(4)));
        assert!(body["note"].as_str().unwrap().starts_with("This month's Google Maps calls reached your cap of 4."), "{body}");
        let (status, body) = s.search("acme", s.project, "what=gyms&where=78701").await;
        assert_eq!(status, 429);
        assert!(body["error"].as_str().unwrap().contains("Raise the cap under Integrations"), "{body}");
        assert_eq!(s.google.calls(), 4);
        assert_eq!(s.call("jo", Method::PUT, "/local-businesses/cap", json!({ "cap": 100 })).await.0, 403);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn adding_businesses_makes_companies_and_leads_with_addresses_from_their_websites(db: PgPool) {
        let s = Seeded::start(db).await;
        s.connect().await;
        {
            let mut places = s.google.places.lock().unwrap();
            places.push(place("smile", "Smile Dental", "https://www.smile.example/", 4.9, 210));
            places.push(place("bright", "Bright Dental", "https://bright.example", 4.5, 80));
            places.push(place("quiet", "Quiet Dental", "https://quiet.example", 4.1, 12));
            places.push(place("walkin", "Walk-in Dental", "", 4.0, 40));
            places.push(place("known", "Known Dental", "https://known.example", 4.0, 3));
            places.push(place("blocked", "Blocked Dental", "https://blocked.example", 4.0, 3));
            let mut no_phone = place("ghost", "Ghost Dental", "", 3.0, 1);
            no_phone["internationalPhoneNumber"].take();
            places.push(no_phone);
        }
        s.sites.page("smile.example", "/", r#"<a href="/contact">Contact</a> info@smile.example"#);
        s.sites.page("smile.example", "/contact", "Dr. Maria Lopez: maria.lopez@smile.example");
        s.sites.page("bright.example", "/", "Book at hello@bright.example or call us.");
        s.sites.page("quiet.example", "/robots.txt", "User-agent: *\nDisallow: /");
        s.sites.page("quiet.example", "/", "info@quiet.example");
        sqlx::query("insert into companies (project_id, domain) values ($1, 'known.example')").bind(s.project).execute(&s.db).await.unwrap();
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(s.project).fetch_one(&s.db).await.unwrap();
        sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, 'blocked.example', 'domain', 'manual')").bind(org).execute(&s.db).await.unwrap();

        let (_, found) = s.search("acme", s.project, "what=dental&where=Austin").await;
        let known: Vec<(&str, Option<&str>)> = found["items"].as_array().unwrap().iter().map(|b| (b["name"].as_str().unwrap(), b["known"].as_str())).collect();
        assert!(known.contains(&("Known Dental", Some("company"))) && known.contains(&("Smile Dental", None)), "{known:?}");

        let ids = ["smile", "bright", "quiet", "walkin", "known", "blocked", "ghost", "never-searched"];
        let path = format!("/projects/{}/local-businesses/add", s.project);
        let (status, body) = s.call("acme", Method::POST, &path, json!({ "segment_id": s.segment, "place_ids": ids })).await;
        assert_eq!(status, 200, "{body}");
        let added: Vec<(&str, Option<&str>, &str, &str)> = body["added"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| (a["name"].as_str().unwrap(), a["email"].as_str(), a["email_kind"].as_str().unwrap(), a["note"].as_str().unwrap()))
            .collect();
        assert_eq!(added, [
            ("Smile Dental", Some("maria.lopez@smile.example"), "person", "Found on its website"),
            ("Bright Dental", Some("hello@bright.example"), "role", "Only a shared address on its website"),
            ("Quiet Dental", None, "", "quiet.example asks robots not to read its pages"),
            ("Walk-in Dental", None, "", "No website. Call or text them instead"),
        ]);
        let skipped: Vec<(&str, &str)> = body["skipped"].as_array().unwrap().iter().map(|k| (k["name"].as_str().unwrap(), k["reason"].as_str().unwrap())).collect();
        assert_eq!(skipped, [
            ("never-searched", "Splee keeps a search for 30 days. Search again to add it."),
            ("Known Dental", "Already in this project"),
            ("Blocked Dental", "On the do-not-contact list"),
            ("Ghost Dental", "No website or phone number to reach them at"),
        ]);
        assert!(!s.sites.fetched("quiet.example", "/"));

        let company: (Uuid, String, String, String, String) =
            sqlx::query_as("select id, status, phone, address, place_id from companies where domain = 'smile.example'").fetch_one(&s.db).await.unwrap();
        let lead: (String, String, String, String, String, String, bool) = sqlx::query_as(
            "select full_name, first_name, email::text, email_status, source, phone, segment_id = $1 and company_id = $2
             from leads where company_name = 'Smile Dental'",
        )
        .bind(s.segment)
        .bind(company.0)
        .fetch_one(&s.db)
        .await
        .unwrap();
        assert_eq!(lead, (
            "Maria Lopez".into(), "Maria".into(), "maria.lopez@smile.example".into(), "found".into(), "google_maps".into(), "+15125550100".into(), true,
        ));
        assert_eq!(&company.1[..], "people_found", "Find people leaves it alone");
        assert_eq!((&company.2[..], &company.3[..], &company.4[..]), ("+15125550100", "100 Congress Ave, Austin, TX 78701, USA", "smile"));
        let walk_in: (Option<Uuid>, Option<String>, String) =
            sqlx::query_as("select company_id, email::text, phone from leads where company_name = 'Walk-in Dental'").fetch_one(&s.db).await.unwrap();
        assert_eq!(walk_in, (None, None, "+15125550100".into()));

        // Now they're all in the project, and Beta can't add Acme's businesses or use Acme's campaign.
        let (_, again) = s.search("acme", s.project, "what=dental&where=Austin").await;
        let smile = again["items"].as_array().unwrap().iter().find(|b| b["place_id"] == "smile").unwrap();
        assert_eq!(smile["known"], "company");
        let walk_in = again["items"].as_array().unwrap().iter().find(|b| b["place_id"] == "walkin").unwrap();
        assert_eq!(walk_in["known"], "lead");
        let (_, beta) = s
            .call("beta", Method::POST, &format!("/projects/{}/local-businesses/add", s.beta_project), json!({ "segment_id": s.beta_segment, "place_ids": ["smile"] }))
            .await;
        assert_eq!(beta["skipped"][0]["reason"], "Splee keeps a search for 30 days. Search again to add it.");
        assert_eq!(s.call("beta", Method::POST, &path, json!({ "segment_id": s.segment, "place_ids": ["smile"] })).await.0, 404);
        let wrong_campaign = s.call("acme", Method::POST, &path, json!({ "segment_id": s.beta_segment, "place_ids": ["smile"] })).await.0;
        assert_eq!(wrong_campaign, 400);
    }
}
