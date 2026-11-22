//! Questionnaires: a buyer's security questionnaire or RFP, kept with its own columns. The agent answers each question
//! (agent/questionnaires.rs), the team approves the answers, and each approved answer joins the project's answer library,
//! which answers the same question in the next questionnaire before the agent reads anything.

use serde::Serialize;
use serde_json::json;
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::text::plain;

/// Questions one questionnaire takes, and rows with the ones that aren't questions.
pub const MAX_QUESTIONS: usize = 500;
pub const MAX_ROWS: usize = 5000;
pub const MAX_ANSWER: usize = 4000;

/// A question's library key: its words, lowercase.
pub fn key(question: &str) -> String {
    plain(question)
}

/// The column the questions are in: one named just "Question", else the one with the longest text of those with it in
/// their name, so "Question ID" loses to "Control question", else the one with the longest text.
fn question_column(headers: &[String], rows: &[Vec<String>]) -> Option<usize> {
    const WORDS: [&str; 3] = ["question", "requirement", "query"];
    let lower: Vec<String> = headers.iter().map(|h| h.trim().to_lowercase()).collect();
    let text = |c: usize| rows.iter().map(|r| r.get(c).map_or(0, |s| s.chars().count())).sum::<usize>();
    // The first of the longest, since with no rows to measure every column ties.
    let longest = |columns: Vec<usize>| columns.into_iter().min_by_key(|&c| std::cmp::Reverse(text(c)));
    let exact = lower.iter().position(|h| WORDS.iter().any(|&w| h == w || h.strip_suffix('s') == Some(w)));
    exact
        .or_else(|| longest((0..headers.len()).filter(|&c| WORDS.iter().any(|w| lower[c].contains(w))).collect()))
        .or_else(|| longest((0..headers.len()).filter(|&c| text(c) > 0).collect()))
}

/// The file's own column for answers, like "Answer", "Response" or "Vendor response".
fn answer_column(headers: &[String], question: usize) -> Option<usize> {
    headers.iter().enumerate().position(|(i, h)| i != question && ["answer", "response", "reply", "comment"].iter().any(|w| h.to_lowercase().contains(w)))
}

/// A column that groups the questions, like "Section", "Category" or "Domain".
fn section_column(headers: &[String], question: usize) -> Option<usize> {
    headers.iter().enumerate().position(|(i, h)| i != question && ["section", "category", "domain", "area", "topic"].iter().any(|w| h.to_lowercase().contains(w)))
}

/// A table as it was uploaded, or pasted questions, one a line, as a one-column table.
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    pub fn pasted(text: &str) -> Self {
        Table { headers: vec!["Question".into()], rows: text.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| vec![l.to_string()]).collect() }
    }
}

/// Saves the questionnaire with every row, and an item for each row that has a question, and returns its id. Answering
/// it is the caller's.
pub async fn create(tx: &mut sqlx::PgConnection, project_id: Uuid, user_id: Uuid, name: &str, table: Table) -> AppResult<Uuid> {
    let q = question_column(&table.headers, &table.rows).ok_or_else(|| AppError::BadRequest("Couldn't find the questions in it".into()))?;
    let answers = answer_column(&table.headers, q);
    let sections = section_column(&table.headers, q);
    if table.rows.len() > MAX_ROWS {
        return Err(AppError::BadRequest(format!("It has {} rows. Split it into parts of up to {MAX_ROWS}", table.rows.len())));
    }
    let items: Vec<(usize, String, String)> = table
        .rows
        .iter()
        .enumerate()
        .filter_map(|(position, row)| {
            let question = row.get(q).map(|s| s.trim().to_string()).filter(|s| plain(s).len() > 3)?;
            let section = sections.and_then(|c| row.get(c)).map(|s| s.trim().to_string()).unwrap_or_default();
            Some((position, question, section))
        })
        .collect();
    if items.is_empty() {
        return Err(AppError::BadRequest("It has no questions".into()));
    }
    if items.len() > MAX_QUESTIONS {
        return Err(AppError::BadRequest(format!("It has {} questions. Split it into parts of up to {MAX_QUESTIONS}", items.len())));
    }
    let id: Uuid = sqlx::query_scalar(
        "insert into questionnaires (project_id, name, headers, rows, question_column, answer_column, created_by) values ($1, $2, $3, $4, $5, $6, $7) returning id",
    )
    .bind(project_id)
    .bind(name.trim())
    .bind(Json(&table.headers))
    .bind(Json(&table.rows))
    .bind(q as i32)
    .bind(answers.map(|c| c as i32))
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    for (position, question, section) in items {
        sqlx::query("insert into questionnaire_items (questionnaire_id, position, question, section) values ($1, $2, $3, $4)")
            .bind(id)
            .bind(position as i32)
            .bind(question.chars().take(2000).collect::<String>())
            .bind(section.chars().take(200).collect::<String>())
            .execute(&mut *tx)
            .await?;
    }
    Ok(id)
}

/// Has the agent answer what isn't answered or approved yet, or what it couldn't source when `retry`. Returns the run.
pub async fn answer(tx: &mut sqlx::PgConnection, id: Uuid, project_id: Uuid, org_id: Uuid, retry: bool) -> sqlx::Result<Uuid> {
    let run = crate::agent::enqueue(&mut *tx, crate::agent::kind::QUESTIONNAIRE, Some(project_id), Some(org_id), json!({ "questionnaire_id": id, "retry": retry })).await?;
    sqlx::query("update questionnaires set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

/// A questionnaire on the list, with how far along it is.
#[derive(Debug, Serialize, FromRow)]
pub struct Summary {
    pub id: Uuid,
    pub name: String,
    pub created_at: Ts,
    /// The agent is answering it.
    pub answering: bool,
    pub questions: i64,
    pub answered: i64,
    pub approved: i64,
    /// Answered with nothing to source it on, and not approved.
    pub unknown: i64,
}

const SUMMARY: &str = "select q.id, q.name, q.created_at, coalesce(r.status in ('queued', 'running'), false) as answering,
        count(i.*) as questions, count(i.*) filter (where i.answer <> '') as answered, count(i.*) filter (where i.approved_at is not null) as approved,
        count(i.*) filter (where i.confidence = 'unknown' and i.approved_at is null) as unknown
     from questionnaires q left join agent_runs r on r.id = q.run_id left join questionnaire_items i on i.questionnaire_id = q.id";

pub async fn list(db: &PgPool, project_id: Uuid) -> sqlx::Result<Vec<Summary>> {
    sqlx::query_as(&format!("{SUMMARY} where q.project_id = $1 group by q.id, r.status order by q.created_at desc")).bind(project_id).fetch_all(db).await
}

#[derive(Debug, Serialize, FromRow)]
pub struct Item {
    pub id: Uuid,
    /// Its row in the file, counting from 0 below the header.
    pub position: i32,
    /// Which question it is, from 1, skipping the rows that aren't questions.
    pub number: i32,
    pub question: String,
    pub section: String,
    pub answer: String,
    pub confidence: Option<String>,
    pub sources: Json<Vec<String>>,
    pub approved_at: Option<Ts>,
}

#[derive(Debug, Serialize)]
pub struct Detail {
    #[serde(flatten)]
    pub summary: Summary,
    pub items: Vec<Item>,
}

pub async fn detail(db: &PgPool, id: Uuid) -> sqlx::Result<Detail> {
    let summary = sqlx::query_as(&format!("{SUMMARY} where q.id = $1 group by q.id, r.status")).bind(id).fetch_one(db).await?;
    let items = sqlx::query_as(
        "select id, position, (row_number() over (order by position))::int as number, question, section, answer, confidence, sources, approved_at
         from questionnaire_items where questionnaire_id = $1 order by position",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    Ok(Detail { summary, items })
}

/// Saves an item's answer, and when `approve` approves it and files it in the project's library. An empty answer
/// can't be approved.
pub async fn save(db: &PgPool, item_id: Uuid, user_id: Uuid, answer: &str, approve: bool) -> AppResult<()> {
    let answer = answer.trim();
    if approve && answer.is_empty() {
        return Err(AppError::BadRequest("Write an answer before approving it".into()));
    }
    let mut tx = db.begin().await?;
    let (project_id, question): (Uuid, String) = sqlx::query_as(
        "update questionnaire_items i set answer = $2, approved_by = case when $3 then $4 end, approved_at = case when $3 then now() end
         from questionnaires q where q.id = i.questionnaire_id and i.id = $1 returning q.project_id, i.question",
    )
    .bind(item_id)
    .bind(answer)
    .bind(approve)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    if approve {
        file(&mut tx, project_id, &question, answer, user_id).await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn file(tx: &mut sqlx::PgConnection, project_id: Uuid, question: &str, answer: &str, user_id: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "insert into answer_library (project_id, key, question, answer, approved_by) values ($1, $2, $3, $4, $5)
         on conflict (project_id, key) do update set question = excluded.question, answer = excluded.answer, approved_by = excluded.approved_by, updated_at = now()",
    )
    .bind(project_id)
    .bind(key(question))
    .bind(question)
    .bind(answer)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Approves every answer that came from the library or was sourced, and files the sourced ones. Returns how many.
pub async fn approve_sourced(db: &PgPool, id: Uuid, project_id: Uuid, user_id: Uuid) -> sqlx::Result<u64> {
    let mut tx = db.begin().await?;
    let approved: Vec<(String, String)> = sqlx::query_as(
        "update questionnaire_items set approved_by = $2, approved_at = now()
         where questionnaire_id = $1 and approved_at is null and answer <> '' and confidence in ('library', 'sourced') returning question, answer",
    )
    .bind(id)
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await?;
    for (question, answer) in &approved {
        file(&mut tx, project_id, question, answer, user_id).await?;
    }
    tx.commit().await?;
    Ok(approved.len() as u64)
}

/// The file as it was uploaded, every row in place, with the answers filled in: in its answer column when it has one,
/// else in a new "Answer" column at the end.
pub async fn export(db: &PgPool, id: Uuid) -> sqlx::Result<(String, Vec<String>, Vec<Vec<String>>)> {
    #[derive(FromRow)]
    struct File {
        name: String,
        headers: Json<Vec<String>>,
        rows: Json<Vec<Vec<String>>>,
        answer_column: Option<i32>,
    }
    let File { name, headers: Json(mut headers), rows: Json(mut rows), answer_column } =
        sqlx::query_as("select name, headers, rows, answer_column from questionnaires where id = $1").bind(id).fetch_one(db).await?;
    let column = answer_column.map(|c| c as usize).unwrap_or_else(|| {
        headers.push("Answer".into());
        headers.len() - 1
    });
    for row in &mut rows {
        row.resize(row.len().max(headers.len()), String::new());
    }
    let answers: Vec<(i32, String)> =
        sqlx::query_as("select position, answer from questionnaire_items where questionnaire_id = $1").bind(id).fetch_all(db).await?;
    for (position, answer) in answers {
        if let Some(row) = rows.get_mut(position as usize) {
            row[column] = answer;
        }
    }
    Ok((name, headers, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn the_columns_are_found_by_name_or_by_the_longest_text() {
        let headers = s(&["ID", "Category", "Control question", "Vendor response"]);
        assert_eq!(question_column(&headers, &[]), Some(2));
        assert_eq!(answer_column(&headers, 2), Some(3));
        assert_eq!(section_column(&headers, 2), Some(1));
        let headers = s(&["#", "Text", "Notes"]);
        let rows = vec![s(&["1", "Do you encrypt customer data at rest and in transit?", ""]), s(&["2", "Is there SSO?", "x"])];
        assert_eq!(question_column(&headers, &rows), Some(1));
        assert_eq!(answer_column(&headers, 1), None, "a notes column stays as it is");
    }

    #[test]
    fn a_question_id_column_isnt_taken_for_the_questions() {
        let rows = vec![s(&["SEC-01", "Do you encrypt customer data at rest?", ""])];
        assert_eq!(question_column(&s(&["Question ID", "Question", "Answer"]), &rows), Some(1));
        assert_eq!(question_column(&s(&["Question ID", "Control question", "Answer"]), &rows), Some(1));
        assert_eq!(question_column(&s(&["Question ID", "Questions", "Answer"]), &[]), Some(1));
    }
}
