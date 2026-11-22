//! The first email editor's routes: My copy for a campaign and the project's default, for the app here and the public API in
//! api_v1/campaigns.rs and api_v1/projects.rs. The text itself lives in crate::my_copy.

use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::{settings, util_scope};
use crate::ab_test::{MIN_SENDS, beats};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::{Lead, Segment};
use crate::my_copy::{self, Fields, FirstEmail, Mode, Sender, Template};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/segments/{id}/my-copy", get(app_campaign).patch(app_update_campaign))
        .route("/segments/{id}/my-copy/source", put(app_set_source))
        .route("/segments/{id}/my-copy/preview", post(app_campaign_preview))
        .route("/projects/{id}/my-copy", get(app_project).patch(app_update_project))
        .route("/projects/{id}/my-copy/preview", post(app_project_preview))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Project,
    Campaign,
}

/// Send only what changes. Also a preview's unsaved text.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    mode: Option<Mode>,
    subject: Option<String>,
    body: Option<String>,
}

impl Update {
    fn apply(self, copy: &FirstEmail) -> FirstEmail {
        FirstEmail {
            mode: self.mode.unwrap_or(copy.mode),
            subject: self.subject.map_or_else(|| copy.subject.clone(), |s| s.trim().to_string()),
            body: self.body.map_or_else(|| copy.body.clone(), |b| b.trim().to_string()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBody {
    pub source: Source,
}

/// Sent first emails of one side of the comparison, and the leads of those who replied (bounces aside).
#[derive(Debug, Default, Serialize)]
pub struct Side {
    sent: i64,
    replies: i64,
}

/// The campaign's own text against the AI email, on first emails written while My copy was on.
#[derive(Serialize)]
pub struct Results {
    my_copy: Side,
    ai: Side,
    note: String,
}

#[derive(Serialize)]
pub struct CampaignCopy {
    campaign_id: Uuid,
    project_id: Uuid,
    /// `project` sends the project's first email, so mode, subject and body are the project's.
    source: Source,
    #[serde(flatten)]
    copy: FirstEmail,
    /// What the AI email is written from for the other leads: the campaign's angle.
    instructions: String,
    results: Results,
}

#[derive(Serialize)]
pub struct ProjectCopy {
    project_id: Uuid,
    #[serde(flatten)]
    copy: FirstEmail,
    /// The project's extra instructions for every AI email.
    instructions: String,
    /// Campaigns sending this first email, the ones with source `project`.
    campaign_ids: Vec<Uuid>,
}

#[derive(Serialize)]
pub struct PreviewEmail {
    lead_id: Uuid,
    lead_name: String,
    lead_company: String,
    /// `my_copy`, or `ai` for a lead that gets the AI email instead: one in ten, or one missing a variable.
    gets: &'static str,
    subject: String,
    body: String,
    sender_name: String,
    /// Why the lead gets the AI email, when it does.
    note: Option<String>,
}

#[derive(Serialize)]
pub struct Preview {
    mode: Mode,
    emails: Vec<PreviewEmail>,
}

struct Project {
    copy: FirstEmail,
    instructions: String,
    sender_name: String,
}

async fn project_in_org(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<()> {
    let found: bool = sqlx::query_scalar("select exists (select 1 from projects where id = $1 and org_id = $2)").bind(id).bind(org_id).fetch_one(db).await?;
    if found { Ok(()) } else { Err(AppError::NotFound) }
}

async fn load_project(db: &PgPool, project_id: Uuid) -> AppResult<Project> {
    let s = settings::load_or_create(db, project_id).await?;
    let (mode, subject, body): (String, String, String) =
        sqlx::query_as("select my_copy_mode, my_copy_subject, my_copy_body from project_settings where project_id = $1")
            .bind(project_id)
            .fetch_one(db)
            .await?;
    Ok(Project { copy: FirstEmail::from_db(&mode, subject, body), instructions: s.email_instructions, sender_name: s.sender_name })
}

/// The campaign's source and its own first email, None until it first had one.
async fn own(db: &PgPool, segment_id: Uuid) -> AppResult<(Source, Option<FirstEmail>)> {
    let (source, mode, subject, body): (String, Option<String>, String, String) =
        sqlx::query_as("select my_copy_source, my_copy_mode, my_copy_subject, my_copy_body from segments where id = $1")
            .bind(segment_id)
            .fetch_one(db)
            .await?;
    let source = if source == "campaign" { Source::Campaign } else { Source::Project };
    Ok((source, mode.map(|m| FirstEmail::from_db(&m, subject, body))))
}

async fn results(db: &PgPool, segment_id: Uuid) -> AppResult<Results> {
    let rows: Vec<(bool, i64, i64)> = sqlx::query_as(
        "select e.my_copy, count(*),
                count(*) filter (where exists (select 1 from email_messages i where i.lead_id = e.lead_id and i.direction = 'inbound'
                                               and i.classification is distinct from 'bounce'))
         from email_messages e
         where e.segment_id = $1 and e.kind = 'initial' and e.direction = 'outbound' and e.status = 'sent' and e.my_copy is not null
         group by e.my_copy",
    )
    .bind(segment_id)
    .fetch_all(db)
    .await?;
    let side = |which: bool| rows.iter().find(|r| r.0 == which).map_or_else(Side::default, |r| Side { sent: r.1, replies: r.2 });
    let (mine, ai) = (side(true), side(false));
    let rate = |s: &Side| s.replies as f64 / s.sent.max(1) as f64 * 100.0;
    let note = if mine.sent < MIN_SENDS || ai.sent < MIN_SENDS {
        format!("Not enough data yet. Each side needs at least {MIN_SENDS} sends before the reply rates mean much.")
    } else if beats(mine.replies, mine.sent, ai.replies, ai.sent) {
        format!("Your text wins on reply rate, {:.1}% against {:.1}% for the AI email, too big a gap to be chance.", rate(&mine), rate(&ai))
    } else if beats(ai.replies, ai.sent, mine.replies, mine.sent) {
        format!("The AI email wins on reply rate, {:.1}% against {:.1}% for your text, too big a gap to be chance.", rate(&ai), rate(&mine))
    } else {
        format!("No clear winner yet: {:.1}% reply rate for your text and {:.1}% for the AI email.", rate(&mine), rate(&ai))
    };
    Ok(Results { my_copy: mine, ai, note })
}

async fn campaign_copy(db: &PgPool, segment: &Segment) -> AppResult<CampaignCopy> {
    let (source, own) = own(db, segment.id).await?;
    let copy = match (source, own) {
        (Source::Campaign, Some(own)) => own,
        _ => load_project(db, segment.project_id).await?.copy,
    };
    Ok(CampaignCopy {
        campaign_id: segment.id,
        project_id: segment.project_id,
        source,
        copy,
        instructions: segment.angle.clone(),
        results: results(db, segment.id).await?,
    })
}

pub async fn campaign(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<CampaignCopy> {
    let segment: Segment = util_scope::load(db, "segments", id, org_id).await?;
    campaign_copy(db, &segment).await
}

/// The campaign's own first email. One that sends the project's isn't changed here.
pub async fn update_campaign(db: &PgPool, org_id: Uuid, id: Uuid, update: Update) -> AppResult<CampaignCopy> {
    let segment: Segment = util_scope::load(db, "segments", id, org_id).await?;
    let (Source::Campaign, Some(current)) = own(db, segment.id).await? else {
        return Err(AppError::Conflict(
            "This campaign sends the project's first email. Change the project's, or switch the campaign to its own first.".into(),
        ));
    };
    let next = update.apply(&current);
    // A text already saved passed the checks then.
    if next != current {
        my_copy::check(&next, &load_project(db, segment.project_id).await?.sender_name)?;
        sqlx::query("update segments set my_copy_mode = $2, my_copy_subject = $3, my_copy_body = $4 where id = $1")
            .bind(segment.id)
            .bind(next.mode.as_str())
            .bind(&next.subject)
            .bind(&next.body)
            .execute(db)
            .await?;
    }
    campaign_copy(db, &segment).await
}

/// From project or Custom. A campaign's first own text starts as a copy of the project's, so what its leads get doesn't change
/// until it's edited. Switching back keeps it.
pub async fn set_source(db: &PgPool, org_id: Uuid, id: Uuid, source: Source) -> AppResult<CampaignCopy> {
    let segment: Segment = util_scope::load(db, "segments", id, org_id).await?;
    let (_, own) = own(db, segment.id).await?;
    if source == Source::Campaign && own.is_none() {
        let project = load_project(db, segment.project_id).await?.copy;
        sqlx::query("update segments set my_copy_mode = $2, my_copy_subject = $3, my_copy_body = $4 where id = $1 and my_copy_mode is null")
            .bind(segment.id)
            .bind(project.mode.as_str())
            .bind(&project.subject)
            .bind(&project.body)
            .execute(db)
            .await?;
    }
    let source_str = if source == Source::Campaign { "campaign" } else { "project" };
    sqlx::query("update segments set my_copy_source = $2 where id = $1").bind(segment.id).bind(source_str).execute(db).await?;
    campaign_copy(db, &segment).await
}

/// The text the campaign sends now, or `draft` over it, rendered for its first 3 leads.
pub async fn campaign_preview(db: &PgPool, org_id: Uuid, id: Uuid, draft: Update) -> AppResult<Preview> {
    let segment: Segment = util_scope::load(db, "segments", id, org_id).await?;
    let copy = campaign_copy(db, &segment).await?.copy;
    preview(db, segment.project_id, Some(segment.id), &draft.apply(&copy)).await
}

async fn project_copy(db: &PgPool, project_id: Uuid) -> AppResult<ProjectCopy> {
    let project = load_project(db, project_id).await?;
    let campaign_ids: Vec<Uuid> = sqlx::query_scalar("select id from segments where project_id = $1 and my_copy_source = 'project' order by position, created_at")
        .bind(project_id)
        .fetch_all(db)
        .await?;
    Ok(ProjectCopy { project_id, copy: project.copy, instructions: project.instructions, campaign_ids })
}

pub async fn project(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<ProjectCopy> {
    project_in_org(db, org_id, id).await?;
    project_copy(db, id).await
}

/// The project's default first email. Campaigns with their own aren't touched.
pub async fn update_project(db: &PgPool, org_id: Uuid, id: Uuid, update: Update) -> AppResult<ProjectCopy> {
    project_in_org(db, org_id, id).await?;
    let project = load_project(db, id).await?;
    let next = update.apply(&project.copy);
    if next != project.copy {
        my_copy::check(&next, &project.sender_name)?;
        sqlx::query("update project_settings set my_copy_mode = $2, my_copy_subject = $3, my_copy_body = $4 where project_id = $1")
            .bind(id)
            .bind(next.mode.as_str())
            .bind(&next.subject)
            .bind(&next.body)
            .execute(db)
            .await?;
    }
    project_copy(db, id).await
}

pub async fn project_preview(db: &PgPool, org_id: Uuid, id: Uuid, draft: Update) -> AppResult<Preview> {
    project_in_org(db, org_id, id).await?;
    let copy = load_project(db, id).await?.copy;
    preview(db, id, None, &draft.apply(&copy)).await
}

/// Leads a preview renders for.
const PREVIEW_LEADS: i64 = 3;

/// No Claude call: an {{ai: …}} line shows its instruction, since the writer only writes it with the email.
async fn preview(db: &PgPool, project_id: Uuid, segment_id: Option<Uuid>, copy: &FirstEmail) -> AppResult<Preview> {
    if copy.body.trim().is_empty() {
        return Ok(Preview { mode: copy.mode, emails: vec![] });
    }
    let template = Template::new(&copy.subject, &copy.body).map_err(AppError::Unprocessable)?;
    let sender = Sender::load(db, project_id).await?;
    let leads: Vec<Lead> = sqlx::query_as(
        "select * from leads where project_id = $1 and ($2::uuid is null or segment_id = $2) and email is not null order by created_at, id limit $3",
    )
    .bind(project_id)
    .bind(segment_id)
    .bind(PREVIEW_LEADS)
    .fetch_all(db)
    .await?;
    let cities = my_copy::company_cities(db, &leads).await?;
    let markers: Vec<String> = template.ai_lines().iter().map(|i| format!("[Written for each lead: {i}]")).collect();
    let emails = leads
        .iter()
        .map(|lead| {
            let email = |gets, subject: String, body: String, note: Option<String>| PreviewEmail {
                lead_id: lead.id,
                lead_name: lead.full_name.clone(),
                lead_company: lead.company_name.clone(),
                gets,
                subject,
                body,
                sender_name: sender.name.clone(),
                note,
            };
            if my_copy::gets_ai_email(lead.id) {
                let note = "One lead in ten gets the AI email instead, so you can compare the two. This lead is one of them.";
                return email("ai", String::new(), String::new(), Some(note.into()));
            }
            match template.render(&Fields { lead, company_city: my_copy::company_city(lead, &cities), sender: &sender }) {
                Ok(text) => email("my_copy", text.subject.trim().to_string(), template.sign(&text.body(&markers), &sender), None),
                Err(name) => {
                    let note = format!("This lead has no {{{{{name}}}}}, so it gets the AI email. A fallback, like {{{{{name}|…}}}}, sends your text anyway.");
                    email("ai", String::new(), String::new(), Some(note))
                }
            }
        })
        .collect();
    Ok(Preview { mode: copy.mode, emails })
}

async fn app_campaign(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<CampaignCopy>> {
    Ok(Json(campaign(&state.db, auth.org_id, id).await?))
}

async fn app_update_campaign(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Update>) -> AppResult<Json<CampaignCopy>> {
    Ok(Json(update_campaign(&state.db, auth.org_id, id, body).await?))
}

async fn app_set_source(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<SourceBody>) -> AppResult<Json<CampaignCopy>> {
    Ok(Json(set_source(&state.db, auth.org_id, id, body.source).await?))
}

async fn app_campaign_preview(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Update>) -> AppResult<Json<Preview>> {
    Ok(Json(campaign_preview(&state.db, auth.org_id, id, body).await?))
}

async fn app_project(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectCopy>> {
    Ok(Json(project(&state.db, auth.org_id, id).await?))
}

async fn app_update_project(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Update>) -> AppResult<Json<ProjectCopy>> {
    Ok(Json(update_project(&state.db, auth.org_id, id, body).await?))
}

async fn app_project_preview(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Update>) -> AppResult<Json<Preview>> {
    Ok(Json(project_preview(&state.db, auth.org_id, id, body).await?))
}
