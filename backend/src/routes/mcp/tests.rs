use std::sync::Arc;

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::Config;
use crate::crypto::sha256_hex;
use crate::state::AppState;

const KEY: &str = "splee_test_key_a";

struct Server {
    url: String,
    client: reqwest::Client,
}

impl Server {
    async fn start(db: PgPool) -> Self {
        Self::with(db, String::new(), String::new()).await
    }

    /// With Google Places and the businesses' websites at fakes.
    async fn with(db: PgPool, places_api_url: String, local_sites_url: String) -> Self {
        let cfg = Config {
            database_url: String::new(),
            app_secret: String::new(),
            public_url: "http://localhost:3346".into(),
            unsubscribe_url: "http://localhost:3346".into(),
            bind_addr: String::new(),
            agent_model: String::new(),
            agent_concurrency: 0,
            sending_enabled: false,
            bird_bin: None,
            open_signup: true,
            public_agent: false,
            porkbun_api_url: String::new(),
            twilio_api_url: String::new(),
            bots_api_url: String::new(),
            outreach_api_url: String::new(),
            postmaster_api_url: String::new(),
            operator_emails: Vec::new(),
            google: None,
            microsoft: None,
            slack: None,
            tumblr: None,
            salesforce: None,
            zoho: None,
            meta: None,
            graph_api_url: String::new(),
            crm_api_url: String::new(),
            payments_api_url: String::new(),
            meetings_api_url: String::new(),
            aircall_api_url: String::new(),
            dialpad_api_url: String::new(),
            justcall_api_url: String::new(),
            webflow_api_url: String::new(),
            linkedin_ads: None,
            google_ads_developer_token: None,
            ads_api_url: String::new(),
            ipinfo_token: None,
            places_api_url,
            local_sites_url,
            sheets_api_url: String::new(),
            sec_user_agent: None,
            sec_api_url: String::new(),
            news_sites_url: String::new(),
            s3_api_url: String::new(),
        };
        let state = AppState { db, cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(16).0, rate_limits: Default::default() };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/mcp", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", super::router().merge(crate::routes::api_v1::router(&state))).with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self { url, client: reqwest::Client::new() }
    }

    async fn post(&self, key: &str, body: Value) -> (u16, Value) {
        let res = self.client.post(&self.url).bearer_auth(key).header("accept", "application/json, text/event-stream").json(&body);
        let res = res.send().await.unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    async fn call(&self, key: &str, tool: &str, arguments: Value) -> (bool, String) {
        let request = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": tool, "arguments": arguments } });
        let (status, body) = self.post(key, request).await;
        assert_eq!(status, 200, "{body}");
        let result = &body["result"];
        (result["isError"].as_bool().unwrap(), result["content"][0]["text"].as_str().unwrap().to_string())
    }
}

/// Two organizations, each with a project and campaign. Only the first has a key, KEY.
async fn seed(db: &PgPool) -> [(Uuid, Uuid); 2] {
    let mut out = vec![];
    for name in ["Acme", "Other"] {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id")
            .bind(org)
            .bind(format!("{name} project"))
            .fetch_one(db)
            .await
            .unwrap();
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Dentists') returning id")
            .bind(project)
            .fetch_one(db)
            .await
            .unwrap();
        out.push((project, segment));
        if name == "Acme" {
            sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'mcp', 'splee_test_', $2)")
                .bind(org)
                .bind(sha256_hex(KEY))
                .execute(db)
                .await
                .unwrap();
        }
    }
    [out[0], out[1]]
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn handshake_and_tool_list(db: PgPool) {
    seed(&db).await;
    let server = Server::start(db).await;

    let init = |version: &str| json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": version, "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } } });
    let (status, body) = server.post(KEY, init("2025-06-18")).await;
    assert_eq!(status, 200);
    assert_eq!(body["id"], 1);
    assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
    assert!(body["result"]["capabilities"]["tools"].is_object());
    assert_eq!(server.post(KEY, init("2099-01-01")).await.1["result"]["protocolVersion"], "2025-11-25");

    assert_eq!(server.post(KEY, json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await.0, 202);
    assert_eq!(server.post(KEY, json!({ "jsonrpc": "2.0", "id": "p", "method": "ping" })).await.1["result"], json!({}));
    assert_eq!(server.post(KEY, json!({ "jsonrpc": "2.0", "id": 2, "method": "nope" })).await.1["error"]["code"], -32601);

    let (_, body) = server.post(KEY, json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" })).await;
    let tools = body["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for name in ["list_projects", "create_campaign", "search_companies", "add_companies", "find_people", "write_emails", "get_run",
                 "list_leads", "list_companies", "list_emails_to_approve", "approve_email", "skip_email", "list_replies", "list_deals",
                 "create_deal", "move_deal", "get_analytics", "assign_lead", "list_call_tasks", "log_call", "list_meetings", "get_thread", "reply_to_lead",
                 "mark_replied", "search_local_businesses", "add_local_businesses", "list_sheet_connections", "import_from_sheet",
                 "list_job_changes", "add_to_watch_list", "set_lead_language", "set_campaign_language", "find_lookalikes",
                 "add_lookalikes", "list_company_signals", "add_research_question", "research_companies", "list_research", "get_campaign",
                 "start_campaign", "stop_campaign", "approve_campaign", "archive_campaign", "unarchive_campaign", "update_project_controls",
                 "list_hot_leads", "get_inbox_changes", "find_companies_by_domains", "get_monthly_cap",
                 "update_campaign",
                 "list_erased_leads",
                 "list_sending_domains", "suggest_sending_domains", "check_sending_domains",
                 "get_project", "create_project",
                 "list_tasks",
                 "get_campaign_send_state",
                 "find_phone",
                 "list_saved_searches", "save_search", "delete_saved_search", "list_new_matches", "find_email", "find_emails",
                 "get_email_batch", "download_companies", "fill_company_filters", "import_campaign", "get_campaign_import",
                 "list_campaign_import_rows", "get_my_copy", "update_my_copy", "set_my_copy_source", "preview_my_copy",
                 "get_project_my_copy", "update_project_my_copy", "list_company_dedup_lists", "create_company_dedup_list",
                 "get_company_dedup_list", "set_company_dedup_list_projects", "delete_company_dedup_list"] {
        assert!(names.contains(&name), "{name} missing");
    }
    assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object" && !t["description"].as_str().unwrap().is_empty()));

    // A version Splee doesn't know gets the list of ones it does, which tells a newer client to pick one rather than fall back.
    let request = json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list", "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2099-01-01" } } });
    let res = server.client.post(&server.url).bearer_auth(KEY).header("mcp-protocol-version", "2099-01-01").json(&request).send().await.unwrap();
    assert_eq!(res.status(), 400);
    let body: Value = res.json().await.unwrap();
    assert_eq!((body["id"].clone(), body["error"]["code"].clone()), (json!(4), json!(-32022)));
    assert_eq!(body["error"]["data"], json!({ "supported": ["2026-07-28", "2025-11-25", "2025-06-18"], "requested": "2099-01-01" }));
    assert_eq!(server.client.get(&server.url).bearer_auth(KEY).send().await.unwrap().status(), 405);
    let res = server.client.post(&server.url).bearer_auth(KEY).header("origin", "https://evil.example").json(&json!({})).send().await.unwrap();
    assert_eq!(res.status(), 403);
    // The public API's per-organization rate limit covers MCP too.
    let res = server.client.post(&server.url).bearer_auth(KEY).json(&json!({ "jsonrpc": "2.0", "id": 5, "method": "ping" })).send().await.unwrap();
    assert_eq!((res.headers()["ratelimit-limit"].to_str().unwrap(), res.headers().contains_key("ratelimit-remaining")), ("10000", true));
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn emails_are_found_by_name_for_the_keys_organization(db: PgPool) {
    let [(project, _), (other_project, _)] = seed(&db).await;
    let server = Server::start(db.clone()).await;
    let jane = json!({ "project_id": project, "first_name": "Jane", "last_name": "Doe", "company_domain": "acme.example" });

    let (is_error, text) = server.call(KEY, "find_email", jane.clone()).await;
    assert!(is_error && text.contains("email finder or verifier"), "{text}");
    for p in [project, other_project] {
        let connect = "insert into project_settings (project_id, verifier, verifier_key_enc) values ($1, 'millionverifier', '')";
        sqlx::query(connect).bind(p).execute(&db).await.unwrap();
    }
    sqlx::query("insert into email_search_attempts (org_id) select org_id from projects, generate_series(1, 200) where id = $1")
        .bind(project)
        .execute(&db)
        .await
        .unwrap();
    let (is_error, text) = server.call(KEY, "find_email", jane.clone()).await;
    assert!(is_error && text.contains("200 email searches"), "{text}");

    let people = json!([{ "first_name": "Jane", "company_domain": "acme.example", "client_ref": "crm-1" }]);
    let (is_error, text) = server.call(KEY, "find_emails", json!({ "project_id": project, "people": people })).await;
    assert!(!is_error, "{text}");
    let started: Value = serde_json::from_str(&text).unwrap();
    assert!(started["next"].as_str().unwrap().contains("get_email_batch"), "{started}");
    let (is_error, text) = server.call(KEY, "get_email_batch", json!({ "task_id": started["task_id"] })).await;
    let batch: Value = serde_json::from_str(&text).unwrap();
    assert!(!is_error && batch["status"] == "pending" && batch["results"][0]["client_ref"] == "crm-1", "{text}");

    let (is_error, text) = server.call(KEY, "find_emails", json!({ "project_id": other_project, "people": people })).await;
    assert!(is_error && text.contains("Not found"), "{text}");
    let (is_error, _) = server.call(KEY, "get_email_batch", json!({ "task_id": Uuid::new_v4() })).await;
    assert!(is_error);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn leads_and_companies_filter_and_sort_by_score(db: PgPool) {
    let [(project, segment), (other_project, _)] = seed(&db).await;
    let reasons = json!([{ "text": "Hiring 2 SDRs", "kind": "intent" }]);
    for (p, name, score, age) in [(project, "Ana", 40, 3), (project, "Bo", 90, 2), (project, "Cy", 10, 1), (other_project, "Di", 95, 0)] {
        sqlx::query(
            "insert into leads (project_id, full_name, email, score, score_fit, score_intent, score_reasons, created_at)
             values ($1, $2, $2 || '@x.com', $3, $3 / 2, $3 - $3 / 2, $4, now() - make_interval(days => $5))",
        )
        .bind(p)
        .bind(name)
        .bind(score as i16)
        .bind(&reasons)
        .bind(age)
        .execute(&db)
        .await
        .unwrap();
    }
    for (domain, score) in [("low.com", 5), ("high.com", 80)] {
        sqlx::query("insert into companies (project_id, segment_id, domain, score) values ($1, $2, $3, $4)")
            .bind(project)
            .bind(segment)
            .bind(domain)
            .bind(score as i16)
            .execute(&db)
            .await
            .unwrap();
    }
    let server = Server::start(db.clone()).await;
    let names = |text: &str, key: &str| -> Vec<String> {
        let page: Value = serde_json::from_str(text).unwrap();
        page["items"].as_array().unwrap().iter().map(|l| l[key].as_str().unwrap().to_string()).collect()
    };

    let (is_error, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "sort": "score", "min_score": 20 })).await;
    assert!(!is_error, "{text}");
    assert_eq!(names(&text, "full_name"), ["Bo", "Ana"]);
    let first: Value = serde_json::from_str::<Value>(&text).unwrap()["items"][0].clone();
    assert_eq!((first["score"].clone(), first["score_fit"].clone(), first["score_reasons"].clone()), (json!(90), json!(45), reasons.clone()));
    let (_, text) = server.call(KEY, "list_leads", json!({ "project_id": project })).await;
    assert_eq!(names(&text, "full_name"), ["Cy", "Bo", "Ana"], "newest first by default");
    let (is_error, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "min_score": 101 })).await;
    assert!(is_error && text.contains("0 to 100"), "{text}");
    let (is_error, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "sort": "vibes" })).await;
    assert!(is_error && text.contains("Sort by score"), "{text}");

    let (is_error, text) = server.call(KEY, "list_companies", json!({ "project_id": project, "sort": "score" })).await;
    assert!(!is_error, "{text}");
    assert_eq!(names(&text, "domain"), ["high.com", "low.com"]);
    let (_, text) = server.call(KEY, "list_companies", json!({ "project_id": project, "min_score": 50 })).await;
    assert_eq!(names(&text, "domain"), ["high.com"]);

    // The REST API takes the same filters, and another organization's project is not found.
    let api = server.url.replace("/mcp", "/v1");
    let get = |path: String| server.client.get(format!("{api}{path}")).bearer_auth(KEY).send();
    let res = get(format!("/projects/{project}/leads?min_score=50&sort=score")).await.unwrap();
    let page: Value = res.json().await.unwrap();
    assert_eq!((page["total"].clone(), page["items"][0]["full_name"].clone()), (json!(1), json!("Bo")));
    let res = get(format!("/projects/{project}/companies?sort=score&min_score=1")).await.unwrap();
    assert_eq!(res.json::<Value>().await.unwrap()["total"], 2);
    for path in [format!("/projects/{other_project}/leads?sort=score"), format!("/projects/{other_project}/companies")] {
        assert_eq!(get(path).await.unwrap().status(), 404);
    }
    let (is_error, _) = server.call(KEY, "list_leads", json!({ "project_id": other_project, "sort": "score" })).await;
    assert!(is_error);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn lookalikes_rank_and_join_a_campaign_for_the_keys_organization(db: PgPool) {
    let [(project, segment), (other_project, other_segment)] = seed(&db).await;
    let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(project).fetch_one(&db).await.unwrap();
    crate::lookalikes::tests::customers(&db, org, project).await;
    let server = Server::start(db.clone()).await;

    let (is_error, text) = server.call(KEY, "find_lookalikes", json!({ "project_id": project, "limit": 2 })).await;
    assert!(!is_error, "{text}");
    let found: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((found["total"].clone(), found["database_empty"].clone()), (json!(4), json!(false)));
    assert_eq!(found["results"].as_array().unwrap().len(), 2);
    assert_eq!(found["results"][0]["domain"], "ledger.example");
    assert_eq!(found["results"][0]["score"], 93);
    assert_eq!(found["results"][0]["reasons"][2], "Hiring a Head of Finance");
    assert_eq!(found["seeds"].as_array().unwrap().len(), 4);
    assert_eq!(found["profile"][0]["key"], "category");

    // Seeds passed in replace the project's, here and in the API's query string.
    let (_, text) = server.call(KEY, "find_lookalikes", json!({ "project_id": project, "seeds": ["beta.example"] })).await;
    let found: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(found["seeds"], json!([{ "domain": "beta.example", "name": "Beta", "won": true, "in_database": true, "known": true }]));
    let api = server.url.replace("/api/mcp", &format!("/api/v1/projects/{project}/lookalikes?seeds=beta.example,https://www.gamma.example/"));
    let res = server.client.get(&api).bearer_auth(KEY).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    let seeds: Vec<&str> = body["profile"]["seeds"].as_array().unwrap().iter().map(|s| s["domain"].as_str().unwrap()).collect();
    assert_eq!(seeds, ["beta.example", "gamma.example"]);
    let bad = server.url.replace("/api/mcp", &format!("/api/v1/projects/{project}/lookalikes?seeds=not%20a%20domain"));
    assert_eq!(server.client.get(&bad).bearer_auth(KEY).send().await.unwrap().status(), 400);

    let (is_error, text) = server.call(KEY, "add_lookalikes", json!({ "project_id": project, "campaign_id": segment, "top": 1 })).await;
    assert!(!is_error, "{text}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!({ "added": 1, "skipped": 0 }));
    let args = json!({ "project_id": project, "campaign_id": segment, "domains": ["tally.example", "noise.example"] });
    let (_, text) = server.call(KEY, "add_lookalikes", args).await;
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!({ "added": 1, "skipped": 1 }), "noise isn't a match");
    let (reason, segment_id): (String, Option<Uuid>) = sqlx::query_as("select fit_reason, segment_id from companies where domain = 'ledger.example' and project_id = $1")
        .bind(project)
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(reason.starts_with("Like your customers: Accounting firm"), "{reason}");
    assert_eq!(segment_id, Some(segment));

    // Another organization's project or campaign is out of reach.
    for (tool, args) in [
        ("find_lookalikes", json!({ "project_id": other_project })),
        ("add_lookalikes", json!({ "project_id": other_project, "campaign_id": other_segment, "top": 1 })),
        ("add_lookalikes", json!({ "project_id": project, "campaign_id": other_segment, "top": 1 })),
    ] {
        let (is_error, text) = server.call(KEY, tool, args).await;
        assert!(is_error, "{tool}: {text}");
    }
    let theirs: i64 = sqlx::query_scalar("select count(*) from companies where project_id = $1").bind(other_project).fetch_one(&db).await.unwrap();
    assert_eq!(theirs, 0);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn research_questions_queue_and_list_answers_by_label(db: PgPool) {
    let [(project, _), (other_project, _)] = seed(&db).await;
    let server = Server::start(db.clone()).await;
    sqlx::query("insert into companies (project_id, domain, name) values ($1, 'acme.example', 'Acme')").bind(project).execute(&db).await.unwrap();

    let (is_error, text) = server.call(KEY, "add_research_question", json!({ "project_id": project, "question": "Do they sell to hospitals?" })).await;
    assert!(!is_error, "{text}");
    let question: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((question["label"].clone(), question["answer_type"].clone()), (json!("Do they sell to hospitals"), json!("yes_no")));
    let (is_error, text) = server.call(KEY, "research_companies", json!({ "project_id": project })).await;
    assert!(!is_error, "{text}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["companies"], 1);

    sqlx::query("update research_answers set status = 'answered', answer = 'yes', evidence = 'Lists hospital clients.'").execute(&db).await.unwrap();
    let (is_error, text) = server.call(KEY, "list_research", json!({ "project_id": project, "question_id": question["id"], "answer": "yes" })).await;
    assert!(!is_error, "{text}");
    let listed: Value = serde_json::from_str(&text).unwrap();
    let qid = question["id"].as_str().unwrap();
    assert_eq!(
        listed["items"][0]["answers"][qid],
        json!({ "label": "Do they sell to hospitals", "status": "answered", "answer": "yes", "evidence": "Lists hospital clients.", "source_url": "" })
    );

    let (is_error, _) = server.call(KEY, "research_companies", json!({ "project_id": other_project })).await;
    assert!(is_error, "another organization's project");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn company_signals_for_the_keys_organization(db: PgPool) {
    let [(project, _), (other_project, _)] = seed(&db).await;
    let server = Server::start(db.clone()).await;
    for (p, domain, kind, key, age) in [
        (project, "acme.example", "funding_filing", "sec:1:021-1", 5),
        (project, "acme.example", "launch", "feed:a", 40),
        (other_project, "theirs.example", "funding_filing", "sec:2:021-2", 1),
    ] {
        sqlx::query(
            "insert into company_signals (project_id, domain, kind, source, key, happened_on, title, amount, url)
             values ($1, $2, $3, 'sec', $4, current_date - $5::int, 'Acme Robotics, Inc.', 12000000, 'https://www.sec.gov/' || $4)",
        )
        .bind(p)
        .bind(domain)
        .bind(kind)
        .bind(key)
        .bind(age)
        .execute(&db)
        .await
        .unwrap();
    }

    let (is_error, text) = server.call(KEY, "list_company_signals", json!({ "project_id": project, "days": 30 })).await;
    assert!(!is_error, "{text}");
    let listed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!((listed[0]["kind"].clone(), listed[0]["amount"].clone()), (json!("funding_filing"), json!(12000000)));
    assert!(listed[0]["headline"].as_str().unwrap().starts_with("Raised $12M (SEC filing, "), "{text}");
    assert!(listed[0].get("id").is_none(), "only the picked fields");
    let (_, text) = server.call(KEY, "list_company_signals", json!({ "project_id": project, "kind": "launch" })).await;
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()[0]["kind"], "launch");
    let (is_error, text) = server.call(KEY, "list_company_signals", json!({ "project_id": project, "kind": "gossip" })).await;
    assert!(is_error && text.contains("kind must be one of"), "{text}");

    let api = server.url.replace("/mcp", "/v1");
    let res = server.client.get(format!("{api}/projects/{project}/signals")).bearer_auth(KEY).send().await.unwrap();
    let all: Value = res.json().await.unwrap();
    assert_eq!(all.as_array().unwrap().len(), 2);
    let res = server.client.get(format!("{api}/projects/{other_project}/signals")).bearer_auth(KEY).send().await.unwrap();
    assert_eq!(res.status(), 404);
    let (is_error, _) = server.call(KEY, "list_company_signals", json!({ "project_id": other_project })).await;
    assert!(is_error);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn job_changes_and_the_watch_list_for_the_keys_organization(db: PgPool) {
    let [(project, _), (other_project, _)] = seed(&db).await;
    let server = Server::start(db.clone()).await;

    let people = json!([{ "name": "Dana Reyes", "email": "dana@acme.example", "linkedin_url": "https://www.linkedin.com/in/dana-reyes", "company": "Acme" },
                        { "name": "No Way" }]);
    let (is_error, text) = server.call(KEY, "add_to_watch_list", json!({ "project_id": project, "people": people })).await;
    assert!(!is_error, "{text}");
    let added: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((added["added"].clone(), added["errors"][0]["error"].clone()), (json!(1), json!("Needs an email or a LinkedIn link")));
    let watched: Uuid = sqlx::query_scalar("select id from watched_people where project_id = $1").bind(project).fetch_one(&db).await.unwrap();
    sqlx::query(
        "insert into job_changes (project_id, watched_id, old_company, new_company, new_domain, move_number, new_title)
         values ($1, $2, 'Acme', 'Brightline', 'brightline.example', 1, 'VP Sales')",
    )
    .bind(project)
    .bind(watched)
    .execute(&db)
    .await
    .unwrap();

    let (is_error, text) = server.call(KEY, "list_job_changes", json!({ "project_id": project })).await;
    assert!(!is_error, "{text}");
    let listed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((listed["connected"].clone(), listed["watching"].clone()), (json!(false), json!(1)));
    let change = &listed["items"][0];
    assert_eq!((change["full_name"].clone(), change["old_company"].clone(), change["new_company"].clone(), change["relationship"].clone()),
               (json!("Dana Reyes"), json!("Acme"), json!("Brightline"), json!("won")));
    assert!(change.get("old_lead_id").is_none(), "only the picked fields");

    // The REST API takes a CSV too, and another organization's project is not found either way.
    let api = server.url.replace("/mcp", "/v1");
    let csv = "Name,Email,LinkedIn,Company\nBo Chen,bo@bright.example,,Brightline\n";
    let res = server.client.post(format!("{api}/projects/{project}/watch-list")).bearer_auth(KEY).json(&json!({ "relationship": "booked", "csv": csv }));
    let added: Value = res.send().await.unwrap().json().await.unwrap();
    assert_eq!(added["added"], 1);
    let res = server.client.get(format!("{api}/projects/{project}/job-changes")).bearer_auth(KEY).send().await.unwrap();
    let overview: Value = res.json().await.unwrap();
    assert_eq!((overview["watching"].clone(), overview["changes"][0]["new_title"].clone()), (json!(2), json!("VP Sales")));
    for res in [
        server.client.get(format!("{api}/projects/{other_project}/job-changes")).bearer_auth(KEY).send().await.unwrap(),
        server.client.post(format!("{api}/projects/{other_project}/watch-list")).bearer_auth(KEY).json(&json!({ "csv": csv })).send().await.unwrap(),
    ] {
        assert_eq!(res.status(), 404);
    }
    let (is_error, _) = server.call(KEY, "list_job_changes", json!({ "project_id": other_project })).await;
    assert!(is_error);
    let theirs: i64 = sqlx::query_scalar("select count(*) from watched_people where project_id = $1").bind(other_project).fetch_one(&db).await.unwrap();
    assert_eq!(theirs, 0);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn leads_are_assigned_and_filtered_by_owner_for_the_keys_organization(db: PgPool) {
    let [(project, _), (other_project, _)] = seed(&db).await;
    let org_of = |p: Uuid| sqlx::query_scalar::<_, Uuid>("select org_id from projects where id = $1").bind(p).fetch_one(&db);
    let (acme, other) = (org_of(project).await.unwrap(), org_of(other_project).await.unwrap());
    let mut people = vec![];
    for (org, email) in [(acme, "ana@acme.test"), (acme, "ben@acme.test"), (other, "eve@other.test")] {
        let id: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ($1, 'x') returning id").bind(email).fetch_one(&db).await.unwrap();
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(org).bind(id).execute(&db).await.unwrap();
        people.push(id);
    }
    let lead = |project: Uuid, name: &'static str| {
        sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, full_name) values ($1, $2) returning id").bind(project).bind(name).fetch_one(&db)
    };
    let (cy, dee, theirs) = (lead(project, "Cy").await.unwrap(), lead(project, "Dee").await.unwrap(), lead(other_project, "Their Lead").await.unwrap());
    let server = Server::start(db.clone()).await;

    let (is_error, text) = server.call(KEY, "assign_lead", json!({ "lead_id": cy, "owner": "Ana@acme.test" })).await;
    assert!(!is_error, "{text}");
    let assigned: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((assigned["owner_id"].clone(), assigned["owner_email"].clone()), (json!(people[0]), json!("ana@acme.test")));
    let (is_error, text) = server.call(KEY, "assign_lead", json!({ "lead_id": dee, "owner": "eve@other.test" })).await;
    assert!(is_error && text.contains("Nobody on your team"), "{text}");
    let (is_error, _) = server.call(KEY, "assign_lead", json!({ "lead_id": dee, "owner": people[2] })).await;
    assert!(is_error, "Eve's id is no better than her email");
    let (is_error, _) = server.call(KEY, "assign_lead", json!({ "lead_id": theirs, "owner": "ana@acme.test" })).await;
    assert!(is_error, "the other organization's lead");

    let names = |text: &str| -> Vec<String> {
        let page: Value = serde_json::from_str(text).unwrap();
        page["items"].as_array().unwrap().iter().map(|l| l["full_name"].as_str().unwrap().to_string()).collect()
    };
    let (_, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "owner": "ana@acme.test" })).await;
    assert_eq!(names(&text), ["Cy"]);
    assert!(text.contains("\"owner_email\":\"ana@acme.test\""), "{text}");
    let (_, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "owner": "unassigned" })).await;
    assert_eq!(names(&text), ["Dee"]);
    let (is_error, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "owner": "mine" })).await;
    assert!(is_error && text.contains("API key"), "{text}");

    // The REST API: assign, take back with null, and filter by id.
    let api = server.url.replace("/mcp", "/v1");
    let res = server.client.post(format!("{api}/leads/{dee}/assign")).bearer_auth(KEY).json(&json!({ "owner": people[1] })).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.json::<Value>().await.unwrap()["owner_email"], "ben@acme.test");
    let page: Value = server.client.get(format!("{api}/projects/{project}/leads?owner={}", people[1])).bearer_auth(KEY).send().await.unwrap().json().await.unwrap();
    assert_eq!(page["items"][0]["full_name"], "Dee");
    let res = server.client.post(format!("{api}/leads/{cy}/assign")).bearer_auth(KEY).json(&json!({ "owner": null })).send().await.unwrap();
    assert_eq!(res.json::<Value>().await.unwrap()["owner_id"], Value::Null);
    let res = server.client.post(format!("{api}/leads/{theirs}/assign")).bearer_auth(KEY).json(&json!({ "owner": null })).send().await.unwrap();
    assert_eq!(res.status(), 404);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn language_on_leads_and_campaigns_through_the_api_and_mcp(db: PgPool) {
    let [(project, segment), (other_project, other_segment)] = seed(&db).await;
    let lead = |project: Uuid, email: &'static str, country: &'static str| {
        sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, full_name, email, country_code) values ($1, $2, $2, $3) returning id")
            .bind(project)
            .bind(email)
            .bind(country)
    };
    let anna = lead(project, "anna@brauhaus.example", "DE").fetch_one(&db).await.unwrap();
    let olaf = lead(other_project, "olaf@fjord.example", "NO").fetch_one(&db).await.unwrap();
    let server = Server::start(db.clone()).await;
    let api = server.url.trim_end_matches("/mcp").to_string();
    let send = |method: reqwest::Method, path: String, body: Value| {
        let request = server.client.request(method, format!("{api}{path}")).bearer_auth(KEY).json(&body);
        async move {
            let res = request.send().await.unwrap();
            (res.status().as_u16(), res.json::<Value>().await.unwrap_or(Value::Null))
        }
    };

    // API v1: a lead added with a language by name keeps its code, and the list filters by the language Splee writes them in.
    let (status, body) = send(reqwest::Method::POST, format!("/v1/projects/{project}/leads"), json!({ "leads": [{ "email": "camille@chocolat.example", "language": "French" }] })).await;
    assert_eq!((status, body["imported"].as_i64()), (200, Some(1)), "{body}");
    let (_, page) = send(reqwest::Method::GET, format!("/v1/projects/{project}/leads?language=fr"), Value::Null).await;
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{page}");
    assert_eq!((items[0]["language"].as_str(), items[0]["effective_language"].as_str(), items[0]["effective_language_source"].as_str()), (Some("fr"), Some("fr"), Some("set")));
    let (_, page) = send(reqwest::Method::GET, format!("/v1/projects/{project}/leads?language=de"), Value::Null).await;
    assert_eq!(page["items"][0]["id"], json!(anna), "{page}");
    assert_eq!(page["total"], 1);

    let (status, detail) = send(reqwest::Method::PATCH, format!("/v1/leads/{anna}"), json!({ "language": "es" })).await;
    assert_eq!((status, detail["language"].as_str(), detail["effective_language"].as_str()), (200, Some("es"), Some("es")), "{detail}");
    let (status, body) = send(reqwest::Method::PATCH, format!("/v1/leads/{anna}"), json!({ "language": "Klingon" })).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(send(reqwest::Method::PATCH, format!("/v1/leads/{olaf}"), json!({ "language": "de" })).await.0, 404);

    // New campaigns write in each lead's language unless told otherwise, like projects did before campaigns had a language.
    let (_, created) = send(reqwest::Method::POST, format!("/v1/projects/{project}/campaigns"), json!({ "prompt": "Breweries in Munich" })).await;
    let (is_error, text) = server.call(KEY, "create_campaign", json!({ "project_id": project, "prompt": "Bakeries in Lyon" })).await;
    assert!(!is_error, "{text}");
    let from_mcp = serde_json::from_str::<Value>(&text).unwrap()["campaign_id"].clone();
    let new: Vec<String> = sqlx::query_scalar("select language from segments where id::text = any($1) order by name")
        .bind(vec![created["campaign_id"].as_str().unwrap(), from_mcp.as_str().unwrap()])
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(new, ["lead", "lead"]);

    let (status, campaign) = send(reqwest::Method::PATCH, format!("/v1/campaigns/{segment}"), json!({ "language": "fr" })).await;
    assert_eq!((status, campaign["language"].as_str()), (200, Some("fr")), "{campaign}");
    assert_eq!(send(reqwest::Method::PATCH, format!("/v1/campaigns/{segment}"), json!({ "language": "xx" })).await.0, 400);
    assert_eq!(send(reqwest::Method::PATCH, format!("/v1/campaigns/{other_segment}"), json!({ "language": "de" })).await.0, 404);
    let (_, campaigns) = send(reqwest::Method::GET, format!("/v1/projects/{project}/campaigns"), Value::Null).await;
    let ours = campaigns.as_array().unwrap().iter().find(|c| c["id"] == json!(segment)).unwrap();
    assert_eq!(ours["language"], "fr");

    // MCP: clearing Anna's language takes her back to her country's, and the other organization's campaign is out of reach.
    let (is_error, text) = server.call(KEY, "set_lead_language", json!({ "lead_id": anna, "language": "" })).await;
    let set: Value = serde_json::from_str(&text).unwrap();
    assert!(!is_error, "{text}");
    assert_eq!((set["language"].as_str(), set["effective_language"].as_str(), set["effective_language_source"].as_str()), (Some(""), Some("de"), Some("country")));
    let (is_error, text) = server.call(KEY, "set_campaign_language", json!({ "campaign_id": segment, "language": "nl" })).await;
    assert!(!is_error && serde_json::from_str::<Value>(&text).unwrap()["language"] == "nl", "{text}");
    let (is_error, _) = server.call(KEY, "set_campaign_language", json!({ "campaign_id": other_segment, "language": "nl" })).await;
    assert!(is_error);
    let (is_error, text) = server.call(KEY, "update_campaign", json!({ "campaign_id": segment, "angle": "Fewer no-shows" })).await;
    let updated: Value = serde_json::from_str(&text).unwrap();
    assert!(!is_error && updated["angle"] == "Fewer no-shows" && updated["language"] == "nl", "{text}");
    let (is_error, _) = server.call(KEY, "set_lead_language", json!({ "lead_id": olaf, "language": "de" })).await;
    assert!(is_error);
    let (_, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "language": "de" })).await;
    let page: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((page["total"].as_i64(), page["items"][0]["effective_language"].as_str()), (Some(1), Some("de")), "{text}");
    let (_, text) = server.call(KEY, "list_campaigns", json!({ "project_id": project })).await;
    let campaigns: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(campaigns.as_array().unwrap().iter().find(|c| c["id"] == json!(segment)).unwrap()["language"], "nl", "{text}");
    let other: String = sqlx::query_scalar("select language from segments where id = $1").bind(other_segment).fetch_one(&db).await.unwrap();
    assert_eq!(other, "lead");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn downloading_companies_answers_the_first_rows_and_a_link_to_the_rest(db: PgPool) {
    seed(&db).await;
    for (domain, name, confidence) in [("smile.co.uk", "Smile Dental", 0.9), ("grin.co.uk", "=Grin Dental", 0.8), ("cafe.co.uk", "Leeds Cafe", 0.7)] {
        sqlx::query(
            "insert into directory_companies (domain, name, website, city, country_code, category, categories, confidence, source_id, release)
             values ($1, $2, 'https://' || $1, 'Leeds', 'GB', 'dental_clinic', '{dental_clinic}', $3, 'x', 'r')",
        )
        .bind(domain)
        .bind(name)
        .bind(confidence)
        .execute(&db)
        .await
        .unwrap();
    }
    let server = Server::start(db).await;

    let (is_error, text) = server.call(KEY, "download_companies", json!({ "q": "dental", "city": "Leeds", "has_email": false, "per_page": 1 })).await;
    assert!(!is_error, "{text}");
    let found: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((found["rows"].clone(), found["more"].clone()), (json!(1), json!(true)));
    assert_eq!(found["csv"].as_str().unwrap().lines().nth(1).unwrap(), "Smile Dental,smile.co.uk,https://smile.co.uk,,,,Leeds,,,GB,dental_clinic,0.90,x,r");
    assert_eq!(found["download_url"], "http://localhost:3346/api/v1/companies/export?q=dental&city=Leeds");

    let (_, text) = server.call(KEY, "download_companies", json!({ "domains": ["grin.co.uk", "cafe.co.uk"], "category": "dental_clinic" })).await;
    let found: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((found["rows"].clone(), found["more"].clone()), (json!(2), json!(false)));
    assert!(found["csv"].as_str().unwrap().contains("\n'=Grin Dental,grin.co.uk,"), "{text}");
    assert_eq!(found["download_url"], "http://localhost:3346/api/v1/companies/export?category=dental_clinic&domains=grin.co.uk%2Ccafe.co.uk");
    assert!(!super::read_tools().iter().any(|t| t["name"] == "download_companies"), "Ask Splee doesn't spend the downloads");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn dedup_lists_hide_companies_from_search_companies(db: PgPool) {
    let [(project, _), (other_project, _)] = seed(&db).await;
    for domain in ["smile.co.uk", "grin.co.uk"] {
        sqlx::query(
            "insert into directory_companies (domain, name, website, city, country_code, category, categories, source_id, release)
             values ($1, $1, 'https://' || $1, 'Leeds', 'GB', 'dental_clinic', '{dental_clinic}', 'x', 'r')",
        )
        .bind(domain)
        .execute(&db)
        .await
        .unwrap();
    }
    let server = &Server::start(db).await;
    let total = |args: Value| async move {
        let (is_error, text) = server.call(KEY, "search_companies", args).await;
        assert!(!is_error, "{text}");
        serde_json::from_str::<Value>(&text).unwrap()["total"].clone()
    };

    let args = json!({ "name": "CRM accounts", "domains": ["https://www.Smile.co.uk/"], "project_ids": [other_project] });
    assert!(server.call(KEY, "create_company_dedup_list", args).await.0, "another org's project");
    let args = json!({ "name": "CRM accounts", "domains": ["https://www.Smile.co.uk/"], "project_ids": [project] });
    let (is_error, text) = server.call(KEY, "create_company_dedup_list", args).await;
    assert!(!is_error, "{text}");
    let list_id = serde_json::from_str::<Value>(&text).unwrap()["list"]["id"].clone();
    assert_eq!(total(json!({ "category": "dental_clinic", "project_id": project })).await, json!(1));
    assert_eq!(total(json!({ "category": "dental_clinic", "project_id": project, "include_hidden": true })).await, json!(2));
    assert_eq!(total(json!({ "category": "dental_clinic" })).await, json!(2), "a list for one project");

    let (_, text) = server.call(KEY, "set_company_dedup_list_projects", json!({ "list_id": list_id, "project_ids": null })).await;
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["project_ids"], Value::Null, "{text}");
    assert_eq!(total(json!({ "category": "dental_clinic" })).await, json!(1));
    let (_, text) = server.call(KEY, "get_company_dedup_list", json!({ "list_id": list_id })).await;
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["domains"], json!(["smile.co.uk"]), "{text}");
    let (_, text) = server.call(KEY, "list_company_dedup_lists", json!({})).await;
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()[0]["size"], json!(1), "{text}");
    let (is_error, _) = server.call(KEY, "delete_company_dedup_list", json!({ "list_id": list_id })).await;
    assert!(!is_error);
    assert_eq!(total(json!({ "category": "dental_clinic" })).await, json!(2));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn tools_act_for_the_keys_organization_only(db: PgPool) {
    let [(project, segment), (other_project, other_segment)] = seed(&db).await;
    sqlx::query(
        "insert into directory_companies (domain, name, website, city, country_code, category, source_id, release)
         values ('smile.co.uk', 'Smile Dental', 'https://smile.co.uk', 'Leeds', 'GB', 'dental_clinic', 'x', 'r')",
    )
    .execute(&db)
    .await
    .unwrap();
    let other_email: Uuid = sqlx::query_scalar(
        "with l as (insert into leads (project_id, segment_id, full_name, email) values ($1, $2, 'Bo', 'bo@x.com') returning id)
         insert into email_messages (project_id, lead_id, segment_id, direction, kind, subject, body, status)
         select $1, l.id, $2, 'outbound', 'initial', 'Hi', 'Hello', 'draft' from l returning id",
    )
    .bind(other_project)
    .bind(other_segment)
    .fetch_one(&db)
    .await
    .unwrap();
    let server = Server::start(db.clone()).await;

    let (is_error, text) = server.call(KEY, "list_projects", json!({})).await;
    assert!(!is_error);
    assert!(text.contains("Acme project") && !text.contains("Other project"), "{text}");

    let (is_error, text) = server.call(KEY, "search_companies", json!({ "q": "dental", "city": "leeds" })).await;
    assert!(!is_error && text.contains("smile.co.uk"), "{text}");
    // A scanned company carries its tools, and the tech filter takes a tool or a category.
    sqlx::query(
        "insert into directory_companies (domain, name, website, city, country_code, category, source_id, release)
         values ('grin.co.uk', 'Grin Dental', 'https://grin.co.uk', 'Leeds', 'GB', 'dental_clinic', 'y', 'r')",
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::query("insert into tech_scans (domain, techs, scanned_at) values ('smile.co.uk', '{HubSpot,Cloudflare}', now())").execute(&db).await.unwrap();
    for filter in ["crm", "hubspot"] {
        let (is_error, text) = server.call(KEY, "search_companies", json!({ "q": "dental", "tech": filter })).await;
        let found: Value = serde_json::from_str(&text).unwrap();
        assert!(!is_error, "{text}");
        assert_eq!((found["total"].clone(), found["items"][0]["tech"].clone()), (json!(1), json!(["HubSpot", "Cloudflare"])), "{filter}");
    }
    let (_, text) = server.call(KEY, "search_companies", json!({ "q": "dental", "tech": "Stripe" })).await;
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["total"], 0);
    let (is_error, text) = server.call(KEY, "search_companies", json!({ "q": "dental", "tech": "Nope" })).await;
    assert!(is_error && text.contains("can't detect"), "{text}");
    let (is_error, text) = server.call(KEY, "add_companies", json!({ "campaign_id": segment, "domains": ["smile.co.uk"] })).await;
    assert!(!is_error, "{text}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["added"], 1);

    let (is_error, text) = server.call(KEY, "create_campaign", json!({ "project_id": project, "prompt": "Dentists in Leeds" })).await;
    assert!(!is_error, "{text}");
    let run_id = serde_json::from_str::<Value>(&text).unwrap()["run_id"].as_str().unwrap().to_string();
    let (_, text) = server.call(KEY, "get_run", json!({ "run_id": run_id })).await;
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["status"], "queued");

    let (is_error, text) = server.call(KEY, "list_campaigns", json!({ "project_id": project })).await;
    assert!(!is_error && text.contains("New campaign"), "{text}");

    // Ids of another organization look like ids that don't exist.
    for (tool, args) in [
        ("list_campaigns", json!({ "project_id": other_project })),
        ("add_companies", json!({ "campaign_id": other_segment, "domains": ["smile.co.uk"] })),
        ("find_people", json!({ "campaign_id": other_segment })),
        ("approve_email", json!({ "email_id": other_email })),
    ] {
        assert_eq!(server.call(KEY, tool, args).await, (true, "Not found".into()), "{tool}");
    }
    let status: String = sqlx::query_scalar("select status from email_messages where id = $1").bind(other_email).fetch_one(&db).await.unwrap();
    assert_eq!(status, "draft");
    let (_, text) = server.call(KEY, "list_emails_to_approve", json!({})).await;
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["total"], 0);

    let (is_error, text) = server.call(KEY, "list_campaigns", json!({})).await;
    assert_eq!((is_error, text.as_str()), (true, "Pass project_id as an id"));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn deals_are_listed_created_and_moved_for_the_keys_organization(db: PgPool) {
    let [(project, segment), (other_project, _)] = seed(&db).await;
    let lead = |project: Uuid, name: &str| {
        sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, segment_id, full_name, email) values ($1, $2, $3, $4) returning id")
            .bind(project)
            .bind(Some(segment).filter(|_| project != other_project))
            .bind(name.to_string())
            .bind(format!("{name}@x.example"))
            .fetch_one(&db)
    };
    let (sarah, bo) = (lead(project, "sarah").await.unwrap(), lead(other_project, "bo").await.unwrap());
    let other_org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(other_project).fetch_one(&db).await.unwrap();
    crate::deals::ensure_stages(&db, other_org).await.unwrap();
    let their_deal: Uuid = sqlx::query_scalar(
        "insert into deals (org_id, project_id, lead_id, stage_id) select $1, $2, $3, id from deal_stages where org_id = $1 and name = 'Proposal' returning id",
    )
    .bind(other_org)
    .bind(other_project)
    .bind(bo)
    .fetch_one(&db)
    .await
    .unwrap();
    let server = Server::start(db.clone()).await;

    let (is_error, text) = server.call(KEY, "create_deal", json!({ "lead_id": sarah, "value": 12000, "close_date": "2026-12-01" })).await;
    assert!(!is_error, "{text}");
    let deal: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((deal["stage"].clone(), deal["value"].clone(), deal["currency"].clone()), (json!("Interested"), json!(12000.0), json!("USD")));
    let (is_error, text) = server.call(KEY, "create_deal", json!({ "lead_id": sarah })).await;
    assert!(is_error && text.contains("already has an open deal"), "{text}");

    let (is_error, text) = server.call(KEY, "move_deal", json!({ "deal_id": deal["id"], "stage": "won", "value": 15000 })).await;
    assert!(!is_error, "{text}");
    let won: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((won["status"].clone(), won["value"].clone()), (json!("won"), json!(15000.0)));

    let (_, text) = server.call(KEY, "list_deals", json!({ "status": "won" })).await;
    let listed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((listed["total"].clone(), listed["items"][0]["lead_name"].clone()), (json!(1), json!("sarah")));
    let stages: Vec<&str> = listed["stages"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(stages, ["Interested", "Meeting booked", "Proposal", "Negotiation", "Won", "Lost"]);
    let (_, text) = server.call(KEY, "list_deals", json!({ "status": "open" })).await;
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["total"], 0);

    // The other organization's deal, lead and project look like ids that don't exist.
    for (tool, args) in [
        ("move_deal", json!({ "deal_id": their_deal, "stage": "Won" })),
        ("create_deal", json!({ "lead_id": bo })),
        ("list_deals", json!({ "project_id": other_project })),
    ] {
        assert_eq!(server.call(KEY, tool, args).await, (true, "Not found".into()), "{tool}");
    }
    let (stage, status): (String, String) =
        sqlx::query_as("select s.name, d.status from deals d join deal_stages s on s.id = d.stage_id where d.id = $1").bind(their_deal).fetch_one(&db).await.unwrap();
    assert_eq!((stage.as_str(), status.as_str()), ("Proposal", "open"));

    // The same through the REST API.
    let api = server.url.replace("/api/mcp", "/api/v1");
    let res = server.client.get(format!("{api}/deals?project_id={project}")).bearer_auth(KEY).send().await.unwrap();
    let page: Value = res.json().await.unwrap();
    assert_eq!((page["total"].clone(), page["items"][0]["stage"].clone()), (json!(1), json!("Won")));
    let res = server.client.post(format!("{api}/deals/{}/move", deal["id"].as_str().unwrap())).bearer_auth(KEY);
    let moved: Value = res.json(&json!({ "stage": "Lost", "lost_reason": "No budget" })).send().await.unwrap().json().await.unwrap();
    assert_eq!((moved["status"].clone(), moved["lost_reason"].clone()), (json!("lost"), json!("No budget")));
    let res = server.client.get(format!("{api}/deal-stages")).bearer_auth(KEY).send().await.unwrap();
    assert_eq!(res.json::<Value>().await.unwrap().as_array().unwrap().len(), 6);
    let res = server.client.get(format!("{api}/deals")).bearer_auth("splee_wrong").send().await.unwrap();
    assert_eq!(res.status(), 401);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_wrong_key_is_refused(db: PgPool) {
    seed(&db).await;
    let server = Server::start(db).await;
    let (status, body) = server.post("splee_wrong", json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await;
    assert_eq!(status, 401);
    assert!(body["error"]["message"].as_str().unwrap().contains("API key"));
    let res = server.client.post(&server.url).json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).send().await.unwrap();
    assert_eq!(res.status(), 401);
    assert_eq!(res.headers()["www-authenticate"], "Bearer resource_metadata=\"http://localhost:3346/.well-known/oauth-protected-resource/api/mcp\"");
    let res = server.client.post(&server.url).bearer_auth("splee_wrong").json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).send().await.unwrap();
    assert!(res.headers()["www-authenticate"].to_str().unwrap().starts_with("Bearer error=\"invalid_token\""));
}

/// A 2026-07-28 request with the headers that mirror its body.
async fn stateless(server: &Server, method: &str, name: Option<&str>, mut params: Value) -> (u16, Value) {
    params["_meta"] = json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {} });
    let body = json!({ "jsonrpc": "2.0", "id": 9, "method": method, "params": params });
    let mut req = server.client.post(&server.url).bearer_auth(KEY).header("mcp-protocol-version", "2026-07-28").header("mcp-method", method);
    if let Some(name) = name {
        req = req.header("mcp-name", name);
    }
    let res = req.json(&body).send().await.unwrap();
    (res.status().as_u16(), res.json().await.unwrap_or(Value::Null))
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_stateless_revision_works_without_initialize(db: PgPool) {
    seed(&db).await;
    let server = Server::start(db).await;

    let (status, body) = stateless(&server, "server/discover", None, json!({})).await;
    assert_eq!(status, 200, "{body}");
    let result = &body["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["supportedVersions"][0], "2026-07-28");
    assert!(result["capabilities"]["tools"].is_object() && result["instructions"].is_string());
    assert_eq!(result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "splee");

    let (_, body) = stateless(&server, "tools/list", None, json!({})).await;
    assert!(body["result"]["tools"].as_array().unwrap().len() >= 12);
    assert_eq!((body["result"]["cacheScope"].as_str(), body["result"]["ttlMs"].is_u64()), (Some("private"), true));

    let (status, body) = stateless(&server, "tools/call", Some("list_projects"), json!({ "name": "list_projects", "arguments": {} })).await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["resultType"], "complete");
    assert!(body["result"]["content"][0]["text"].as_str().unwrap().contains("Acme project"));
    // Mcp-Name can carry a name in base64.
    let encoded = "=?base64?bGlzdF9wcm9qZWN0cw==?=";
    let (status, _) = stateless(&server, "tools/call", Some(encoded), json!({ "name": "list_projects", "arguments": {} })).await;
    assert_eq!(status, 200);

    // The headers have to match the body.
    let (status, body) = stateless(&server, "tools/call", Some("list_leads"), json!({ "name": "list_projects", "arguments": {} })).await;
    assert_eq!((status, body["error"]["code"].clone()), (400, json!(-32020)));
    let (status, body) = stateless(&server, "tools/call", None, json!({ "name": "list_projects", "arguments": {} })).await;
    assert_eq!((status, body["error"]["code"].clone()), (400, json!(-32020)));
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list",
        "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {} } } });
    let res = server.client.post(&server.url).bearer_auth(KEY).header("mcp-protocol-version", "2026-07-28").json(&request).send().await.unwrap();
    assert_eq!(res.status(), 400);
    assert_eq!(res.json::<Value>().await.unwrap()["error"]["code"], -32020);

    // Every request has to say what the client can do, and the handshake's methods are gone.
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" } } });
    let res = server.client.post(&server.url).bearer_auth(KEY).header("mcp-protocol-version", "2026-07-28").header("mcp-method", "tools/list");
    let res = res.json(&request).send().await.unwrap();
    assert_eq!((res.status().as_u16(), res.json::<Value>().await.unwrap()["error"]["code"].clone()), (400, json!(-32602)));
    let (status, body) = stateless(&server, "ping", None, json!({})).await;
    assert_eq!((status, body["error"]["code"].clone()), (404, json!(-32601)));

    // initialize still starts a legacy session, whatever header comes with it.
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2026-07-28", "capabilities": {} } });
    let (status, body) = server.post(KEY, init).await;
    assert_eq!((status, body["result"]["protocolVersion"].as_str()), (200, Some("2025-11-25")));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn local_business_tools_and_api_use_the_keys_organization_only(db: PgPool) {
    let [(project, segment), (other_project, other_segment)] = seed(&db).await;
    let (places_url, google) = crate::places::fake::start().await;
    let (sites_url, sites) = crate::site_contacts::fake::start().await;
    google.places.lock().unwrap().push(crate::places::fake::place("smile", "Smile Dental", "https://smile.example", 4.8, 90));
    sites.page("smile.example", "/", "Ask for Ana: ana.ruiz@smile.example");
    let server = Server::with(db.clone(), places_url, sites_url).await;

    let (is_error, text) = server.call(KEY, "search_local_businesses", json!({ "what": "dentists", "where": "Austin, TX" })).await;
    assert_eq!((is_error, text.as_str()), (true, "Connect a Google Maps key under Integrations first"));
    sqlx::query("insert into places_keys (org_id, key_enc) select org_id, $1 from projects where id = $2")
        .bind(crate::crypto::encrypt("", "good"))
        .bind(project)
        .execute(&db)
        .await
        .unwrap();

    let (is_error, text) = server.call(KEY, "search_local_businesses", json!({ "what": "dentists", "where": "Austin, TX", "project_id": project })).await;
    assert!(!is_error, "{text}");
    let found: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((found["items"][0]["place_id"].as_str(), found["items"][0]["known"].clone()), (Some("smile"), Value::Null));
    assert!(found["items"][0].get("lat").is_none(), "the tool keeps its answer short");
    let api = server.url.replace("/mcp", "/v1");
    let res = server.client.get(format!("{api}/local-businesses?what=dentists&where=Austin,%20TX")).bearer_auth(KEY).send().await.unwrap();
    let body: Value = res.json().await.unwrap();
    assert_eq!((body["items"][0]["lat"].as_f64(), body["calls"].as_i64()), (Some(30.2672), Some(0)), "the API answers in full, from the cache");
    assert_eq!(google.calls(), 1);

    for (tool, args) in [
        ("search_local_businesses", json!({ "what": "dentists", "where": "Austin", "project_id": other_project })),
        ("add_local_businesses", json!({ "campaign_id": other_segment, "place_ids": ["smile"] })),
    ] {
        assert_eq!(server.call(KEY, tool, args).await, (true, "Not found".into()), "{tool}");
    }
    let res = server.client.post(format!("{api}/segments/{segment}/local-businesses")).bearer_auth(KEY).json(&json!({ "place_ids": ["smile"] }));
    let added: Value = res.send().await.unwrap().json().await.unwrap();
    assert_eq!((added["added"][0]["email"].as_str(), added["added"][0]["contact_name"].as_str()), (Some("ana.ruiz@smile.example"), Some("Ana Ruiz")));
    let (_, text) = server.call(KEY, "add_local_businesses", json!({ "campaign_id": segment, "place_ids": ["smile"] })).await;
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["skipped"][0]["reason"], "Already in this project");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn campaigns_run_and_replies_follow_through_mcp(db: PgPool) {
    let [(project, segment), (other_project, other_segment)] = seed(&db).await;
    let lead: Uuid = sqlx::query_scalar(
        "with l as (insert into leads (project_id, segment_id, full_name, email) values ($1, $2, 'Ana', 'ana@smile.example') returning id),
         e as (insert into email_messages (project_id, lead_id, segment_id, direction, kind, subject, body, status)
               select $1, l.id, $2, 'outbound', 'initial', 'Hi', 'Hello', 'draft' from l)
         select id from l",
    )
    .bind(project)
    .bind(segment)
    .fetch_one(&db)
    .await
    .unwrap();
    sqlx::query(
        "insert into email_messages (project_id, lead_id, direction, kind, body, status, classification, classification_reason, created_at)
         values ($1, $2, 'inbound', 'inbound', 'Can we talk Tuesday?', 'received', 'hot', 'Wants a call', now() - interval '1 minute')",
    )
    .bind(project)
    .bind(lead)
    .execute(&db)
    .await
    .unwrap();
    let server = Server::start(db.clone()).await;
    let status = |id: Uuid| {
        let db = db.clone();
        async move { sqlx::query_scalar::<_, String>("select status from segments where id = $1").bind(id).fetch_one(&db).await.unwrap() }
    };
    let parsed = |text: &str| serde_json::from_str::<Value>(text).unwrap();

    let (is_error, text) = server.call(KEY, "get_campaign", json!({ "campaign_id": segment })).await;
    assert!(!is_error && parsed(&text)["status"] == "draft" && parsed(&text)["schedule"].is_object(), "{text}");
    let (is_error, text) = server.call(KEY, "approve_campaign", json!({ "campaign_id": segment })).await;
    assert_eq!((is_error, parsed(&text)), (false, json!({ "accepted": true, "approved": 1 })), "{text}");
    assert_eq!(server.call(KEY, "start_campaign", json!({ "campaign_id": segment })).await, (false, r#"{"accepted":true}"#.into()));
    assert_eq!(status(segment).await, "working");
    assert_eq!(server.call(KEY, "stop_campaign", json!({ "campaign_id": segment })).await, (false, r#"{"accepted":true}"#.into()));
    assert_eq!(status(segment).await, "paused");
    let audited: i64 = sqlx::query_scalar("select count(*) from audit_events where action in ('campaign.launched', 'campaign.paused') and actor_key_id is not null")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(audited, 2, "the audit log names the key");

    // Archiving takes it off list_campaigns until it's unarchived, which leaves it paused.
    let srv = &server;
    let listed = move |archived: bool| async move {
        let (_, text) = srv.call(KEY, "list_campaigns", json!({ "project_id": project, "archived": archived })).await;
        serde_json::from_str::<Value>(&text).unwrap().as_array().unwrap().iter().map(|c| c["id"].clone()).collect::<Vec<_>>()
    };
    for _ in 0..2 {
        let (is_error, text) = server.call(KEY, "archive_campaign", json!({ "campaign_id": segment })).await;
        assert!(!is_error && parsed(&text)["archived_at"].is_string(), "{text}");
    }
    assert_eq!((listed(false).await, listed(true).await), (vec![], vec![json!(segment)]));
    let (is_error, text) = server.call(KEY, "unarchive_campaign", json!({ "campaign_id": segment })).await;
    assert!(!is_error && parsed(&text)["archived_at"].is_null() && parsed(&text)["status"] == "paused", "{text}");
    assert_eq!(listed(false).await, vec![json!(segment)]);

    let (is_error, text) = server.call(KEY, "update_project_controls", json!({ "project_id": project, "daily_email_limit": 70, "review_window_hours": 12 })).await;
    assert!(!is_error && parsed(&text)["daily_email_limit"] == 70 && parsed(&text)["review_window_hours"] == 12, "{text}");
    let (is_error, text) = server.call(KEY, "update_project_controls", json!({ "project_id": project, "budget": 5 })).await;
    assert!(is_error && text.contains("unknown field `budget`"), "{text}");

    let (is_error, text) = server.call(KEY, "list_hot_leads", json!({})).await;
    let hot = parsed(&text);
    assert!(!is_error, "{text}");
    assert_eq!((hot["total"].clone(), hot["items"][0]["lead_id"].clone(), hot["items"][0]["reply"].clone()), (json!(1), json!(lead), json!("Can we talk Tuesday?")));
    let (is_error, text) = server.call(KEY, "get_inbox_changes", json!({ "project_id": project })).await;
    let changes = parsed(&text);
    assert!(!is_error && changes["items"].as_array().unwrap().len() == 1 && changes["has_more"] == false, "{text}");
    let (_, text) = server.call(KEY, "get_inbox_changes", json!({ "project_id": project, "cursor": changes["next_cursor"] })).await;
    assert_eq!(parsed(&text)["items"], json!([]));

    // Another organization's ids look like ids that don't exist.
    for (tool, args) in [
        ("get_campaign", json!({ "campaign_id": other_segment })),
        ("start_campaign", json!({ "campaign_id": other_segment })),
        ("stop_campaign", json!({ "campaign_id": other_segment })),
        ("approve_campaign", json!({ "campaign_id": other_segment })),
        ("archive_campaign", json!({ "campaign_id": other_segment })),
        ("unarchive_campaign", json!({ "campaign_id": other_segment })),
        ("update_project_controls", json!({ "project_id": other_project, "daily_email_limit": 70 })),
        ("get_inbox_changes", json!({ "project_id": other_project })),
        ("list_hot_leads", json!({ "project_id": other_project })),
    ] {
        assert_eq!(server.call(KEY, tool, args).await, (true, "Not found".into()), "{tool}");
    }
    assert_eq!(status(other_segment).await, "draft");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn list_tasks_filters_the_keys_organizations_runs_by_kind(db: PgPool) {
    let [(project, _), (other_project, _)] = seed(&db).await;
    for (project, kind) in [(project, "find_people"), (project, "write_emails"), (other_project, "find_people")] {
        sqlx::query("insert into agent_runs (project_id, kind) values ($1, $2)").bind(project).bind(kind).execute(&db).await.unwrap();
    }
    let server = Server::start(db).await;
    let (error, text) = server.call(KEY, "list_tasks", json!({ "type": "run", "kind": "find_people", "per_page": 5 })).await;
    assert!(!error, "{text}");
    let page: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((&page["total"], &page["items"][0]["project_id"], &page["items"][0]["status"]), (&json!(1), &json!(project), &json!("pending")));
    assert!(server.call(KEY, "list_tasks", json!({ "status": "done" })).await.0);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn replies_and_new_replies_filter_by_campaign_id(db: PgPool) {
    let [(project, segment), _] = seed(&db).await;
    sqlx::query(
        "with l as (insert into leads (project_id, segment_id, full_name, email) values ($1, $2, 'Ana', 'ana@smile.example') returning id)
         insert into email_messages (project_id, lead_id, segment_id, direction, kind, status, created_at)
         select $1, l.id, $2, 'inbound', 'inbound', 'received', now() - interval '1 minute' from l",
    )
    .bind(project)
    .bind(segment)
    .execute(&db)
    .await
    .unwrap();
    let server = Server::start(db).await;
    let other = Uuid::new_v4();
    for (tool, args, field) in [
        ("list_replies", json!({}), "total"),
        ("get_inbox_changes", json!({ "project_id": project }), "items"),
    ] {
        let count = |campaign: Uuid| {
            let mut args = args.clone();
            args["campaign_id"] = json!(campaign);
            let server = &server;
            async move {
                let (error, text) = server.call(KEY, tool, args).await;
                assert!(!error, "{tool}: {text}");
                let body: Value = serde_json::from_str(&text).unwrap();
                if field == "total" { body["total"].as_i64().unwrap() } else { body["items"].as_array().unwrap().len() as i64 }
            }
        };
        assert_eq!((count(segment).await, count(other).await), (1, 0), "{tool}");
    }
}
