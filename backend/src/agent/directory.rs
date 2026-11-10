//! The company database as a tool for finding companies. It runs in this process as an SDK MCP server named `directory`, so
//! Claude sees `mcp__directory__search`, and it runs the same search as the Companies page.

use claude_agent_sdk_rs::{McpServerConfig, McpToolResultContent, ToolResult, create_sdk_mcp_server, tool};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::directory::{self, Search};

pub const SEARCH: &str = "mcp__directory__search";

/// Appended to the task prompt of calls that have the tool, like x::GUIDANCE.
pub const GUIDANCE: &str = "\n\nYou can also search our company database with mcp__directory__search. It lists businesses that have their own \
website, from Overture Maps' open places data, with their category, city and whether they publish an email or phone number. \
Use it first when the segment is local or small businesses, like clinics, shops, trades or agencies in a region: search by the kind \
of business and a country or city, and try a few wordings and cities. Then search the web for what it lacks, like larger or \
online-only companies, or to check a fit you can't tell from the listing. For companies from the database, use their website as \
source_url.";

const DESCRIPTION: &str = "Search the company database: businesses with their own website, the most reliable listings first. Every word \
in `q` has to start a word in the business's name or category, so `dent` finds dental_clinic and general_dentistry. Companies already \
in this project are left out.";

const MAX_LIMIT: i64 = 50;

pub fn server(db: PgPool, project_id: Uuid) -> McpServerConfig {
    let schema = json!({
        "type": "object",
        "properties": {
            "q": {"type": "string", "description": "Words for the kind of business or its name, like \"dental\" or \"physio clinic\"."},
            "country": {"type": "string", "description": "ISO 3166-1 alpha-2 code, like GB."},
            "city": {"type": "string", "description": "The whole city name, like Leeds."},
            "category": {"type": "string", "description": "An exact category as the results show it, like dental_clinic, or a parent like health_care."},
            "has_email": {"type": "boolean", "description": "Only businesses that list an email address."},
            "has_phone": {"type": "boolean", "description": "Only businesses that list a phone number."},
            "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": "Companies to return. Defaults to 25."}
        }
    });
    let search = tool!("search", DESCRIPTION, schema, move |args: Value| {
        let db = db.clone();
        async move {
            Ok(match run(&db, project_id, &args).await {
                Ok(text) => result(text, false),
                Err(e) => result(e, true),
            })
        }
    });
    McpServerConfig::Sdk(create_sdk_mcp_server("directory", "1.0.0", vec![search]))
}

fn result(text: String, is_error: bool) -> ToolResult {
    ToolResult { content: vec![McpToolResultContent::Text { text }], is_error }
}

/// One line per company, or why there are none.
async fn run(db: &PgPool, project_id: Uuid, args: &Value) -> Result<String, String> {
    let text = |key: &str| args[key].as_str();
    let mut search = Search::new(
        text("q"),
        text("country"),
        text("city"),
        text("category"),
        args["has_email"].as_bool().unwrap_or(false),
        args["has_phone"].as_bool().unwrap_or(false),
    )?;
    search.skip_project = Some(project_id);
    let limit = args["limit"].as_i64().unwrap_or(25).clamp(1, MAX_LIMIT);
    let found = directory::search(db, &search, limit, 0).await.map_err(|e| format!("The company database failed: {e}"))?;
    if found.is_empty() {
        return Ok("No companies matched. Try fewer words, a parent category or a bigger city.".into());
    }
    let mut lines: Vec<String> = found
        .iter()
        .map(|c| {
            let place = [c.city.as_deref(), c.region.as_deref(), Some(c.country_code.as_str())].into_iter().flatten().collect::<Vec<_>>().join(", ");
            let contact = [c.email.as_ref().map(|_| "email"), c.phone.as_ref().map(|_| "phone")].into_iter().flatten().collect::<Vec<_>>();
            let contact = if contact.is_empty() { String::new() } else { format!(" · lists {}", contact.join(" and ")) };
            format!("- {} · {} · {} · {place}{contact}", c.domain, c.name, c.category.as_deref().unwrap_or("uncategorized"))
        })
        .collect();
    if found.len() as i64 == limit {
        lines.push(format!("These are the first {limit}. Narrow the search, or search another city, for more."));
    }
    Ok(lines.join("\n"))
}

/// Offering the tool with nothing behind it would only waste the model's turns.
pub async fn has_companies(db: &PgPool) -> sqlx::Result<bool> {
    sqlx::query_scalar("select exists (select 1 from directory_companies)").fetch_one(db).await
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::PgPool;
    use uuid::Uuid;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn search_lists_new_matches_for_the_project(db: PgPool) {
        assert!(!super::has_companies(&db).await.unwrap());
        for (domain, name, city, category, email) in [
            ("smile.co.uk", "Smile Dental", "Leeds", "dental_clinic", Some("hi@smile.co.uk")),
            ("known.co.uk", "Known Dental", "Leeds", "dental_clinic", None),
            ("york.co.uk", "York Dental", "York", "dental_clinic", None),
            ("bakery.co.uk", "Leeds Bakery", "Leeds", "bakery", None),
        ] {
            sqlx::query(
                "insert into directory_companies (domain, name, website, email, city, country_code, category, categories, source_id, release)
                 values ($1, $2, 'https://' || $1, $5, $3, 'GB', $4, array['health_care', $4], 'x', 'r')",
            )
            .bind(domain)
            .bind(name)
            .bind(city)
            .bind(category)
            .bind(email)
            .execute(&db)
            .await
            .unwrap();
        }
        let project: Uuid = sqlx::query_scalar("insert into projects default values returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into companies (project_id, domain) values ($1, 'Known.co.uk')").bind(project).execute(&db).await.unwrap();
        assert!(super::has_companies(&db).await.unwrap());

        let found = super::run(&db, project, &json!({"q": "dent", "country": "gb", "city": "leeds"})).await.unwrap();
        assert_eq!(found, "- smile.co.uk · Smile Dental · dental_clinic · Leeds, GB · lists email");
        let found = super::run(&db, project, &json!({"category": "health care", "limit": 1})).await.unwrap();
        assert!(found.starts_with("- ") && found.ends_with("These are the first 1. Narrow the search, or search another city, for more."), "{found}");
        let none = super::run(&db, project, &json!({"q": "dent", "has_phone": true})).await.unwrap();
        assert!(none.starts_with("No companies matched"));
        assert!(super::run(&db, project, &json!({"country": "Britain"})).await.is_err());
    }
}
