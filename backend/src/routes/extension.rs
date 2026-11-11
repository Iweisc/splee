//! The browser extension's API. It adds the person or company open in a tab as a lead, with an API key like the rest of
//! /v1. These are the only routes that answer cross-origin requests, and only from extension pages.

use axum::extract::{Query, State};
use axum::http::{HeaderValue, Method, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use tower_http::cors::{AllowOrigin, CorsLayer};
use uuid::Uuid;

use super::api_v1::check_project;
use super::util_input;
use super::util_lead_import::{self, NewLead, profile_key};
use crate::auth::ApiKeyAuth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::waterfall;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/extension/projects", get(projects))
        .route("/v1/extension/check", get(check))
        .route("/v1/extension/leads", post(add_lead))
        .route("/v1/extension/companies", post(add_company))
        .layer(cors())
}

/// Extension ids differ per browser and per install, so any extension origin gets in. The API key guards these routes, and
/// no cookie is sent or accepted.
fn cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin: &HeaderValue, _| {
            origin.to_str().is_ok_and(|o| o.starts_with("chrome-extension://") || o.starts_with("moz-extension://"))
        }))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

#[derive(Serialize)]
struct ProjectChoice {
    id: Uuid,
    name: String,
    campaigns: Vec<CampaignChoice>,
}

#[derive(Serialize, FromRow)]
struct CampaignChoice {
    id: Uuid,
    #[serde(skip)]
    project_id: Uuid,
    name: String,
    status: String,
}

/// The popup's two pickers in one request: the organization's projects, newest first, each with its campaigns.
async fn projects(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<ProjectChoice>>> {
    let projects: Vec<(Uuid, String)> = sqlx::query_as("select id, name from projects where org_id = $1 order by created_at desc")
        .bind(auth.org_id)
        .fetch_all(&state.db)
        .await?;
    let ids: Vec<Uuid> = projects.iter().map(|(id, _)| *id).collect();
    let mut campaigns: Vec<CampaignChoice> =
        sqlx::query_as("select id, project_id, name, status from segments where project_id = any($1) order by position, created_at")
            .bind(&ids)
            .fetch_all(&state.db)
            .await?;
    let choices = projects.into_iter().map(|(id, name)| {
        let (mine, rest) = std::mem::take(&mut campaigns).into_iter().partition(|c| c.project_id == id);
        campaigns = rest;
        ProjectChoice { id, name, campaigns: mine }
    });
    Ok(Json(choices.collect()))
}

#[derive(Deserialize)]
struct CheckQuery {
    project_id: Uuid,
    #[serde(default)]
    linkedin_url: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    domain: String,
}

#[derive(Serialize)]
struct Known {
    /// The same person as a lead in any of the organization's projects, newest first.
    leads: Vec<KnownLead>,
    /// The company, when it's already in the picked project.
    company: Option<KnownCompany>,
    /// The do-not-contact entry that covers the email, its domain or the company's domain.
    suppressed: Option<String>,
}

#[derive(Serialize, FromRow)]
struct KnownLead {
    id: Uuid,
    project_id: Uuid,
    project_name: String,
    full_name: String,
    email: Option<String>,
    status: String,
    campaign: Option<String>,
    #[serde(skip)]
    linkedin_url: String,
}

#[derive(Serialize, FromRow)]
struct KnownCompany {
    id: Uuid,
    name: String,
    domain: String,
}

/// What the popup asks before adding: is this person already a lead, or this company in the project, or on the do-not-contact list.
async fn check(State(state): State<AppState>, auth: ApiKeyAuth, Query(q): Query<CheckQuery>) -> AppResult<Json<Known>> {
    check_project(&state.db, q.project_id, auth.org_id).await?;
    Ok(Json(look_up(&state.db, auth.org_id, q.project_id, &q.email, &q.linkedin_url, &q.domain).await?))
}

const KNOWN_LEADS: usize = 10;

async fn look_up(db: &PgPool, org_id: Uuid, project_id: Uuid, email: &str, linkedin_url: &str, domain: &str) -> AppResult<Known> {
    let email = util_input::email(email);
    let profile = profile_key(linkedin_url);
    let domain = util_input::domain(domain);
    let mut leads = Vec::new();
    if email.is_some() || profile.is_some() {
        // The position() narrows it down to links that start with the profile's id, and profile_key below tells sam-lee from sam-leeds.
        let candidates: Vec<KnownLead> = sqlx::query_as(
            "select l.id, l.project_id, p.name as project_name, l.full_name, l.email::text as email, l.status, s.name as campaign, l.linkedin_url
             from leads l join projects p on p.id = l.project_id left join segments s on s.id = l.segment_id
             where p.org_id = $1
               and (l.email = $2::citext or ($3::text is not null and position('linkedin.com/in/' || $3 in lower(l.linkedin_url)) > 0))
             order by l.created_at desc limit 100",
        )
        .bind(org_id)
        .bind(&email)
        .bind(&profile)
        .fetch_all(db)
        .await?;
        let same = |l: &KnownLead| {
            (email.is_some() && l.email.as_deref().map(str::to_lowercase) == email) || (profile.is_some() && profile_key(&l.linkedin_url) == profile)
        };
        leads = candidates.into_iter().filter(same).take(KNOWN_LEADS).collect();
    }
    let company = match &domain {
        Some(d) => {
            sqlx::query_as("select id, name, domain::text from companies where project_id = $1 and domain = $2::citext")
                .bind(project_id)
                .bind(d)
                .fetch_optional(db)
                .await?
        }
        None => None,
    };
    let values: Vec<String> = [email.clone(), email.as_deref().and_then(|e| e.rsplit_once('@')).map(|(_, d)| d.to_string()), domain]
        .into_iter()
        .flatten()
        .collect();
    let suppressed = sqlx::query_scalar("select value::text from suppressions where org_id = $1 and lower(value::text) = any($2) limit 1")
        .bind(org_id)
        .bind(&values)
        .fetch_optional(db)
        .await?;
    Ok(Known { leads, company, suppressed })
}

#[derive(Deserialize)]
struct LeadBody {
    project_id: Uuid,
    segment_id: Option<Uuid>,
    #[serde(default)]
    email: String,
    #[serde(default)]
    full_name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    company_name: String,
    #[serde(default)]
    company_domain: String,
    #[serde(default)]
    linkedin_url: String,
    /// Queue the email waterfall for a lead added without an address.
    #[serde(default)]
    find_email: bool,
}

/// Adds one lead through the import path, so the same dedupe, suppression checks and lock apply. A LinkedIn profile makes it
/// a LinkedIn lead, which may come without an email.
async fn add_lead(State(state): State<AppState>, auth: ApiKeyAuth, Json(body): Json<LeadBody>) -> AppResult<Json<Value>> {
    check_project(&state.db, body.project_id, auth.org_id).await?;
    let linkedin = !body.linkedin_url.trim().is_empty();
    if linkedin && profile_key(&body.linkedin_url).is_none() {
        return Err(AppError::BadRequest("That isn't a link to a LinkedIn profile".into()));
    }
    if !linkedin && body.email.trim().is_empty() {
        return Err(AppError::BadRequest("Add an email or a LinkedIn profile".into()));
    }
    refuse_known(&state.db, auth.org_id, body.project_id, &body.email, &body.linkedin_url, &body.company_domain, false).await?;

    let lead = NewLead {
        email: body.email.clone(),
        full_name: body.full_name,
        title: body.title,
        company_name: body.company_name,
        company_domain: body.company_domain,
        linkedin_url: body.linkedin_url,
        segment_id: body.segment_id,
        ..Default::default()
    };
    let has_domain = util_input::domain(&lead.company_domain).is_some();
    let source = if linkedin { "linkedin" } else { "api" };
    let outcome = util_lead_import::insert(&state.db, auth.org_id, body.project_id, source, vec![(1, lead)]).await?;
    let Some(&lead_id) = outcome.ids.first() else {
        // Lost a race with another import, or the email didn't parse.
        let error = outcome.errors.into_iter().next().map_or_else(|| "Already a lead in this project".to_string(), |e| e.error);
        return Err(AppError::BadRequest(error));
    };

    let mut email_search = None;
    let mut note = None;
    if body.find_email && body.email.trim().is_empty() {
        let connected: bool =
            sqlx::query_scalar("select exists (select 1 from project_settings where project_id = $1 and (finder <> '' or verifier <> ''))")
                .bind(body.project_id)
                .fetch_one(&state.db)
                .await?;
        if !connected {
            note = Some("Connect an email finder or verifier under Integrations to find the email.");
        } else if !has_domain {
            note = Some("Add the company's website to find the email.");
        } else {
            sqlx::query("update leads set email_search = 'queued', email_search_note = '' where id = $1 and email is null")
                .bind(lead_id)
                .execute(&state.db)
                .await?;
            waterfall::wake();
            email_search = Some("queued");
        }
    }
    Ok(Json(json!({ "lead_id": lead_id, "email_search": email_search, "note": note })))
}

/// The popup checks first, but a second tab or a teammate may have added them since.
async fn refuse_known(db: &PgPool, org_id: Uuid, project_id: Uuid, email: &str, linkedin_url: &str, domain: &str, company: bool) -> AppResult<()> {
    let known = look_up(db, org_id, project_id, email, linkedin_url, domain).await?;
    if let Some(value) = known.suppressed {
        return Err(AppError::Conflict(format!("{value} is on the do-not-contact list")));
    }
    if company && known.company.is_some() {
        return Err(AppError::Conflict("This company is already in the project".into()));
    }
    if !company && known.leads.iter().any(|l| l.project_id == project_id) {
        return Err(AppError::Conflict("Already a lead in this project".into()));
    }
    Ok(())
}

#[derive(Deserialize)]
struct CompanyBody {
    project_id: Uuid,
    segment_id: Option<Uuid>,
    domain: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    employees: String,
    /// The page it was added from.
    #[serde(default)]
    source_url: String,
}

/// For a company page or website with no address to add: the campaign's "Find people" then looks for people there.
async fn add_company(State(state): State<AppState>, auth: ApiKeyAuth, Json(body): Json<CompanyBody>) -> AppResult<Json<Value>> {
    check_project(&state.db, body.project_id, auth.org_id).await?;
    let domain = util_input::domain(&body.domain).ok_or_else(|| AppError::BadRequest("Add the company's website".into()))?;
    if let Some(segment) = body.segment_id {
        sqlx::query("select 1 from segments where id = $1 and project_id = $2")
            .bind(segment)
            .bind(body.project_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::BadRequest("Unknown campaign".into()))?;
    }
    refuse_known(&state.db, auth.org_id, body.project_id, "", "", &domain, true).await?;
    let id: Option<Uuid> = sqlx::query_scalar(
        "insert into companies (project_id, segment_id, domain, name, employees, source_url, fit_reason)
         values ($1, $2, $3, $4, $5, $6, 'Added with the browser extension')
         on conflict (project_id, domain) do nothing returning id",
    )
    .bind(body.project_id)
    .bind(body.segment_id)
    .bind(&domain)
    .bind(body.name.trim())
    .bind(body.employees.trim())
    .bind(body.source_url.trim())
    .fetch_optional(&state.db)
    .await?;
    let id = id.ok_or_else(|| AppError::Conflict("This company is already in the project".into()))?;
    Ok(Json(json!({ "company_id": id })))
}

#[cfg(test)]
mod tests {
    use reqwest::Method;
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use uuid::Uuid;

    use crate::crypto::sha256_hex;

    const EXTENSION: &str = "chrome-extension://abcdefghijklmnopabcdefghijklmnop";

    struct Server {
        base: String,
        client: reqwest::Client,
        db: PgPool,
        project: Uuid,
        other_project: Uuid,
    }

    type Reply = (u16, Value);

    impl Server {
        /// Acme has the key "splee_acme" and a project. Other is another organization with a project of its own.
        async fn start(db: PgPool) -> Self {
            let mut projects = vec![];
            for name in ["Acme", "Other"] {
                let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap();
                let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id")
                    .bind(org)
                    .bind(format!("{name} project"))
                    .fetch_one(&db)
                    .await
                    .unwrap();
                sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'Extension', 'splee_', $2)")
                    .bind(org)
                    .bind(sha256_hex(&format!("splee_{}", name.to_lowercase())))
                    .execute(&db)
                    .await
                    .unwrap();
                projects.push(project);
            }
            let state = crate::autopilot::tests::state(db.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let app = axum::Router::new().nest("/api", crate::routes::router()).with_state(state);
            tokio::spawn(async move { axum::serve(listener, app).await });
            Self { base, client: reqwest::Client::new(), db, project: projects[0], other_project: projects[1] }
        }

        async fn call(&self, method: Method, path: &str, key: Option<&str>, body: Option<Value>) -> Reply {
            let mut req = self.client.request(method, format!("{}{path}", self.base)).header("origin", EXTENSION);
            if let Some(key) = key {
                req = req.bearer_auth(key);
            }
            if let Some(body) = body {
                req = req.json(&body);
            }
            let res = req.send().await.unwrap();
            (res.status().as_u16(), res.json().await.unwrap_or(Value::Null))
        }

        async fn get(&self, path: &str) -> Reply {
            self.call(Method::GET, path, Some("splee_acme"), None).await
        }

        async fn post(&self, path: &str, body: Value) -> Reply {
            self.call(Method::POST, path, Some("splee_acme"), Some(body)).await
        }

        async fn leads(&self) -> i64 {
            sqlx::query_scalar("select count(*) from leads where project_id = $1").bind(self.project).fetch_one(&self.db).await.unwrap()
        }

        /// The CORS answer to a preflight from `origin` for `path`.
        async fn preflight(&self, path: &str, origin: &str) -> Option<String> {
            let res = self
                .client
                .request(Method::OPTIONS, format!("{}{path}", self.base))
                .header("origin", origin)
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", "authorization, content-type")
                .send()
                .await
                .unwrap();
            res.headers().get("access-control-allow-origin").map(|v| v.to_str().unwrap().to_string())
        }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn routes_need_a_key_and_keep_to_its_organization(db: PgPool) {
        let s = Server::start(db).await;
        sqlx::query("insert into segments (project_id, name) values ($1, 'Dental clinics')").bind(s.project).execute(&s.db).await.unwrap();

        assert_eq!(s.call(Method::GET, "/v1/extension/projects", None, None).await.0, 401);
        assert_eq!(s.call(Method::GET, "/v1/extension/projects", Some("splee_wrong"), None).await.0, 401);
        let (status, projects) = s.get("/v1/extension/projects").await;
        assert_eq!(status, 200);
        let campaign = &projects[0]["campaigns"][0]["id"];
        let expected = json!([{ "id": s.project, "name": "Acme project", "campaigns": [{ "id": campaign, "name": "Dental clinics", "status": "draft" }] }]);
        assert_eq!(projects, expected);

        // Another organization's project looks like one that doesn't exist.
        let other = s.other_project;
        assert_eq!(s.get(&format!("/v1/extension/check?project_id={other}&email=sam@fieldnote.test")).await.0, 404);
        let lead = json!({ "project_id": other, "email": "sam@fieldnote.test", "full_name": "Sam Lee" });
        assert_eq!(s.post("/v1/extension/leads", lead).await.0, 404);
        assert_eq!(s.post("/v1/extension/companies", json!({ "project_id": other, "domain": "fieldnote.test" })).await.0, 404);
        let theirs: i64 = sqlx::query_scalar("select (select count(*) from leads) + (select count(*) from companies)").fetch_one(&s.db).await.unwrap();
        assert_eq!(theirs, 0);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_person_already_a_lead_is_found_and_not_added_twice(db: PgPool) {
        let s = Server::start(db).await;
        let project = s.project;
        let sam = json!({ "project_id": project, "full_name": "Sam Lee", "title": "Head of Growth", "company_name": "Fieldnote",
                          "linkedin_url": "https://www.linkedin.com/in/sam-lee/" });
        let (status, added) = s.post("/v1/extension/leads", sam.clone()).await;
        assert_eq!(status, 200, "{added}");
        let lead: Uuid = added["lead_id"].as_str().unwrap().parse().unwrap();
        let source: String = sqlx::query_scalar("select source from leads where id = $1").bind(lead).fetch_one(&s.db).await.unwrap();
        assert_eq!(source, "linkedin");

        // The same profile, linked another way, is the same person. A longer id that starts the same is someone else.
        let (_, known) = s.get(&format!("/v1/extension/check?project_id={project}&linkedin_url=https://linkedin.com/in/Sam-Lee?trk=x")).await;
        assert_eq!(known["leads"].as_array().unwrap().len(), 1);
        assert_eq!(known["leads"][0]["project_name"], "Acme project");
        assert_eq!(known["suppressed"], Value::Null);
        let (_, known) = s.get(&format!("/v1/extension/check?project_id={project}&linkedin_url=https://www.linkedin.com/in/sam-leeds/")).await;
        assert_eq!(known["leads"], json!([]));

        let (status, refused) = s.post("/v1/extension/leads", sam).await;
        assert_eq!((status, refused["error"].as_str()), (409, Some("Already a lead in this project")));
        let ana = json!({ "project_id": project, "email": "ana@fieldnote.test", "full_name": "Ana" });
        assert_eq!(s.post("/v1/extension/leads", ana.clone()).await.0, 200);
        assert_eq!(s.post("/v1/extension/leads", json!({ "project_id": project, "email": "ANA@fieldnote.test" })).await.0, 409);
        assert_eq!(s.leads().await, 2);

        let (_, known) = s.get(&format!("/v1/extension/check?project_id={project}&email=Ana@Fieldnote.test")).await;
        assert_eq!(known["leads"][0]["email"], "ana@fieldnote.test");

        // A company already in the project.
        let company = json!({ "project_id": project, "domain": "https://www.fieldnote.test/about", "name": "Fieldnote", "employees": "11-50" });
        assert_eq!(s.post("/v1/extension/companies", company.clone()).await.0, 200);
        let (_, known) = s.get(&format!("/v1/extension/check?project_id={project}&domain=fieldnote.test")).await;
        assert_eq!(known["company"]["name"], "Fieldnote");
        assert_eq!(s.post("/v1/extension/companies", company).await.0, 409);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn suppressed_people_and_companies_are_refused(db: PgPool) {
        let s = Server::start(db).await;
        let project = s.project;
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(project).fetch_one(&s.db).await.unwrap();
        sqlx::query(
            "insert into suppressions (org_id, value, kind, reason)
             values ($1, 'sam@fieldnote.test', 'email', 'unsubscribe'), ($1, 'blocked.test', 'domain', 'manual')",
        )
            .bind(org)
            .execute(&s.db)
            .await
            .unwrap();

        let (_, known) = s.get(&format!("/v1/extension/check?project_id={project}&email=Sam@fieldnote.test")).await;
        assert_eq!(known["suppressed"], "sam@fieldnote.test");
        let (status, refused) = s.post("/v1/extension/leads", json!({ "project_id": project, "email": "sam@fieldnote.test", "full_name": "Sam" })).await;
        assert_eq!((status, refused["error"].as_str()), (409, Some("sam@fieldnote.test is on the do-not-contact list")));
        let at_blocked =
            json!({ "project_id": project, "linkedin_url": "https://www.linkedin.com/in/kim/", "full_name": "Kim", "company_domain": "blocked.test" });
        assert_eq!(s.post("/v1/extension/leads", at_blocked).await.0, 409);
        assert_eq!(s.post("/v1/extension/companies", json!({ "project_id": project, "domain": "www.blocked.test" })).await.0, 409);
        assert_eq!(s.leads().await, 0);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn find_email_queues_the_waterfall_without_an_agent_run(db: PgPool) {
        let s = Server::start(db).await;
        let project = s.project;
        let kim = |slug: &str, domain: &str| {
            json!({ "project_id": project, "full_name": "Kim Park", "company_domain": domain, "find_email": true,
                    "linkedin_url": format!("https://www.linkedin.com/in/{slug}/") })
        };
        let (_, added) = s.post("/v1/extension/leads", kim("kim-1", "fieldnote.test")).await;
        assert_eq!(added["email_search"], Value::Null);
        assert!(added["note"].as_str().unwrap().contains("Connect an email finder"));

        sqlx::query("insert into project_settings (project_id, finder) values ($1, 'hunter') on conflict (project_id) do update set finder = 'hunter'")
            .bind(project)
            .execute(&s.db)
            .await
            .unwrap();
        let (_, added) = s.post("/v1/extension/leads", kim("kim-2", "")).await;
        assert!(added["note"].as_str().unwrap().contains("website"));
        let (status, added) = s.post("/v1/extension/leads", kim("kim-3", "fieldnote.test")).await;
        assert_eq!((status, &added["email_search"]), (200, &json!("queued")), "{added}");
        let searches: Vec<String> =
            sqlx::query_scalar("select email_search from leads where project_id = $1 order by linkedin_url").bind(project).fetch_all(&s.db).await.unwrap();
        assert_eq!(searches, ["", "", "queued"]);
        let runs: i64 = sqlx::query_scalar("select count(*) from agent_runs").fetch_one(&s.db).await.unwrap();
        assert_eq!(runs, 0);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn only_extension_origins_get_cors_and_only_on_these_routes(db: PgPool) {
        let s = Server::start(db).await;
        let firefox = "moz-extension://2f1c0c5e-7d7a-4c38-9a3b-0d6a1d2f9e10";
        assert_eq!(s.preflight("/v1/extension/leads", EXTENSION).await.as_deref(), Some(EXTENSION));
        assert_eq!(s.preflight("/v1/extension/check", firefox).await.as_deref(), Some(firefox));
        assert_eq!(s.preflight("/v1/extension/leads", "https://evil.test").await, None);
        let project = s.project;
        assert_eq!(s.preflight(&format!("/v1/projects/{project}/leads"), EXTENSION).await, None);
        assert_eq!(s.preflight("/organization/api-keys", EXTENSION).await, None);

        let res = s.client.get(format!("{}/v1/extension/projects", s.base)).header("origin", EXTENSION).bearer_auth("splee_acme").send().await.unwrap();
        assert_eq!(res.headers().get("access-control-allow-origin").unwrap(), EXTENSION);
        let res = s.client.get(format!("{}/v1/projects", s.base)).header("origin", EXTENSION).bearer_auth("splee_acme").send().await.unwrap();
        assert_eq!(res.status(), 200);
        assert!(res.headers().get("access-control-allow-origin").is_none());
    }
}
