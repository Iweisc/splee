use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::util_input;
use crate::error::AppResult;

const MAX_ERRORS: usize = 200;

/// A lead from a CSV row or the public API, before validation.
#[derive(Debug, Default, Deserialize)]
pub struct NewLead {
    pub email: String,
    #[serde(default)]
    pub full_name: String,
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub last_name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub company_name: String,
    #[serde(default)]
    pub company_domain: String,
    #[serde(default)]
    pub linkedin_url: String,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub country_code: String,
    pub segment_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct RowError {
    pub row: usize,
    pub error: String,
}

pub struct Outcome {
    pub imported: i64,
    pub skipped: i64,
    pub errors: Vec<RowError>,
}

#[derive(Default)]
struct Columns {
    email: Vec<String>,
    full_name: Vec<String>,
    first_name: Vec<String>,
    last_name: Vec<String>,
    title: Vec<String>,
    company_name: Vec<String>,
    company_domain: Vec<Option<String>>,
    linkedin_url: Vec<String>,
    city: Vec<String>,
    country_code: Vec<String>,
    segment_id: Vec<Option<Uuid>>,
}

/// Validates and inserts leads into a project. Invalid, suppressed and duplicate emails are skipped.
/// `rows` pairs each lead with the row number reported back in errors.
pub async fn insert(db: &PgPool, org_id: Uuid, project_id: Uuid, source: &str, rows: Vec<(usize, NewLead)>) -> AppResult<Outcome> {
    let suppressed: HashSet<String> =
        sqlx::query_scalar::<_, String>("select value::text from suppressions where org_id = $1").bind(org_id).fetch_all(db).await?.into_iter().collect();
    let mut seen: HashSet<String> = sqlx::query_scalar::<_, String>("select email::text from leads where project_id = $1 and email is not null")
        .bind(project_id)
        .fetch_all(db)
        .await?
        .into_iter()
        .collect();
    let segments: HashSet<Uuid> =
        sqlx::query_scalar::<_, Uuid>("select id from segments where project_id = $1").bind(project_id).fetch_all(db).await?.into_iter().collect();

    let total = rows.len() as i64;
    let mut errors = Vec::new();
    let mut cols = Columns::default();
    for (row, lead) in rows {
        let error = match util_input::email(&lead.email) {
            None if lead.email.trim().is_empty() => Err("Missing email".to_string()),
            None => Err(format!("Invalid email: {}", lead.email.trim())),
            Some(email) => {
                let domain = email.rsplit('@').next().unwrap_or_default();
                if suppressed.contains(&email) || suppressed.contains(domain) {
                    Err(format!("Suppressed: {email}"))
                } else if lead.segment_id.is_some_and(|s| !segments.contains(&s)) {
                    Err("Unknown segment".to_string())
                } else if !seen.insert(email.clone()) {
                    Err(format!("Duplicate: {email}"))
                } else {
                    Ok(email)
                }
            }
        };
        let email = match error {
            Ok(email) => email,
            Err(error) => {
                if errors.len() < MAX_ERRORS {
                    errors.push(RowError { row, error });
                }
                continue;
            }
        };

        let mut first = lead.first_name.trim().to_string();
        let mut last = lead.last_name.trim().to_string();
        let mut full = lead.full_name.trim().to_string();
        if full.is_empty() {
            full = format!("{first} {last}").trim().to_string();
        } else if first.is_empty() && last.is_empty() {
            let (f, l) = full.split_once(' ').unwrap_or((&full, ""));
            (first, last) = (f.to_string(), l.trim().to_string());
        }
        cols.email.push(email);
        cols.full_name.push(full);
        cols.first_name.push(first);
        cols.last_name.push(last);
        cols.title.push(lead.title.trim().to_string());
        cols.company_name.push(lead.company_name.trim().to_string());
        cols.company_domain.push(util_input::domain(&lead.company_domain));
        cols.linkedin_url.push(lead.linkedin_url.trim().to_string());
        cols.city.push(lead.city.trim().to_string());
        cols.country_code.push(country_code(&lead.country_code));
        cols.segment_id.push(lead.segment_id);
    }

    let imported = sqlx::query(
        "insert into leads (project_id, source, email, full_name, first_name, last_name, title, company_name,
           company_domain, linkedin_url, city, country_code, segment_id)
         select $1, $2, * from unnest($3::text[], $4::text[], $5::text[], $6::text[], $7::text[], $8::text[],
           $9::text[], $10::text[], $11::text[], $12::text[], $13::uuid[])
         on conflict (project_id, email) do nothing",
    )
    .bind(project_id)
    .bind(source)
    .bind(&cols.email)
    .bind(&cols.full_name)
    .bind(&cols.first_name)
    .bind(&cols.last_name)
    .bind(&cols.title)
    .bind(&cols.company_name)
    .bind(&cols.company_domain)
    .bind(&cols.linkedin_url)
    .bind(&cols.city)
    .bind(&cols.country_code)
    .bind(&cols.segment_id)
    .execute(db)
    .await?
    .rows_affected() as i64;
    Ok(Outcome { imported, skipped: total - imported, errors })
}

/// An ISO 3166-1 alpha-2 code like "GB", or "" for anything else, such as a country's full name, so imported leads
/// hold the same kind of code as the ones the agent finds.
fn country_code(raw: &str) -> String {
    let code = raw.trim().to_ascii_uppercase();
    if code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase()) { code } else { String::new() }
}

#[cfg(test)]
mod tests {
    use super::country_code;

    #[test]
    fn keeps_only_two_letter_country_codes() {
        assert_eq!(country_code(" gb "), "GB");
        assert_eq!(country_code("United Kingdom"), "");
        assert_eq!(country_code("U1"), "");
    }
}
