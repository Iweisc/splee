use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use super::auth::ClaimBody;
use super::{settings, util_input};
use crate::agent::{self, kind};
use crate::audit::{self, Target, Who};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::{AgentRun, Project, ProjectSettings};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", get(list).post(create))
        .route("/projects/claim", post(claim_signed_in))
        .route("/projects/{id}", get(detail).patch(update).delete(remove))
        .route("/projects/{id}/research", post(research))
        .route("/projects/{id}/runs", get(runs))
        .route("/projects/{id}/autopilot", get(autopilot))
        .route("/projects/{id}/rewrite-emails", post(rewrite_emails))
        .route("/projects/{id}/start", post(start))
        .route("/projects/{id}/pause", post(pause))
}

/// Moves an anonymous demo project into an org. Returns false when the token is wrong or it was already claimed.
pub async fn claim(conn: &mut PgConnection, project_id: Uuid, token: &str, org_id: Uuid, user_id: Uuid) -> AppResult<bool> {
    let claimed = sqlx::query(
        "update projects set org_id = $1, created_by = $2, claim_token = null
         where id = $3 and org_id is null and claim_token = $4",
    )
    .bind(org_id)
    .bind(user_id)
    .bind(project_id)
    .bind(token)
    .execute(&mut *conn)
    .await?
    .rows_affected()
        == 1;
    if claimed {
        sqlx::query("insert into project_settings (project_id) values ($1) on conflict do nothing")
            .bind(project_id)
            .execute(&mut *conn)
            .await?;
        // Demo runs were created without an org. Runs are org-scoped once the project has an owner.
        sqlx::query("update agent_runs set org_id = $1 where project_id = $2")
            .bind(org_id)
            .bind(project_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(claimed)
}

#[derive(Serialize, FromRow)]
struct ProjectSummary {
    #[serde(flatten)]
    #[sqlx(flatten)]
    project: Project,
    sent: i64,
    replies: i64,
    hot: i64,
    active_segments: i64,
    paused: bool,
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<ProjectSummary>>> {
    let projects = sqlx::query_as::<_, ProjectSummary>(
        "select p.*,
           (select count(*) from email_messages e
             where e.project_id = p.id and e.direction = 'outbound' and e.status = 'sent') as sent,
           (select count(*) from leads l where l.project_id = p.id
             and exists (select 1 from email_messages e where e.lead_id = l.id and e.direction = 'inbound'
                         and e.classification is distinct from 'bounce')) as replies,
           (select count(*) from leads l where l.project_id = p.id and l.status in ('hot', 'meeting_booked')) as hot,
           (select count(*) from segments s where s.project_id = p.id and s.status in ('working', 'scaling')) as active_segments,
           coalesce((select ps.paused from project_settings ps where ps.project_id = p.id), true) as paused
         from projects p where p.org_id = $1 order by p.created_at desc",
    )
    .bind(auth.org_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(projects))
}

#[derive(Deserialize)]
pub(super) struct CreateBody {
    domain: Option<String>,
    description: Option<String>,
}

async fn create(State(state): State<AppState>, auth: Auth, Json(body): Json<CreateBody>) -> AppResult<Json<Project>> {
    Ok(Json(insert(&state.db, auth.org_id, Some(auth.user_id), body).await?.0))
}

/// A project from the business's website or a description of it, and the explore run that researches it, whose id comes back
/// with the project. `created_by` is None for an API key.
pub(super) async fn insert(db: &PgPool, org_id: Uuid, created_by: Option<Uuid>, body: CreateBody) -> AppResult<(Project, Uuid)> {
    let domain = match body.domain.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        Some(raw) => Some(util_input::domain(raw).ok_or_else(|| AppError::BadRequest("Enter a valid website, like acme.com".into()))?),
        None => None,
    };
    let description = body.description.as_deref().map(str::trim).filter(|d| !d.is_empty());
    if domain.is_none() && description.is_none() {
        return Err(AppError::BadRequest("Enter your website or describe your business".into()));
    }

    let mut tx = db.begin().await?;
    let project = sqlx::query_as::<_, Project>(
        "insert into projects (org_id, domain, business_description, name, created_by)
         values ($1, $2, $3, coalesce($2, ''), $4) returning *",
    )
    .bind(org_id)
    .bind(&domain)
    .bind(description)
    .bind(created_by)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("insert into project_settings (project_id) values ($1)").bind(project.id).execute(&mut *tx).await?;
    tx.commit().await?;

    let run_id = agent::enqueue(db, kind::EXPLORE, Some(project.id), Some(org_id), json!({ "mode": "app" })).await?;
    Ok((project, run_id))
}

async fn claim_signed_in(State(state): State<AppState>, auth: Auth, Json(body): Json<ClaimBody>) -> AppResult<Json<Project>> {
    let mut tx = state.db.begin().await?;
    if !claim(&mut tx, body.project_id, &body.token, auth.org_id, auth.user_id).await? {
        return Err(AppError::BadRequest("This demo can't be claimed. It may already belong to an account.".into()));
    }
    tx.commit().await?;
    Ok(Json(auth.project(&state.db, body.project_id).await?))
}

#[derive(Serialize)]
struct ProjectDetail {
    project: Project,
    settings: ProjectSettings,
    latest_run: Option<AgentRun>,
}

async fn load_detail(state: &AppState, project: Project) -> AppResult<ProjectDetail> {
    let settings = settings::load_or_create(&state.db, project.id).await?;
    let latest_run = sqlx::query_as::<_, AgentRun>(
        "select * from agent_runs where project_id = $1 and kind = 'explore' order by created_at desc limit 1",
    )
    .bind(project.id)
    .fetch_optional(&state.db)
    .await?;
    Ok(ProjectDetail { project, settings, latest_run })
}

async fn detail(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectDetail>> {
    let project = auth.project(&state.db, id).await?;
    Ok(Json(load_detail(&state, project).await?))
}

#[derive(Deserialize)]
struct UpdateBody {
    name: Option<String>,
    company: Option<Value>,
}

async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> AppResult<Json<ProjectDetail>> {
    auth.project(&state.db, id).await?;
    if body.company.as_ref().is_some_and(|c| !c.is_object()) {
        return Err(AppError::BadRequest("company must be an object".into()));
    }
    // company is merged key by key so the UI can save one field without resending the whole profile.
    let project = sqlx::query_as::<_, Project>(
        "update projects set name = coalesce($2, name), company = company || coalesce($3, '{}'::jsonb)
         where id = $1 returning *",
    )
    .bind(id)
    .bind(body.name.as_deref().map(str::trim))
    .bind(body.company)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(load_detail(&state, project).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    auth.project(&state.db, id).await?;
    sqlx::query("delete from projects where id = $1").bind(id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, Default)]
struct ResearchBody {
    note: Option<String>,
}

async fn research(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    body: Option<Json<ResearchBody>>,
) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    let mut input = json!({ "mode": "app", "from_step": 1 });
    if let Some(note) = body.unwrap_or_default().0.note.filter(|n| !n.trim().is_empty()) {
        input["note"] = json!(note);
    }
    sqlx::query("update projects set status = 'researching' where id = $1").bind(id).execute(&state.db).await?;
    let run_id = agent::enqueue(&state.db, kind::EXPLORE, Some(id), Some(auth.org_id), input).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

/// Rewrites every unsent email in the project with the current writing settings, e.g. after the templates changed.
async fn rewrite_emails(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    let busy: bool = sqlx::query_scalar(
        "select exists (select 1 from agent_runs where project_id = $1 and kind = 'write_emails' and status in ('queued', 'running'))",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    if busy {
        return Err(AppError::Conflict("The agent is already writing emails for this project. Try again when it's done.".into()));
    }
    let lead_ids: Vec<Uuid> = sqlx::query_scalar(
        "select distinct lead_id from email_messages
         where project_id = $1 and direction = 'outbound' and kind in ('initial', 'follow_up') and status in ('draft', 'approved', 'scheduled')",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    if lead_ids.is_empty() {
        return Err(AppError::Conflict("There are no unsent emails to rewrite.".into()));
    }
    let input = json!({ "lead_ids": lead_ids, "rewrite": true });
    let run_id = agent::enqueue(&state.db, kind::WRITE_EMAILS, Some(id), Some(auth.org_id), input).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

async fn runs(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<AgentRun>>> {
    auth.project(&state.db, id).await?;
    let runs = sqlx::query_as::<_, AgentRun>(
        "select * from agent_runs where project_id = $1 order by created_at desc limit 50",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(runs))
}

async fn autopilot(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<crate::autopilot::Status>> {
    auth.project(&state.db, id).await?;
    settings::load_or_create(&state.db, id).await?;
    Ok(Json(crate::autopilot::status(&state.db, id).await?))
}

/// How the audit log names a project: its name, or its domain before it has one.
fn label(project: &Project) -> &str {
    if project.name.is_empty() { project.domain.as_deref().unwrap_or_default() } else { &project.name }
}

async fn start(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let project = auth.project(&state.db, id).await?;
    start_sending(&state.db, &auth, &project).await?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) const NO_MAILBOX: &str = "Connect an active mailbox before starting. Emails are sent from your own mailboxes.";

/// Emails are sent from the organization's own mailboxes, so a project can't start sending without one.
pub(super) async fn has_mailbox(db: &PgPool, org_id: Uuid) -> AppResult<bool> {
    Ok(sqlx::query_scalar("select exists(select 1 from mailboxes where org_id = $1 and status = 'active')").bind(org_id).fetch_one(db).await?)
}

/// The project's Start sending button. PATCH /v1/projects/{id} with sending true calls it too.
pub(super) async fn start_sending(db: &PgPool, who: impl Into<Who>, project: &Project) -> AppResult<()> {
    let who = who.into();
    // The app's toggle offers to connect a mailbox on this 400. The public API checks first and answers 409.
    if !has_mailbox(db, who.org_id()).await? {
        return Err(AppError::BadRequest(NO_MAILBOX.into()));
    }
    settings::load_or_create(db, project.id).await?;
    let mut tx = db.begin().await?;
    sqlx::query("update project_settings set paused = false where project_id = $1").bind(project.id).execute(&mut *tx).await?;
    sqlx::query("update projects set status = 'active' where id = $1").bind(project.id).execute(&mut *tx).await?;
    tx.commit().await?;
    audit::record(db, who, "project.started", Target::new("project", project.id, label(project)), json!({})).await;
    Ok(())
}

async fn pause(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let project = auth.project(&state.db, id).await?;
    pause_sending(&state.db, &auth, &project).await?;
    Ok(Json(json!({ "ok": true })))
}

/// The project's Pause sending button. PATCH /v1/projects/{id} with sending false calls it too.
pub(super) async fn pause_sending(db: &PgPool, who: impl Into<Who>, project: &Project) -> AppResult<()> {
    let mut tx = db.begin().await?;
    sqlx::query("update project_settings set paused = true where project_id = $1").bind(project.id).execute(&mut *tx).await?;
    sqlx::query("update projects set status = 'paused' where id = $1 and status = 'active'").bind(project.id).execute(&mut *tx).await?;
    tx.commit().await?;
    audit::record(db, who, "project.paused", Target::new("project", project.id, label(project)), json!({})).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::Config;

    struct Seeded {
        state: AppState,
        org: Uuid,
        user: Uuid,
        other_org: Uuid,
    }

    async fn seed(db: PgPool) -> Seeded {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let other_org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Other') returning id").fetch_one(&db).await.unwrap();
        let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('dana@acme.example', 'Dana', 'x') returning id")
            .fetch_one(&db)
            .await
            .unwrap();
        let state = AppState { db, cfg: Arc::new(Config::test()), events: tokio::sync::broadcast::channel(16).0, rate_limits: Default::default() };
        Seeded { state, org, user, other_org }
    }

    fn member(s: &Seeded, org_id: Uuid, role: &str) -> Auth {
        Auth { user_id: s.user, org_id, role: role.into(), token_hash: String::new() }
    }

    async fn make(s: &Seeded, org: Uuid, body: Value) -> AppResult<Project> {
        let body: CreateBody = serde_json::from_value(body).unwrap();
        create(State(s.state.clone()), member(s, org, "owner"), Json(body)).await.map(|Json(p)| p)
    }

    async fn runs_of(s: &Seeded, project: Uuid) -> Vec<(String, Value)> {
        sqlx::query_as("select kind, input from agent_runs where project_id = $1 order by created_at").bind(project).fetch_all(&s.state.db).await.unwrap()
    }

    fn refused<T>(result: AppResult<T>) -> String {
        match result {
            Err(AppError::BadRequest(m) | AppError::Conflict(m)) => m,
            Err(e) => panic!("{e:?}"),
            Ok(_) => panic!("accepted"),
        }
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_project_needs_a_website_or_a_description_and_starts_its_research(db: PgPool) {
        let s = seed(db).await;
        for body in [json!({}), json!({ "domain": "  ", "description": " " })] {
            assert_eq!(refused(make(&s, s.org, body).await), "Enter your website or describe your business");
        }
        assert_eq!(refused(make(&s, s.org, json!({ "domain": "not a website", "description": "Inventory software" })).await), "Enter a valid website, like acme.com");
        assert_eq!(sqlx::query_scalar::<_, i64>("select count(*) from projects").fetch_one(&s.state.db).await.unwrap(), 0);

        let site = make(&s, s.org, json!({ "domain": "https://www.Northwind.example/about" })).await.unwrap();
        assert_eq!((site.domain.as_deref(), site.name.as_str(), site.status.as_str(), site.org_id), (Some("northwind.example"), "northwind.example", "researching", Some(s.org)));
        assert_eq!(site.created_by, Some(s.user));
        let described = make(&s, s.org, json!({ "description": "  Inventory software for small warehouses.  " })).await.unwrap();
        assert_eq!((described.domain.as_deref(), described.business_description.as_deref(), described.name.as_str()), (None, Some("Inventory software for small warehouses."), ""));
        for project in [&site, &described] {
            assert_eq!(runs_of(&s, project.id).await, [("explore".to_string(), json!({ "mode": "app" }))]);
            let paused: bool = sqlx::query_scalar("select paused from project_settings where project_id = $1").bind(project.id).fetch_one(&s.state.db).await.unwrap();
            assert!(paused, "a new project doesn't send until it's started");
        }
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_list_counts_each_projects_sends_replies_hot_leads_and_live_campaigns(db: PgPool) {
        let s = seed(db).await;
        let older = make(&s, s.org, json!({ "domain": "older.example" })).await.unwrap();
        let newer = make(&s, s.org, json!({ "domain": "newer.example" })).await.unwrap();
        make(&s, s.other_org, json!({ "domain": "theirs.example" })).await.unwrap();
        sqlx::query("update projects set created_at = now() - interval '1 day' where id = $1").bind(older.id).execute(&s.state.db).await.unwrap();
        sqlx::query("update project_settings set paused = false where project_id = $1").bind(newer.id).execute(&s.state.db).await.unwrap();
        for status in ["working", "scaling", "draft", "paused"] {
            sqlx::query("insert into segments (project_id, name, status) values ($1, $2, $2)").bind(newer.id).bind(status).execute(&s.state.db).await.unwrap();
        }
        // Ana replied twice, Ed once with a reply nothing has classified yet, Bo only bounced, Cy is hot without a reply,
        // and Di booked a meeting.
        let mut leads = Vec::new();
        for (name, status) in [("ana", "replied"), ("bo", "bounced"), ("cy", "hot"), ("di", "meeting_booked"), ("ed", "replied")] {
            let id: Uuid = sqlx::query_scalar("insert into leads (project_id, email, status) values ($1, $2 || '@lead.example', $3) returning id")
                .bind(newer.id)
                .bind(name)
                .bind(status)
                .fetch_one(&s.state.db)
                .await
                .unwrap();
            leads.push(id);
        }
        let email = |lead: Uuid, direction: &'static str, status: &'static str, classification: Option<&'static str>| {
            let (db, project) = (s.state.db.clone(), newer.id);
            async move {
                sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status, classification) values ($1, $2, $3, case when $3 = 'inbound' then 'inbound' else 'initial' end, $4, $5)")
                    .bind(project)
                    .bind(lead)
                    .bind(direction)
                    .bind(status)
                    .bind(classification)
                    .execute(&db)
                    .await
                    .unwrap();
            }
        };
        for lead in &leads {
            email(*lead, "outbound", "sent", None).await;
        }
        email(leads[0], "outbound", "draft", None).await;
        email(leads[0], "inbound", "received", Some("interested")).await;
        email(leads[0], "inbound", "received", None).await;
        email(leads[1], "inbound", "received", Some("bounce")).await;
        email(leads[4], "inbound", "received", None).await;

        let Json(listed) = list(State(s.state.clone()), member(&s, s.org, "member")).await.unwrap();
        let rows: Vec<_> = listed.iter().map(|p| (p.project.id, p.sent, p.replies, p.hot, p.active_segments, p.paused)).collect();
        assert_eq!(rows, [(newer.id, 5, 2, 2, 2, false), (older.id, 0, 0, 0, 0, true)], "newest first, only this org's");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn editing_merges_the_company_profile_and_only_an_admin_deletes(db: PgPool) {
        let s = seed(db).await;
        let project = make(&s, s.org, json!({ "domain": "northwind.example" })).await.unwrap();
        let theirs = make(&s, s.other_org, json!({ "domain": "theirs.example" })).await.unwrap();
        let edit = |org: Uuid, id: Uuid, body: Value| {
            let state = s.state.clone();
            let auth = member(&s, org, "member");
            async move {
                let body: UpdateBody = serde_json::from_value(body).unwrap();
                update(State(state), auth, Path(id), Json(body)).await.map(|Json(d)| d)
            }
        };

        edit(s.org, project.id, json!({ "company": { "name": "Northwind", "country_code": "GB" } })).await.unwrap();
        let edited = edit(s.org, project.id, json!({ "name": "  Northwind  ", "company": { "country_code": "US" } })).await.unwrap();
        assert_eq!(edited.project.name, "Northwind");
        assert_eq!(edited.project.company, json!({ "name": "Northwind", "country_code": "US" }), "one key saved keeps the others");
        assert_eq!((edited.settings.project_id, edited.latest_run.map(|r| r.kind)), (project.id, Some("explore".to_string())));
        assert_eq!(edit(s.org, project.id, json!({})).await.unwrap().project.company["name"], "Northwind", "an empty edit changes nothing");
        assert_eq!(refused(edit(s.org, project.id, json!({ "company": ["Northwind"] })).await), "company must be an object");
        assert!(matches!(edit(s.org, theirs.id, json!({ "name": "Mine" })).await, Err(AppError::NotFound)));
        assert!(matches!(detail(State(s.state.clone()), member(&s, s.org, "member"), Path(theirs.id)).await, Err(AppError::NotFound)));

        let remove_as = |org: Uuid, role: &str, id: Uuid| remove(State(s.state.clone()), member(&s, org, role), Path(id));
        assert!(matches!(remove_as(s.org, "member", project.id).await, Err(AppError::Forbidden(_))));
        assert!(matches!(remove_as(s.org, "admin", theirs.id).await, Err(AppError::NotFound)));
        assert_eq!(remove_as(s.org, "admin", project.id).await.unwrap().0, json!({ "ok": true }));
        let left: Vec<Uuid> = sqlx::query_scalar("select id from projects").fetch_all(&s.state.db).await.unwrap();
        assert_eq!(left, [theirs.id]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn research_and_rewrites_queue_a_run_unless_theres_nothing_to_do(db: PgPool) {
        let s = seed(db).await;
        let project = make(&s, s.org, json!({ "domain": "northwind.example" })).await.unwrap();
        let theirs = make(&s, s.other_org, json!({ "domain": "theirs.example" })).await.unwrap();
        sqlx::query("update projects set status = 'ready' where id = $1").bind(project.id).execute(&s.state.db).await.unwrap();
        let owner = || member(&s, s.org, "member");

        let research_with = |body: Option<Value>| {
            let (state, auth) = (s.state.clone(), owner());
            async move { research(State(state), auth, Path(project.id), body.map(|b| Json(serde_json::from_value(b).unwrap()))).await.map(|Json(v)| v) }
        };
        let cases = [
            (None, json!({ "mode": "app", "from_step": 1 })),
            (Some(json!({ "note": "  " })), json!({ "mode": "app", "from_step": 1 })),
            (Some(json!({ "note": "Focus on the UK" })), json!({ "mode": "app", "from_step": 1, "note": "Focus on the UK" })),
        ];
        for (body, want) in cases {
            let queued = research_with(body.clone()).await.unwrap();
            let id: Uuid = serde_json::from_value(queued["run_id"].clone()).unwrap();
            let run: (String, Option<Uuid>, Value) =
                sqlx::query_as("select kind, org_id, input from agent_runs where id = $1 and project_id = $2").bind(id).bind(project.id).fetch_one(&s.state.db).await.unwrap();
            assert_eq!(run, ("explore".to_string(), Some(s.org), want), "{body:?}");
        }
        let status: String = sqlx::query_scalar("select status from projects where id = $1").bind(project.id).fetch_one(&s.state.db).await.unwrap();
        assert_eq!(status, "researching");
        assert!(matches!(research(State(s.state.clone()), owner(), Path(theirs.id), None).await, Err(AppError::NotFound)));

        let rewrite = || rewrite_emails(State(s.state.clone()), owner(), Path(project.id));
        assert_eq!(refused(rewrite().await), "There are no unsent emails to rewrite.");
        // Each unsent status on its own lead: Ana's approved first email and follow-up, Di's scheduled follow-up, Ed's draft.
        // Bo's went out, and Cy's is a reply draft, which a rewrite leaves alone.
        let mut leads = Vec::new();
        for name in ["ana", "bo", "cy", "di", "ed"] {
            let id: Uuid = sqlx::query_scalar("insert into leads (project_id, email) values ($1, $2 || '@lead.example') returning id")
                .bind(project.id)
                .bind(name)
                .fetch_one(&s.state.db)
                .await
                .unwrap();
            leads.push(id);
        }
        let unsent = [(0, "initial", "approved"), (0, "follow_up", "approved"), (3, "follow_up", "scheduled"), (4, "initial", "draft"), (1, "initial", "sent"), (2, "auto_reply", "draft")];
        for (lead, kind, status) in unsent {
            sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'outbound', $3, $4)")
                .bind(project.id)
                .bind(leads[lead])
                .bind(kind)
                .bind(status)
                .execute(&s.state.db)
                .await
                .unwrap();
        }
        let Json(started) = rewrite().await.unwrap();
        let (run, input): (Uuid, Value) =
            sqlx::query_as("select id, input from agent_runs where project_id = $1 and kind = 'write_emails'").bind(project.id).fetch_one(&s.state.db).await.unwrap();
        assert_eq!((started, &input["rewrite"]), (json!({ "run_id": run }), &json!(true)));
        let mut rewritten: Vec<Uuid> = serde_json::from_value(input["lead_ids"].clone()).unwrap();
        rewritten.sort();
        let mut want = vec![leads[0], leads[3], leads[4]];
        want.sort();
        assert_eq!(rewritten, want, "each lead with an unsent first email or follow-up, once");
        assert_eq!(refused(rewrite().await), "The agent is already writing emails for this project. Try again when it's done.");
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(run).execute(&s.state.db).await.unwrap();
        assert!(rewrite().await.is_ok(), "a finished writer doesn't block the next rewrite");

        // Each run a minute apart, the order they were made in shuffled.
        let ids: Vec<Uuid> = sqlx::query_scalar("select id from agent_runs where project_id = $1 order by id").bind(project.id).fetch_all(&s.state.db).await.unwrap();
        for (minutes, id) in ids.iter().enumerate() {
            sqlx::query("update agent_runs set created_at = now() - make_interval(mins => $2) where id = $1").bind(id).bind(minutes as i32).execute(&s.state.db).await.unwrap();
        }
        let Json(listed) = runs(State(s.state.clone()), owner(), Path(project.id)).await.unwrap();
        assert_eq!((ids.len(), listed.into_iter().map(|r| r.id).collect::<Vec<_>>()), (6, ids), "newest first");
        assert!(matches!(runs(State(s.state.clone()), owner(), Path(theirs.id)).await, Err(AppError::NotFound)));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn starting_needs_an_active_mailbox_and_pausing_keeps_a_project_thats_not_live(db: PgPool) {
        let s = seed(db).await;
        let project = make(&s, s.org, json!({ "domain": "northwind.example" })).await.unwrap();
        let theirs = make(&s, s.other_org, json!({ "domain": "theirs.example" })).await.unwrap();
        let owner = || member(&s, s.org, "member");
        let state_of = || async {
            let (status, paused): (String, bool) =
                sqlx::query_as("select p.status, ps.paused from projects p join project_settings ps on ps.project_id = p.id where p.id = $1")
                    .bind(project.id)
                    .fetch_one(&s.state.db)
                    .await
                    .unwrap();
            (status, paused)
        };

        // Another org's active mailbox, and this org's paused one, don't count.
        for (org, status) in [(s.other_org, "active"), (s.org, "paused")] {
            sqlx::query("insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, status) values ($1, gen_random_uuid() || '@mail.example', 'smtp.example', '', '', $2)")
                .bind(org)
                .bind(status)
                .execute(&s.state.db)
                .await
                .unwrap();
        }
        assert_eq!(refused(start(State(s.state.clone()), owner(), Path(project.id)).await), NO_MAILBOX);
        assert_eq!(state_of().await, ("researching".to_string(), true));

        sqlx::query("update mailboxes set status = 'active' where org_id = $1").bind(s.org).execute(&s.state.db).await.unwrap();
        assert!(matches!(start(State(s.state.clone()), owner(), Path(theirs.id)).await, Err(AppError::NotFound)));
        assert_eq!(start(State(s.state.clone()), owner(), Path(project.id)).await.unwrap().0, json!({ "ok": true }));
        assert_eq!(state_of().await, ("active".to_string(), false));
        assert_eq!(pause(State(s.state.clone()), owner(), Path(project.id)).await.unwrap().0, json!({ "ok": true }));
        assert_eq!(state_of().await, ("paused".to_string(), true));

        // A project still being researched stays that way when paused.
        sqlx::query("update projects set status = 'researching' where id = $1").bind(project.id).execute(&s.state.db).await.unwrap();
        sqlx::query("update project_settings set paused = false where project_id = $1").bind(project.id).execute(&s.state.db).await.unwrap();
        assert_eq!(pause(State(s.state.clone()), owner(), Path(project.id)).await.unwrap().0, json!({ "ok": true }));
        assert_eq!(state_of().await, ("researching".to_string(), true));

        let audit: Vec<(String, Option<Uuid>, String)> =
            sqlx::query_as("select action, actor_user_id, target_label from audit_events where target_id = $1 order by action").bind(project.id).fetch_all(&s.state.db).await.unwrap();
        let started_by = (s.user, "northwind.example".to_string());
        assert_eq!(audit, [
            ("project.paused".to_string(), Some(started_by.0), started_by.1.clone()),
            ("project.paused".to_string(), Some(started_by.0), started_by.1.clone()),
            ("project.started".to_string(), Some(started_by.0), started_by.1),
        ]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_demo_project_is_claimed_once_with_its_token(db: PgPool) {
        let s = seed(db).await;
        let demo: Uuid = sqlx::query_scalar("insert into projects (domain, name, claim_token) values ('demo.example', 'demo.example', 'secret-token') returning id")
            .fetch_one(&s.state.db)
            .await
            .unwrap();
        let run = agent::enqueue(&s.state.db, kind::EXPLORE, Some(demo), None, json!({ "mode": "demo" })).await.unwrap();
        let claim_as = |org: Uuid, token: &str| {
            let (state, auth) = (s.state.clone(), member(&s, org, "owner"));
            let body = ClaimBody { project_id: demo, token: token.into() };
            async move { claim_signed_in(State(state), auth, Json(body)).await.map(|Json(p)| p) }
        };
        let cant = "This demo can't be claimed. It may already belong to an account.";

        assert_eq!(refused(claim_as(s.org, "wrong-token").await), cant);
        let claimed = claim_as(s.org, "secret-token").await.unwrap();
        assert_eq!((claimed.org_id, claimed.created_by), (Some(s.org), Some(s.user)));
        let (token, run_org, settings): (Option<String>, Option<Uuid>, i64) = sqlx::query_as(
            "select p.claim_token, (select org_id from agent_runs where id = $2), (select count(*) from project_settings where project_id = p.id)
             from projects p where p.id = $1",
        )
        .bind(demo)
        .bind(run)
        .fetch_one(&s.state.db)
        .await
        .unwrap();
        assert_eq!((token, run_org, settings), (None, Some(s.org), 1), "its research run moves to the org with it");
        assert_eq!(refused(claim_as(s.other_org, "secret-token").await), cant, "a claimed demo can't be taken again");
        let owner: Option<Uuid> = sqlx::query_scalar("select org_id from projects where id = $1").bind(demo).fetch_one(&s.state.db).await.unwrap();
        assert_eq!(owner, Some(s.org));
    }
}
