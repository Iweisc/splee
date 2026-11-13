//! Research questions on the Companies page: the questions, each company's answers as a grid, and starting the runs that
//! answer them. The runs themselves are agent/research.rs.

use std::collections::{BTreeMap, HashMap};

use axum::extract::{Path, Query, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::util_input;
use super::util_pagination::PageQuery;
use crate::agent::research::{self, MAX_COMPANIES, MAX_QUESTIONS};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::{ResearchAnswer, ResearchQuestion};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/research", get(grid))
        .route("/projects/{id}/research/questions", post(create_question))
        .route("/projects/{id}/research/run", post(start))
        .route("/research/questions/{id}", patch(update_question).delete(delete_question))
}

pub async fn questions(db: &PgPool, project_id: Uuid) -> sqlx::Result<Vec<ResearchQuestion>> {
    sqlx::query_as::<_, ResearchQuestion>("select * from research_questions where project_id = $1 order by position, created_at")
        .bind(project_id)
        .fetch_all(db)
        .await
}

#[derive(Deserialize, Default)]
pub struct GridQuery {
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    segment_id: Option<Uuid>,
    q: Option<String>,
    /// With `answer`, only companies whose answer to this question is `answer`.
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    question_id: Option<Uuid>,
    /// yes, no, answered (any answer), unknown, or none (not researched yet, or failed).
    answer: Option<String>,
}

#[derive(Serialize, FromRow)]
pub struct GridRow {
    id: Uuid,
    domain: String,
    name: String,
    score: i16,
    score_fit: i16,
    score_intent: i16,
    status: String,
    segment_name: Option<String>,
    /// By question id.
    #[sqlx(skip)]
    answers: HashMap<Uuid, ResearchAnswer>,
}

#[derive(Serialize)]
pub struct Grid {
    questions: Vec<ResearchQuestion>,
    items: Vec<GridRow>,
    total: i64,
    /// Answers being researched right now, across the project, so the page knows to refresh.
    researching: i64,
}

const ANSWERS: [&str; 5] = ["yes", "no", "answered", "unknown", "none"];

/// The project's companies, best score first, with their answers. The caller checks the project belongs to its organization.
pub async fn grid_page(db: &PgPool, project_id: Uuid, q: &GridQuery, page: &PageQuery) -> AppResult<Grid> {
    let answer = q.answer.as_deref().map(str::trim).filter(|a| !a.is_empty());
    if let Some(a) = answer {
        util_input::one_of("answer", a, &ANSWERS)?;
    }
    let filter_on = q.question_id.filter(|_| answer.is_some());
    let like = util_input::like_pattern(q.q.as_deref());
    // $4 is the question and $5 the answer the filter wants. "none" also matches failed and queued answers.
    let filter = "from companies c left join segments s on s.id = c.segment_id
         where c.project_id = $1 and ($2::uuid is null or c.segment_id = $2) and ($3::text is null or c.name ilike $3 or c.domain::text ilike $3)
           and ($4::uuid is null or case $5
                 when 'none' then not exists (select 1 from research_answers a where a.question_id = $4 and a.domain = c.domain and a.status in ('answered', 'unknown'))
                 else exists (select 1 from research_answers a where a.question_id = $4 and a.domain = c.domain and case $5
                   when 'unknown' then a.status = 'unknown'
                   when 'answered' then a.status = 'answered'
                   else a.status = 'answered' and a.answer = $5 end) end)";
    let total: i64 = sqlx::query_scalar(&format!("select count(*) {filter}"))
        .bind(project_id)
        .bind(q.segment_id)
        .bind(&like)
        .bind(filter_on)
        .bind(answer)
        .fetch_one(db)
        .await?;
    let mut items = sqlx::query_as::<_, GridRow>(&format!(
        "select c.id, c.domain::text as domain, c.name, c.score, c.score_fit, c.score_intent, c.status, s.name as segment_name {filter}
         order by c.score desc, c.created_at desc, c.id desc limit $6 offset $7"
    ))
    .bind(project_id)
    .bind(q.segment_id)
    .bind(&like)
    .bind(filter_on)
    .bind(answer)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(db)
    .await?;
    let domains: Vec<String> = items.iter().map(|c| c.domain.clone()).collect();
    let mut by_domain: HashMap<String, HashMap<Uuid, ResearchAnswer>> = HashMap::new();
    let mut researching = 0;
    for a in research::answers(db, project_id, None).await? {
        if a.status == "queued" {
            researching += 1;
        }
        if domains.iter().any(|d| d.eq_ignore_ascii_case(&a.domain)) {
            by_domain.entry(a.domain.to_lowercase()).or_default().insert(a.question_id, a);
        }
    }
    for c in &mut items {
        c.answers = by_domain.remove(&c.domain.to_lowercase()).unwrap_or_default();
    }
    Ok(Grid { questions: questions(db, project_id).await?, items, total, researching })
}

async fn grid(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(q): Query<GridQuery>, Query(page): Query<PageQuery>) -> AppResult<Json<Grid>> {
    auth.project(&state.db, id).await?;
    Ok(Json(grid_page(&state.db, id, &q, &page).await?))
}

#[derive(Deserialize)]
pub struct QuestionBody {
    label: Option<String>,
    question: Option<String>,
    answer_type: Option<String>,
    use_in_emails: Option<bool>,
}

/// The trimmed text, refusing an empty or overlong one.
fn text(field: &str, value: &str, max: usize) -> AppResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::BadRequest(format!("{field} is required")));
    }
    if value.chars().count() > max {
        return Err(AppError::BadRequest(format!("{field} must be at most {max} characters")));
    }
    Ok(value.to_string())
}

/// A column name from the question when none is given: its first few words.
fn label_from(question: &str) -> String {
    let words: Vec<&str> = question.trim_end_matches('?').split_whitespace().take(5).collect();
    words.join(" ").chars().take(40).collect()
}

pub async fn create(db: &PgPool, project_id: Uuid, body: QuestionBody) -> AppResult<ResearchQuestion> {
    let question = text("question", body.question.as_deref().unwrap_or_default(), 300)?;
    let label = match body.label.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        Some(l) => text("label", l, 40)?,
        None => label_from(&question),
    };
    let answer_type = body.answer_type.unwrap_or_else(|| "yes_no".into());
    util_input::one_of("answer_type", &answer_type, &["yes_no", "text"])?;
    let mut tx = db.begin().await?;
    // Under a lock on the project so two adds at once can't pass the cap together.
    sqlx::query("select 1 from projects where id = $1 for update").bind(project_id).execute(&mut *tx).await?;
    let count: i64 = sqlx::query_scalar("select count(*) from research_questions where project_id = $1").bind(project_id).fetch_one(&mut *tx).await?;
    if count >= MAX_QUESTIONS {
        return Err(AppError::BadRequest(format!("A project can have up to {MAX_QUESTIONS} research questions")));
    }
    let q = sqlx::query_as::<_, ResearchQuestion>(
        "insert into research_questions (project_id, label, question, answer_type, use_in_emails, position)
         values ($1, $2, $3, $4, $5, (select coalesce(max(position) + 1, 0) from research_questions where project_id = $1)) returning *",
    )
    .bind(project_id)
    .bind(&label)
    .bind(&question)
    .bind(&answer_type)
    .bind(body.use_in_emails.unwrap_or(false))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(q)
}

async fn create_question(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<QuestionBody>) -> AppResult<Json<ResearchQuestion>> {
    auth.project(&state.db, id).await?;
    Ok(Json(create(&state.db, id, body).await?))
}

async fn update_question(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<QuestionBody>) -> AppResult<Json<ResearchQuestion>> {
    let current: ResearchQuestion = super::util_scope::load(&state.db, "research_questions", id, auth.org_id).await?;
    let label = match body.label.as_deref() {
        Some(l) => text("label", l, 40)?,
        None => current.label.clone(),
    };
    let question = match body.question.as_deref() {
        Some(q) => text("question", q, 300)?,
        None => current.question.clone(),
    };
    let answer_type = body.answer_type.unwrap_or_else(|| current.answer_type.clone());
    util_input::one_of("answer_type", &answer_type, &["yes_no", "text"])?;
    let mut tx = state.db.begin().await?;
    // Answers to the old wording, or of the old type, would show under a question they don't answer.
    if question != current.question || answer_type != current.answer_type {
        sqlx::query("delete from research_answers where question_id = $1").bind(id).execute(&mut *tx).await?;
    }
    let q = sqlx::query_as::<_, ResearchQuestion>(
        "update research_questions set label = $2, question = $3, answer_type = $4, use_in_emails = $5 where id = $1 returning *",
    )
    .bind(id)
    .bind(&label)
    .bind(&question)
    .bind(&answer_type)
    .bind(body.use_in_emails.unwrap_or(current.use_in_emails))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(q))
}

async fn delete_question(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<serde_json::Value>> {
    let q: ResearchQuestion = super::util_scope::load(&state.db, "research_questions", id, auth.org_id).await?;
    sqlx::query("delete from research_questions where id = $1").bind(q.id).execute(&state.db).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize, Default)]
pub struct StartBody {
    /// Every question without it.
    #[serde(default)]
    question_ids: Option<Vec<Uuid>>,
    /// Every company that isn't excluded without it, in `segment_id` when that's set.
    #[serde(default)]
    company_ids: Option<Vec<Uuid>>,
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    segment_id: Option<Uuid>,
    /// Asks again questions a company already has an answer to.
    #[serde(default)]
    redo: bool,
    /// Counts what it would queue without queueing it.
    #[serde(default)]
    dry_run: bool,
}

#[derive(Serialize)]
pub struct Started {
    /// Companies queued, at most MAX_COMPANIES.
    companies: usize,
    /// Answers queued.
    answers: usize,
    /// Companies left over past the cap, for another go.
    more: usize,
    runs: usize,
    /// What one company cost on average in this project's research runs so far, in micro-dollars, for the estimate.
    cost_per_company_micros: Option<i64>,
}

/// Queues research for the request: per company, the picked questions it has no answer to yet, or all of them with
/// `redo`. Companies that need the same questions share runs. The caller checks the project belongs to its organization.
pub async fn start_research(db: &PgPool, project_id: Uuid, org_id: Uuid, body: StartBody) -> AppResult<Started> {
    let all = questions(db, project_id).await?;
    let picked: Vec<Uuid> = match &body.question_ids {
        Some(ids) => all.iter().map(|q| q.id).filter(|id| ids.contains(id)).collect(),
        None => all.iter().map(|q| q.id).collect(),
    };
    if picked.is_empty() {
        return Err(AppError::BadRequest("Add a research question first".into()));
    }
    let domains: Vec<String> = sqlx::query_scalar(
        "select domain::text from companies where project_id = $1
           and case when $2::uuid[] is null then status <> 'excluded' and ($3::uuid is null or segment_id = $3) else id = any($2) end
         order by score desc, created_at desc",
    )
    .bind(project_id)
    .bind(&body.company_ids)
    .bind(body.segment_id)
    .fetch_all(db)
    .await?;
    // What each domain already has: answered, unknown, or still researching, all of which a new run would repeat.
    let have: HashMap<(String, Uuid), String> =
        research::answers(db, project_id, None).await?.into_iter().map(|a| ((a.domain.to_lowercase(), a.question_id), a.status)).collect();
    let mut groups: BTreeMap<Vec<Uuid>, Vec<String>> = BTreeMap::new();
    let mut more = 0;
    let mut queued = 0;
    for domain in domains {
        let lower = domain.to_lowercase();
        let needed: Vec<Uuid> = picked
            .iter()
            .copied()
            .filter(|q| {
                match have.get(&(lower.clone(), *q)).map(String::as_str) {
                    Some("queued") => false,
                    Some("answered" | "unknown") => body.redo,
                    _ => true,
                }
            })
            .collect();
        if needed.is_empty() {
            continue;
        }
        if queued == MAX_COMPANIES {
            more += 1;
            continue;
        }
        queued += 1;
        groups.entry(needed).or_default().push(domain);
    }
    let answers = groups.iter().map(|(q, d)| q.len() * d.len()).sum();
    let mut runs = 0;
    if !body.dry_run {
        for (questions, domains) in &groups {
            runs += research::enqueue(db, project_id, org_id, questions, domains).await?;
        }
    }
    let cost_per_company_micros: Option<i64> = sqlx::query_scalar(
        "select (sum(cost_usd) * 1000000 / nullif(sum((output->>'companies')::int), 0))::bigint from agent_runs
         where project_id = $1 and kind = 'research_companies' and status = 'succeeded'",
    )
    .bind(project_id)
    .fetch_one(db)
    .await?;
    Ok(Started { companies: queued, answers, more, runs, cost_per_company_micros })
}

async fn start(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<StartBody>) -> AppResult<Json<Started>> {
    auth.project(&state.db, id).await?;
    Ok(Json(start_research(&state.db, id, auth.org_id, body).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn research_queues_only_missing_answers_and_the_grid_filters_by_answer(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(&db).await.unwrap();
        for (domain, status) in [("a.example", "new"), ("b.example", "new"), ("c.example", "excluded")] {
            sqlx::query("insert into companies (project_id, domain, name, status) values ($1, $2, $2, $3)").bind(project).bind(domain).bind(status).execute(&db).await.unwrap();
        }
        let body = |redo: bool, dry_run: bool| StartBody { redo, dry_run, ..Default::default() };
        assert!(start_research(&db, project, org, body(false, false)).await.is_err(), "no questions yet");
        let hospitals = create(&db, project, QuestionBody { label: None, question: Some("Do they sell to hospitals?".into()), answer_type: None, use_in_emails: None }).await.unwrap();
        assert_eq!((hospitals.label.as_str(), hospitals.answer_type.as_str()), ("Do they sell to hospitals", "yes_no"));
        create(&db, project, QuestionBody { label: Some("CRM".into()), question: Some("Which CRM?".into()), answer_type: Some("text".into()), use_in_emails: None }).await.unwrap();

        let dry = start_research(&db, project, org, body(false, true)).await.unwrap();
        assert_eq!((dry.companies, dry.answers, dry.runs), (2, 4, 0), "the excluded company is left out");
        assert!(research::answers(&db, project, None).await.unwrap().is_empty(), "a dry run queues nothing");

        // a.example already knows about hospitals, so it only needs the CRM question, in a run of its own.
        sqlx::query("insert into research_answers (question_id, project_id, domain, status, answer) values ($1, $2, 'a.example', 'answered', 'yes')")
            .bind(hospitals.id)
            .bind(project)
            .execute(&db)
            .await
            .unwrap();
        let started = start_research(&db, project, org, body(false, false)).await.unwrap();
        assert_eq!((started.companies, started.answers, started.runs), (2, 3, 2));
        let again = start_research(&db, project, org, body(false, false)).await.unwrap();
        assert_eq!(again.companies, 0, "queued answers aren't queued twice");

        let page = PageQuery { page: None, per_page: None };
        let filter = |answer: &str| GridQuery { question_id: Some(hospitals.id), answer: Some(answer.into()), ..Default::default() };
        let yes = grid_page(&db, project, &filter("yes"), &page).await.unwrap();
        assert_eq!(yes.items.iter().map(|c| c.domain.as_str()).collect::<Vec<_>>(), ["a.example"]);
        assert_eq!(yes.items[0].answers[&hospitals.id].answer, "yes");
        assert_eq!(yes.researching, 3);
        let none = grid_page(&db, project, &filter("none"), &page).await.unwrap();
        assert_eq!(none.total, 2, "b.example is still researching and c.example was never asked");
        assert!(grid_page(&db, project, &filter("maybe"), &page).await.is_err());

        // Renaming keeps the answers, changing the wording drops them.
        let auth = |org_id| Auth { user_id: Uuid::nil(), org_id, role: "owner".into(), token_hash: String::new() };
        let state = crate::autopilot::tests::state(db.clone());
        let edit = |label: Option<&str>, question: Option<&str>| QuestionBody { label: label.map(Into::into), question: question.map(Into::into), answer_type: None, use_in_emails: Some(true) };
        let other_org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Other') returning id").fetch_one(&db).await.unwrap();
        assert!(update_question(State(state.clone()), auth(other_org), Path(hospitals.id), Json(edit(Some("Mine"), None))).await.is_err());
        let Json(renamed) = update_question(State(state.clone()), auth(org), Path(hospitals.id), Json(edit(Some("Hospitals"), None))).await.unwrap();
        assert!(renamed.use_in_emails && renamed.label == "Hospitals");
        assert_eq!(grid_page(&db, project, &filter("yes"), &page).await.unwrap().total, 1);
        let _ = update_question(State(state), auth(org), Path(hospitals.id), Json(edit(None, Some("Do they run a hospital?")))).await.unwrap();
        assert_eq!(grid_page(&db, project, &filter("yes"), &page).await.unwrap().total, 0);
    }
}
