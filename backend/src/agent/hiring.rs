//! A company's open roles as a tool for prospecting runs. It runs in this process as an SDK MCP server named `hiring`, so
//! Claude sees `mcp__hiring__job_openings`, and it reads the same public job boards as the daily hiring signals.

use std::sync::Arc;

use claude_agent_sdk_rs::{McpServerConfig, McpToolResultContent, ToolResult, create_sdk_mcp_server, tool};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::jobs::{self, Boards};
use crate::routes::util_input;

pub const JOB_OPENINGS: &str = "mcp__hiring__job_openings";

/// Appended to the task prompt of calls that have the tool, like directory::GUIDANCE.
pub const GUIDANCE: &str = "\n\nYou can check what a company is hiring for with mcp__hiring__job_openings: give it the company's domain and it \
lists the open roles on its public job board (Greenhouse, Lever, Ashby, Workable or Recruitee). A company hiring for a role our \
offer helps with, or replaces, is a warm prospect, and the job post makes a specific, verifiable hook. Use it on the best \
candidates, not every one.";

const DESCRIPTION: &str = "List a company's open roles from its public job board, newest first: title, department, location, date \
posted and link. Says so when the company has no board we can read.";

/// Enough roles to see what a company hires for without filling the context with a big employer's whole board.
const MAX_ROLES: usize = 30;

pub fn server(db: PgPool, boards: Arc<Boards>) -> McpServerConfig {
    let schema = json!({
        "type": "object",
        "properties": {"domain": {"type": "string", "description": "The company's website domain, like acme.com."}},
        "required": ["domain"]
    });
    let openings = tool!("job_openings", DESCRIPTION, schema, move |args: Value| {
        let (db, boards) = (db.clone(), boards.clone());
        async move {
            Ok(match run(&db, &boards, &args).await {
                Ok(text) => result(text, false),
                Err(e) => result(e, true),
            })
        }
    });
    McpServerConfig::Sdk(create_sdk_mcp_server("hiring", "1.0.0", vec![openings]))
}

fn result(text: String, is_error: bool) -> ToolResult {
    ToolResult { content: vec![McpToolResultContent::Text { text }], is_error }
}

async fn run(db: &PgPool, boards: &Boards, args: &Value) -> Result<String, String> {
    let domain = util_input::domain(args["domain"].as_str().unwrap_or_default()).ok_or("Give a domain, like acme.com.")?;
    let found = jobs::openings(db, boards, &domain).await.map_err(|e| format!("The job board couldn't be read: {e:#}"))?;
    let Some((board, mut openings)) = found else {
        return Ok(format!("{domain} has no public job board we can read. Its careers page may still list roles."));
    };
    if openings.is_empty() {
        return Ok(format!("{domain} uses {} but has no open roles right now.", board.provider.name()));
    }
    openings.sort_by(|a, b| b.posted_at.cmp(&a.posted_at));
    let total = openings.len();
    let roles = if total == 1 { "role" } else { "roles" };
    let mut lines = vec![format!("{domain} has {total} open {roles} on {}:", board.provider.name())];
    lines.extend(openings.iter().take(MAX_ROLES).map(|o| {
        let about = [o.department.as_str(), o.location.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        let posted = o.posted_at.map(|t| format!(" · posted {}", t.format("%Y-%m-%d"))).unwrap_or_default();
        format!("- {}{}{posted} · {}", o.title, if about.is_empty() { String::new() } else { format!(" · {about}") }, o.url)
    }));
    if total > MAX_ROLES {
        lines.push(format!("…and {} more.", total - MAX_ROLES));
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::PgPool;

    use crate::jobs::tests::{greenhouse, serve};

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_tool_lists_roles_or_says_there_is_no_board(db: PgPool) {
        let (boards, fake) = serve().await;
        fake.board("greenhouse/acme", greenhouse(&[("SDR", "https://job-boards.greenhouse.io/acme/jobs/1")]));
        let listed = super::run(&db, &boards, &json!({"domain": "https://www.acme.com/about"})).await.unwrap();
        assert_eq!(
            listed,
            "acme.com has 1 open role on greenhouse:\n- SDR · Sales · Remote, US · posted 2026-10-22 · https://job-boards.greenhouse.io/acme/jobs/1"
        );
        let none = super::run(&db, &boards, &json!({"domain": "nobody.com"})).await.unwrap();
        assert!(none.starts_with("nobody.com has no public job board"), "{none}");
        assert!(super::run(&db, &boards, &json!({})).await.is_err());
    }
}
