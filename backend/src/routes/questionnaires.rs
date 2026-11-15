//! The Questionnaires page: upload or paste a buyer's questionnaire, review the agent's answers, approve them into the
//! answer library and download the file filled in. See questionnaires.rs.

use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::util_csv;
use super::util_table::{parse_csv, parse_spreadsheet_with_blank_rows};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::questionnaires::{self, Detail, MAX_ANSWER, Summary, Table};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/questionnaires", get(list).post(create))
        .route("/questionnaires/{id}", get(detail).delete(remove))
        .route("/questionnaires/{id}/answer", post(answer_again))
        .route("/questionnaires/{id}/approve", post(approve_sourced))
        .route("/questionnaires/{id}/download", get(download))
        .route("/questionnaire-items/{id}", post(save))
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024))
}

#[derive(Serialize)]
struct List {
    questionnaires: Vec<Summary>,
    /// Approved answers the next questionnaire starts from.
    library: i64,
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<List>> {
    auth.project(&state.db, id).await?;
    let library: i64 = sqlx::query_scalar("select count(*) from answer_library where project_id = $1").bind(id).fetch_one(&state.db).await?;
    Ok(Json(List { questionnaires: questionnaires::list(&state.db, id).await?, library }))
}

/// A questionnaire from a CSV or spreadsheet (`file`) or pasted questions (`text`), named `name`, which the agent starts
/// answering now.
async fn create(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, mut form: Multipart) -> AppResult<Json<Summary>> {
    auth.project(&state.db, id).await?;
    let unreadable = |e: axum::extract::multipart::MultipartError| AppError::BadRequest(e.body_text());
    let (mut name, mut file, mut text) = (String::new(), None, String::new());
    while let Some(field) = form.next_field().await.map_err(unreadable)? {
        match field.name() {
            Some("name") => name = field.text().await.map_err(unreadable)?,
            Some("text") => text = field.text().await.map_err(unreadable)?,
            Some("file") => {
                let filename = field.file_name().unwrap_or("questionnaire.csv").to_string();
                file = Some((filename, field.bytes().await.map_err(unreadable)?.to_vec()));
            }
            _ => {}
        }
    }
    let table = match file {
        Some((filename, bytes)) => {
            let lower = filename.to_lowercase();
            let table = if [".xlsx", ".xlsm", ".xls", ".ods"].iter().any(|ext| lower.ends_with(ext)) {
                let (headers, rows) = parse_spreadsheet_with_blank_rows(bytes, questionnaires::MAX_ROWS)?;
                Table { headers, rows }
            } else {
                let csv = parse_csv(&bytes)?;
                Table { headers: csv.headers, rows: csv.rows.into_iter().map(|(_, cells)| cells).collect() }
            };
            if name.trim().is_empty() {
                name = named_after(&filename);
            }
            table
        }
        None if !text.trim().is_empty() => Table::pasted(&text),
        None => return Err(AppError::BadRequest("Attach the questionnaire or paste its questions".into())),
    };
    let name = if name.trim().is_empty() { "Questionnaire".to_string() } else { name.trim().chars().take(200).collect() };
    let mut tx = state.db.begin().await?;
    let questionnaire = questionnaires::create(&mut tx, id, auth.user_id, &name, table).await?;
    let run = questionnaires::answer(&mut tx, questionnaire, id, auth.org_id, false).await?;
    tx.commit().await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(questionnaires::detail(&state.db, questionnaire).await?.summary))
}

/// "northpine_vendor-review.xlsx" as "Northpine vendor review".
fn named_after(filename: &str) -> String {
    let stem = filename.rsplit_once('.').map_or(filename, |(stem, _)| stem).replace(['-', '_'], " ");
    let mut chars = stem.trim().chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// The questionnaire's project, when it's in the organization.
async fn project_of(state: &AppState, auth: &Auth, id: Uuid) -> AppResult<Uuid> {
    sqlx::query_scalar("select q.project_id from questionnaires q join projects p on p.id = q.project_id where q.id = $1 and p.org_id = $2")
        .bind(id)
        .bind(auth.org_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}

async fn detail(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Detail>> {
    project_of(&state, &auth, id).await?;
    Ok(Json(questionnaires::detail(&state.db, id).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<()>> {
    project_of(&state, &auth, id).await?;
    sqlx::query("delete from questionnaires where id = $1").bind(id).execute(&state.db).await?;
    Ok(Json(()))
}

/// Has the agent try again on what it couldn't source, like after adding to Materials.
async fn answer_again(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Detail>> {
    let project_id = project_of(&state, &auth, id).await?;
    if questionnaires::detail(&state.db, id).await?.summary.answering {
        return Err(AppError::BadRequest("The agent is still answering it".into()));
    }
    let mut tx = state.db.begin().await?;
    let run = questionnaires::answer(&mut tx, id, project_id, auth.org_id, true).await?;
    tx.commit().await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(questionnaires::detail(&state.db, id).await?))
}

/// Approves every answer from the library or sourced from Materials.
async fn approve_sourced(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Detail>> {
    let project_id = project_of(&state, &auth, id).await?;
    questionnaires::approve_sourced(&state.db, id, project_id, auth.user_id).await?;
    Ok(Json(questionnaires::detail(&state.db, id).await?))
}

async fn download(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Response> {
    project_of(&state, &auth, id).await?;
    let (name, headers, rows) = questionnaires::export(&state.db, id).await?;
    let filename: String = name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
    util_csv::download(&format!("{} answered.csv", filename.trim()), &headers.iter().map(String::as_str).collect::<Vec<_>>(), rows)
}

#[derive(Deserialize)]
struct Save {
    answer: String,
    approve: bool,
}

/// Saves an answer, approved into the library or not, and returns the questionnaire.
async fn save(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Save>) -> AppResult<Json<Detail>> {
    if body.answer.chars().count() > MAX_ANSWER {
        return Err(AppError::BadRequest("That answer is too long".into()));
    }
    let questionnaire: Uuid = sqlx::query_scalar("select questionnaire_id from questionnaire_items where id = $1").bind(id).fetch_optional(&state.db).await?.ok_or(AppError::NotFound)?;
    project_of(&state, &auth, questionnaire).await?;
    questionnaires::save(&state.db, id, auth.user_id, &body.answer, body.approve).await?;
    Ok(Json(questionnaires::detail(&state.db, questionnaire).await?))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_file_names_its_questionnaire() {
        assert_eq!(super::named_after("northpine_vendor-review.xlsx"), "Northpine vendor review");
        assert_eq!(super::named_after(".csv"), "");
    }
}
