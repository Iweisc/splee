//! Search over the company database that `splee import-companies` loads.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::util_pagination::PageQuery;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/directory/companies", get(search))
}

/// Counting every match of a broad search, like all restaurants in the US, is slow, so the count stops here.
const COUNT_CAP: i64 = 10_000;

const FILTER: &str = "from directory_companies
    where ($1::text is null or search @@ to_tsquery('simple', $1))
      and ($2::text is null or country_code = $2)
      and ($3::text is null or lower(city) = lower($3))
      and ($4::text is null or categories @> array[$4])
      and (not $5 or email is not null)
      and (not $6 or phone is not null)";

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
    country: Option<String>,
    city: Option<String>,
    category: Option<String>,
    #[serde(default)]
    has_email: bool,
    #[serde(default)]
    has_phone: bool,
}

#[derive(Serialize, sqlx::FromRow)]
struct Company {
    domain: String,
    name: String,
    website: String,
    phone: Option<String>,
    email: Option<String>,
    address: Option<String>,
    city: Option<String>,
    region: Option<String>,
    postcode: Option<String>,
    country_code: String,
    category: Option<String>,
}

#[derive(Serialize)]
struct Page {
    items: Vec<Company>,
    /// The number of matches, at most COUNT_CAP. `capped` says there are more.
    total: i64,
    capped: bool,
}

/// Companies whose name or category has words starting with each word in `q` and the filters, the most reliable listings first.
async fn search(State(state): State<AppState>, _auth: Auth, Query(q): Query<SearchQuery>, Query(page): Query<PageQuery>) -> AppResult<Json<Page>> {
    let blank = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let country = blank(q.country).map(|c| c.to_uppercase());
    if country.as_ref().is_some_and(|c| c.len() != 2 || !c.chars().all(|c| c.is_ascii_alphabetic())) {
        return Err(AppError::BadRequest("Pick a country by its two-letter code, like GB".into()));
    }
    let (words, city) = (q.q.as_deref().and_then(prefixes), blank(q.city));
    let category = blank(q.category).map(|c| c.to_lowercase().replace(' ', "_"));
    let total: i64 = sqlx::query_scalar(&format!("select count(*) from (select 1 {FILTER} limit {COUNT_CAP} + 1) matches"))
        .bind(&words)
        .bind(&country)
        .bind(&city)
        .bind(&category)
        .bind(q.has_email)
        .bind(q.has_phone)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, Company>(&format!(
        "select domain, name, website, phone, email, address, city, region, postcode, country_code, category {FILTER}
         order by confidence desc, domain limit $7 offset $8"
    ))
    .bind(&words)
    .bind(&country)
    .bind(&city)
    .bind(&category)
    .bind(q.has_email)
    .bind(q.has_phone)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Page { items, total: total.min(COUNT_CAP), capped: total > COUNT_CAP }))
}

/// Every word as a prefix, so "dent" finds dental_clinic and general_dentistry, as a tsquery. Only letters and digits get
/// through, so a query can't hold tsquery syntax.
fn prefixes(q: &str) -> Option<String> {
    let words: Vec<String> = q.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| format!("{}:*", w.to_lowercase())).collect();
    (!words.is_empty()).then(|| words.join(" & "))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_search_becomes_word_prefixes() {
        assert_eq!(super::prefixes("Dentist  London"), Some("dentist:* & london:*".into()));
        assert_eq!(super::prefixes("o'brien & sons | !x"), Some("o:* & brien:* & sons:* & x:*".into()));
        assert_eq!(super::prefixes(" &|! "), None);
    }
}
