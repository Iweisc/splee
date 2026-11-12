//! Google Maps as a tool for finding companies, through the organization's own key. It runs in this process as an SDK MCP
//! server named `places`, so Claude sees `mcp__places__search`, and it runs the same search as the Companies page's Local
//! businesses tab, with its cache and monthly cap.

use claude_agent_sdk_rs::{McpServerConfig, McpToolResultContent, ToolResult, create_sdk_mcp_server, tool};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::local_businesses::{self, SearchQuery};
use crate::state::AppState;

pub const SEARCH: &str = "mcp__places__search";

/// Appended to the task prompt of calls that have the tool, like directory::GUIDANCE.
pub const GUIDANCE: &str = "\n\nYou can also search Google Maps with mcp__places__search. It spends this team's own Google Maps calls, so use it \
only when this segment's buyers are local businesses with a storefront or a service area, like dentists, gyms, restaurants, salons, \
clinics, trades or agencies in a city. Search the kind of business and a city or ZIP code, one search per place you need. It lists \
businesses with a website that this project doesn't have yet, with their category, address, rating and review count, and a place \
searched in the last 30 days costs nothing. For businesses from it, use their website's domain, and their Google Maps link as source_url.";

const DESCRIPTION: &str = "Search Google Maps for local businesses that have a website and aren't in this project yet, in Google's order. \
Each search reads up to 60 businesses.";

const MAX_LIMIT: usize = 60;

/// Offered when the organization has a key and calls left this month. Whether the segment's buyers are local businesses is
/// left to the model, which GUIDANCE tells when to use it.
pub async fn available(db: &PgPool, org_id: Uuid) -> sqlx::Result<bool> {
    let usage = local_businesses::usage(db, org_id).await?;
    Ok(usage.connected && usage.calls < i64::from(usage.cap))
}

pub fn server(state: AppState, org_id: Uuid, project_id: Uuid) -> McpServerConfig {
    let schema = json!({
        "type": "object",
        "properties": {
            "what": {"type": "string", "description": "The kind of business, like \"dentists\" or \"physical therapy clinic\"."},
            "where": {"type": "string", "description": "A city, area or ZIP code, like \"Austin, TX\"."},
            "min_rating": {"type": "number", "minimum": 0, "maximum": 5, "description": "Only businesses rated at least this."},
            "min_reviews": {"type": "integer", "minimum": 0, "description": "Only businesses with at least this many reviews."},
            "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": "Businesses to return. Defaults to 25."}
        },
        "required": ["what", "where"]
    });
    let search = tool!("search", DESCRIPTION, schema, move |args: Value| {
        let state = state.clone();
        async move {
            let (text, is_error) = match run(&state, org_id, project_id, &args).await {
                Ok(text) => (text, false),
                Err(e) => (e, true),
            };
            Ok(ToolResult { content: vec![McpToolResultContent::Text { text }], is_error })
        }
    });
    McpServerConfig::Sdk(create_sdk_mcp_server("places", "1.0.0", vec![search]))
}

/// One line per business, or why there are none.
async fn run(state: &AppState, org_id: Uuid, project_id: Uuid, args: &Value) -> Result<String, String> {
    let q = SearchQuery {
        what: args["what"].as_str().unwrap_or_default().into(),
        place: args["where"].as_str().unwrap_or_default().into(),
        min_rating: args["min_rating"].as_f64().map(|r| r as f32),
        min_reviews: args["min_reviews"].as_i64().map(|r| r as i32),
        with_website: true,
        project_id: Some(project_id),
    };
    let found = local_businesses::search(state, org_id, &q).await.map_err(|e| e.public_message())?;
    let limit = args["limit"].as_u64().map_or(25, |l| l as usize).clamp(1, MAX_LIMIT);
    let fresh: Vec<_> = found.items.iter().filter(|b| b.known.is_none() && b.domain.is_some()).collect();
    if fresh.is_empty() {
        return Ok(match found.note {
            Some(note) => note,
            None => "No new businesses matched. Try another wording, a nearby city or a lower rating.".into(),
        });
    }
    let mut lines: Vec<String> = fresh
        .iter()
        .take(limit)
        .map(|b| {
            let rating = b.rating.map(|r| format!(" · {r:.1} stars from {} reviews", b.review_count)).unwrap_or_default();
            let category = if b.category.is_empty() { "uncategorized" } else { &b.category };
            format!("- {} · {} · {category} · {}{rating} · {}", b.domain.as_deref().unwrap_or_default(), b.name, b.address, b.maps_url)
        })
        .collect();
    if fresh.len() > limit {
        lines.push(format!("These are the first {limit} of {}.", fresh.len()));
    }
    if let Some(note) = found.note {
        lines.push(note);
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use sqlx::PgPool;
    use uuid::Uuid;

    use crate::config::Config;
    use crate::places::fake::{self, place};
    use crate::state::AppState;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_tool_lists_new_businesses_with_a_website_once_a_key_is_connected(db: PgPool) {
        let (url, google) = fake::start().await;
        for (id, name, site) in [("a", "Smile Dental", "https://smile.example"), ("b", "Known Dental", "https://known.example"), ("c", "Walk-in Dental", "")] {
            google.places.lock().unwrap().push(place(id, name, site, 4.5, 30));
        }
        let base = crate::autopilot::tests::state(db.clone());
        let state = AppState { cfg: Arc::new(Config { places_api_url: url, ..(*base.cfg).clone() }), ..base };
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('T') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id) values ($1) returning id").bind(org).fetch_one(&db).await.unwrap();
        sqlx::query("insert into companies (project_id, domain) values ($1, 'known.example')").bind(project).execute(&db).await.unwrap();
        assert!(!super::available(&db, org).await.unwrap());

        sqlx::query("insert into places_keys (org_id, key_enc, monthly_cap) values ($1, $2, 1)").bind(org).bind(crate::crypto::encrypt("", "good")).execute(&db).await.unwrap();
        assert!(super::available(&db, org).await.unwrap());
        let found = super::run(&state, org, project, &json!({"what": "dentists", "where": "Austin, TX"})).await.unwrap();
        assert_eq!(found, "- smile.example · Smile Dental · Dentist · 100 Congress Ave, Austin, TX 78701, USA · 4.5 stars from 30 reviews · https://maps.google.com/?cid=a");
        assert!(!super::available(&db, org).await.unwrap(), "the month's one call is spent");
        let refused = super::run(&state, org, project, &json!({"what": "gyms", "where": "Austin, TX"})).await.unwrap_err();
        assert!(refused.starts_with("This month's Google Maps calls reached your cap of 1."), "{refused}");
    }
}
