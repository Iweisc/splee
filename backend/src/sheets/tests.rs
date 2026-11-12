use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use reqwest::Url;
use serde_json::{Value, json};
use sqlx::PgPool;

use super::fake::{Fake, Lost};
use super::*;
use crate::crypto::sha256_hex;
use crate::hooks;
use crate::mail::oauth::tests::{FakeProvider, config};
use crate::models::Lead;

#[test]
fn columns_are_guessed_from_their_headers_like_a_file_import() {
    let headers: Vec<String> = ["Email Address", "First name", "Company", "Website", "Mobile", "Notes", "email"].map(String::from).to_vec();
    let guess = import::guess(&headers);
    let guessed: Vec<(&str, &str)> = guess.iter().map(|(f, h)| (*f, h.as_str())).collect();
    assert_eq!(
        guessed,
        [("company_domain", "Website"), ("company_name", "Company"), ("email", "Email Address"), ("first_name", "First name"), ("phone", "Mobile")],
        "the first column that fits wins, and Notes fits nothing"
    );

    let cells = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(import::row_key(" Ada@Acme.TEST ", &cells(&["x"])), "email:ada@acme.test");
    let no_email = import::row_key("", &cells(&["", "Nobody", "Acme"]));
    assert!(no_email.starts_with("row:"));
    assert_eq!(no_email, import::row_key("not an email", &cells(&["", " Nobody ", "Acme", "", ""])), "spaces and empty cells at the end don't count");
    assert_ne!(no_email, import::row_key("", &cells(&["", "Nobody", "Beta"])));

    let mapping = HashMap::from([("email".to_string(), "Email".to_string()), ("company_name".to_string(), "Firm".to_string())]);
    let err = import::columns(&mapping, &cells(&["Email", "Company"])).unwrap_err();
    assert_eq!(err, "The tab has no column called \"Firm\" anymore. Map its columns again.");
    let only_name = HashMap::from([("full_name".to_string(), "Name".to_string())]);
    assert!(import::columns(&only_name, &cells(&["Name"])).unwrap_err().contains("email addresses"));
}

#[test]
fn a_spreadsheet_link_gives_its_id_and_open_tab() {
    assert_eq!(link("https://docs.google.com/spreadsheets/d/1Bxi_M-9/edit#gid=42"), Some(Link { id: "1Bxi_M-9".into(), gid: Some(42) }));
    assert_eq!(link(" https://docs.google.com/spreadsheets/d/1Bxi/edit?usp=sharing "), Some(Link { id: "1Bxi".into(), gid: None }));
    assert_eq!(link("https://evil.example/spreadsheets/d/1Bxi/edit"), None);
    assert_eq!(link("https://docs.google.com/spreadsheets/d/e/2PACX/pubhtml"), None);
    assert_eq!(range("Bob's list", "!1:1"), "'Bob''s list'!1:1");
}

const KEY: &str = "splee_test_sheets_key";

struct Server {
    base: String,
    client: reqwest::Client,
    db: PgPool,
    fake: Fake,
    provider: FakeProvider,
    state: AppState,
    /// Ana's project in Acme, with the Spring campaign. Cy's org Other has its own project.
    org: Uuid,
    acme: Uuid,
    campaign: Uuid,
    other: Uuid,
}

impl Server {
    async fn start(db: PgPool) -> Self {
        let provider = FakeProvider::start().await;
        let fake = Fake::start().await;
        let cfg = Config { sheets_api_url: fake.base.clone(), ..config(&provider) };
        let state = AppState { db: db.clone(), cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(16).0 };
        let mut ids = vec![];
        for (email, org_name) in [("ana@acme.test", "acme"), ("cy@other.test", "other")] {
            let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, 'x', 'x') returning id").bind(email).fetch_one(&db).await.unwrap();
            let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(org_name).fetch_one(&db).await.unwrap();
            // A member, not an admin: a project's Sheets is up to anyone in it.
            sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(org).bind(user).execute(&db).await.unwrap();
            sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                .bind(sha256_hex(&format!("session-{org_name}")))
                .bind(user)
                .bind(org)
                .execute(&db)
                .await
                .unwrap();
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(org_name).fetch_one(&db).await.unwrap();
            ids.push((org, project));
        }
        sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'sheets', 'splee_test_', $2)").bind(ids[1].0).bind(sha256_hex(KEY)).execute(&db).await.unwrap();
        let campaign: Uuid =
            sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Spring') returning id").bind(ids[0].1).fetch_one(&db).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", crate::routes::router()).with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
        Self { base, client, db, fake, provider, state, org: ids[0].0, acme: ids[0].1, campaign, other: ids[1].1 }
    }

    async fn call(&self, org: &str, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.client.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session=session-{org}"));
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    /// Acme's Google account, as if Ana had signed in an hour ago with this refresh token and a live access token.
    async fn seed_account(&self, refresh: &str) {
        let secret = &self.state.cfg.app_secret;
        sqlx::query(
            "insert into sheet_accounts (project_id, email, refresh_token_enc, access_token_enc, expires_at) values ($1, 'ana@acme.test', $2, $3, now() + interval '1 hour')",
        )
        .bind(self.acme)
        .bind(crypto::encrypt(secret, refresh))
        .bind(crypto::encrypt(secret, "access-seeded"))
        .execute(&self.db)
        .await
        .unwrap();
    }

    async fn lead(&self, project: Uuid, email: &str, name: &str) -> Lead {
        sqlx::query_as("insert into leads (project_id, segment_id, email, full_name, company_name, status) values ($1, $2, $3, $4, 'Brightline', 'hot') returning *")
            .bind(project)
            .bind((project == self.acme).then_some(self.campaign))
            .bind(email)
            .bind(name)
            .fetch_one(&self.db)
            .await
            .unwrap()
    }

    async fn export(&self, spreadsheet: &str, tab: &str) -> Uuid {
        let url = format!("https://docs.google.com/spreadsheets/d/{spreadsheet}/edit");
        let (status, body) = self.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/connections", self.acme), Some(json!({"kind": "export", "url": url, "tab": tab}))).await;
        assert_eq!(status, 200, "{body}");
        body["connections"].as_array().unwrap().iter().find(|c| c["tab"] == tab).unwrap()["id"].as_str().unwrap().parse().unwrap()
    }

    /// Ana signs in to Sheets again, and Google gives the code `code`.
    async fn reconnect(&self, code: &str) {
        let (_, body) = self.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/oauth", self.acme), None).await;
        let q: HashMap<String, String> = Url::parse(body["url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect();
        assert_eq!(q["login_hint"], "ana@acme.test");
        let callback = Url::parse_with_params(&format!("{}/mailboxes/oauth/callback", self.base), [("code", code), ("state", q["state"].as_str())]).unwrap();
        let res = self.client.get(callback).header("cookie", "splee_session=session-acme").send().await.unwrap();
        assert_eq!(res.headers()["location"], format!("/app/p/{}/integrations?sheets=connected", self.acme));
    }

    async fn pending(&self) -> i64 {
        sqlx::query_scalar("select count(*) from sheet_export_rows where written_at is null").fetch_one(&self.db).await.unwrap()
    }

    /// As if export::SETTLE_SECS went by since rows were sent without a clear answer.
    async fn settle(&self) {
        sqlx::query("update sheet_export_rows set sent_at = sent_at - make_interval(secs => $1)").bind(export::SETTLE_SECS).execute(&self.db).await.unwrap();
    }
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn signing_in_asks_only_for_sheets_and_renews_the_token(db: PgPool) {
    let s = Server::start(db).await;
    s.fake.sheet("s1", "Leads sheet", &[("Leads", vec![vec!["Email"]]), ("Hot", vec![])]);
    let (status, body) = s.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/oauth", s.acme), None).await;
    assert_eq!(status, 200, "{body}");
    let q: HashMap<String, String> = Url::parse(body["url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect();
    assert_eq!(q["scope"], "https://www.googleapis.com/auth/spreadsheets openid email");
    assert!(!q.contains_key("include_granted_scopes"), "the token stays apart from a mailbox's");
    assert_eq!((q["redirect_uri"].as_str(), q["access_type"].as_str()), ("http://localhost:3404/api/mailboxes/oauth/callback", "offline"));

    let callback = Url::parse_with_params(&format!("{}/mailboxes/oauth/callback", s.base), [("code", "the-code"), ("state", q["state"].as_str())]).unwrap();
    let res = s.client.get(callback).header("cookie", "splee_session=session-acme").send().await.unwrap();
    assert_eq!(res.headers()["location"], format!("/app/p/{}/integrations?sheets=connected", s.acme));
    let (email, refresh): (String, String) = sqlx::query_as("select email, refresh_token_enc from sheet_accounts where project_id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
    assert_eq!((email.as_str(), crypto::decrypt("test-secret", &refresh).unwrap().as_str()), ("ana@acme.test", "refresh-for-the-code"));
    let mailboxes: i64 = sqlx::query_scalar("select count(*) from mailboxes").fetch_one(&s.db).await.unwrap();
    assert_eq!(mailboxes, 0, "Sheets doesn't add a mailbox");

    // The link's gid picks the tab.
    let open = json!({"url": "https://docs.google.com/spreadsheets/d/s1/edit#gid=100"});
    let (status, opened) = s.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/open", s.acme), Some(open.clone())).await;
    assert_eq!(status, 200, "{opened}");
    assert_eq!((opened["name"].as_str(), opened["tab"].as_str(), opened["tabs"].as_array().unwrap().len()), (Some("Leads sheet"), Some("Hot"), 2));
    assert_eq!(s.fake.requests()[0].token, "access-from-code");

    // Once it has run out, the refresh token gets a new one, which is saved and used.
    sqlx::query("update sheet_accounts set expires_at = now() - interval '1 minute' where project_id = $1").bind(s.acme).execute(&s.db).await.unwrap();
    assert_eq!(s.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/open", s.acme), Some(open)).await.0, 200);
    let form = s.provider.forms.lock().unwrap().last().unwrap().clone();
    assert_eq!((form["grant_type"].as_str(), form["refresh_token"].as_str()), ("refresh_token", "refresh-for-the-code"));
    let used = s.fake.requests().last().unwrap().token.clone();
    assert!(used.starts_with("access-") && used != "access-from-code", "{used}");
    let saved: String = sqlx::query_scalar("select access_token_enc from sheet_accounts where project_id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
    assert_eq!(crypto::decrypt("test-secret", &saved).unwrap(), used);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_revoked_sign_in_says_so_and_asks_to_reconnect(db: PgPool) {
    let s = Server::start(db).await;
    s.fake.sheet("s1", "Leads sheet", &[("Leads", vec![vec!["Email"]])]);
    let open = json!({"url": "https://docs.google.com/spreadsheets/d/s1/edit"});

    // A token Sheets turns down before it runs out is renewed once, and that's enough.
    s.seed_account("still-good").await;
    s.fake.world.lock().unwrap().dead.insert("access-seeded".into());
    assert_eq!(s.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/open", s.acme), Some(open.clone())).await.0, 200);
    assert_eq!(s.provider.hits(), 1);

    // Google revoked the refresh token.
    sqlx::query("update sheet_accounts set refresh_token_enc = $2, expires_at = now() - interval '1 minute' where project_id = $1")
        .bind(s.acme)
        .bind(crypto::encrypt("test-secret", "revoked"))
        .execute(&s.db)
        .await
        .unwrap();
    let (status, body) = s.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/open", s.acme), Some(open.clone())).await;
    assert_eq!((status, body["error"].as_str()), (400, Some(SIGNED_OUT)));
    let (_, page) = s.call("acme", reqwest::Method::GET, &format!("/projects/{}/sheets", s.acme), None).await;
    assert_eq!((page["account"]["needs_reconnect"].as_bool(), page["account"]["error"].as_str()), (Some(true), Some(SIGNED_OUT)), "the card shows Reconnect");
    assert_eq!(s.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/open", s.acme), Some(open)).await.0, 400);
    assert_eq!(s.provider.hits(), 2, "a revoked sign-in isn't tried again");

    // Reconnecting clears it.
    s.reconnect("again").await;
    let (_, page) = s.call("acme", reqwest::Method::GET, &format!("/projects/{}/sheets", s.acme), None).await;
    assert_eq!((page["account"]["needs_reconnect"].as_bool(), &page["account"]["error"]), (Some(false), &Value::Null));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn an_import_adds_each_row_once_and_keeps_adding_new_ones(db: PgPool) {
    let s = Server::start(db).await;
    s.seed_account("still-good").await;
    sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, 'sup@acme.test', 'email', 'manual')").bind(s.org).execute(&s.db).await.unwrap();
    s.fake.sheet("s1", "Leads sheet", &[("Leads", vec![
        vec!["Email", "Name", "Company"],
        vec!["ada@acme.test", "Ada Byron", "Acme"],
        vec!["bob@acme.test", "Bob Ray", "Acme"],
        vec!["ADA@acme.test", "Ada again", "Acme"],
        vec!["sup@acme.test", "Sue Pressed", "Acme"],
        vec!["", "Nobody Yet", "Acme"],
        vec!["", "Nobody Yet", "Acme"],
        vec![],
    ])]);
    let url = "https://docs.google.com/spreadsheets/d/s1/edit#gid=0";
    let (status, preview) = s.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/preview", s.acme), Some(json!({"url": url, "tab": "Leads"}))).await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["suggested_mapping"], json!({"email": "Email", "full_name": "Name", "company_name": "Company"}));
    assert_eq!(preview["rows"].as_array().unwrap().len(), 5, "the header and five rows are read");

    let body = json!({"kind": "import", "url": url, "tab": "Leads", "mapping": preview["suggested_mapping"], "segment_id": s.campaign, "keep_importing": true});
    let (status, created) = s.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/connections", s.acme), Some(body)).await;
    assert_eq!(status, 200, "{created}");
    let import = &created["import"];
    assert_eq!((import["total"].as_i64(), import["imported"].as_i64(), import["skipped"].as_i64()), (Some(4), Some(2), Some(2)), "{import}");
    assert_eq!(import["errors"], json!([{"row": 5, "error": "Suppressed: sup@acme.test"}, {"row": 6, "error": "Missing email"}]));
    assert_eq!(import["filename"], "Google Sheets: Leads sheet › Leads");
    let conn = &created["connections"][0];
    assert_eq!((conn["rows_added"].as_i64(), &conn["error"], conn["keep_importing"].as_bool()), (Some(2), &Value::Null, Some(true)));
    assert!(conn["last_sync_at"].is_string());
    let id: Uuid = conn["id"].as_str().unwrap().parse().unwrap();
    let leads = || async {
        sqlx::query_as::<_, (String, Option<Uuid>)>("select email::text, segment_id from leads where project_id = $1 order by email").bind(s.acme).fetch_all(&s.db).await.unwrap()
    };
    assert_eq!(leads().await, [("ada@acme.test".into(), Some(s.campaign)), ("bob@acme.test".into(), Some(s.campaign))]);

    // Another tool fills the sheet: a new row, and an email for the row that had none.
    s.fake.set_rows("s1", "Leads", vec![
        vec!["Email", "Name", "Company"],
        vec!["ada@acme.test", "Ada Byron", "Acme"],
        vec!["bob@acme.test", "Bob Ray, edited", "Acme"],
        vec!["ADA@acme.test", "Ada again", "Acme"],
        vec!["sup@acme.test", "Sue Pressed", "Acme"],
        vec!["dee@acme.test", "Nobody Yet", "Acme"],
        vec!["", "Nobody Yet", "Acme"],
        vec!["cy@acme.test", "Cy Young", "Acme"],
    ]);
    // Not due yet, so the worker leaves it.
    let reads = s.fake.requests().len();
    tick(&s.state).await.unwrap();
    assert_eq!(s.fake.requests().len(), reads);
    sqlx::query("update sheet_connections set next_import_at = now() - interval '1 second' where id = $1").bind(id).execute(&s.db).await.unwrap();
    tick(&s.state).await.unwrap();
    let emails: Vec<String> = leads().await.into_iter().map(|(e, _)| e).collect();
    assert_eq!(emails, ["ada@acme.test", "bob@acme.test", "cy@acme.test", "dee@acme.test"]);
    let (added, next): (i64, Option<Ts>) = sqlx::query_as("select rows_added, next_import_at from sheet_connections where id = $1").bind(id).fetch_one(&s.db).await.unwrap();
    assert_eq!(added, 4);
    assert!(next.unwrap() > Utc::now() + chrono::Duration::minutes(14));

    // Nothing new: the tab is read, and no import is recorded.
    let (status, now) = s.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/connections/{id}/import", s.acme), None).await;
    assert_eq!((status, &now["import"]), (200, &Value::Null), "{now}");
    let imports: i64 = sqlx::query_scalar("select count(*) from imports where project_id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
    assert_eq!(imports, 2);

    // Off, the worker never reads it.
    let (status, _) = s.call("acme", reqwest::Method::PATCH, &format!("/projects/{}/sheets/connections/{id}", s.acme), Some(json!({"keep_importing": false}))).await;
    assert_eq!(status, 200);
    sqlx::query("update sheet_connections set next_import_at = now() - interval '1 second' where id = $1").bind(id).execute(&s.db).await.unwrap();
    let reads = s.fake.requests().len();
    tick(&s.state).await.unwrap();
    assert_eq!(s.fake.requests().len(), reads);

    // A column the mapping names going missing stops the import with a plain error.
    s.fake.set_rows("s1", "Leads", vec![vec!["E-mail", "Name"], vec!["eve@acme.test", "Eve"]]);
    let (status, body) = s.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/connections/{id}/import", s.acme), None).await;
    assert_eq!((status, body["error"].as_str()), (400, Some("The tab has no column called \"Email\" anymore. Map its columns again.")));
    let error: Option<String> = sqlx::query_scalar("select error from sheet_connections where id = $1").bind(id).fetch_one(&s.db).await.unwrap();
    assert_eq!(error.as_deref(), body["error"].as_str());
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn hot_booked_and_won_leads_are_written_once_even_with_two_ticks(db: PgPool) {
    let s = Server::start(db).await;
    s.seed_account("still-good").await;
    s.fake.sheet("s2", "Pipeline", &[("Hot", vec![]), ("Mine", vec![vec!["Their own header"]])]);
    let hot = s.export("s2", "Hot").await;
    let mine = s.export("s2", "Mine").await;
    let (status, _) = s.call("acme", reqwest::Method::PATCH, &format!("/projects/{}/sheets/connections/{mine}", s.acme), Some(json!({"events": ["deal.won"]}))).await;
    assert_eq!(status, 200);

    let sarah = s.lead(s.acme, "sarah@brightline.example", "Sarah Chen").await;
    sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status, body) values ($1, $2, 'inbound', 'inbound', 'received', $3)")
        .bind(s.acme)
        .bind(sarah.id)
        .bind("Tuesday works.\n\nOn Mon, 9 Nov 2026, Ana wrote:\n> Shall we talk?")
        .execute(&s.db)
        .await
        .unwrap();
    hooks::send_lead_event(&s.state, s.acme, "lead_replied", &sarah, Some(("reply", json!({"body": "Hi"})))).await;
    hooks::send_lead_event(&s.state, s.acme, "hot_lead", &sarah, Some(("reply", json!({"body": "Sounds good, call me =1+1"})))).await;
    sqlx::query("update leads set status = 'meeting_booked' where id = $1").bind(sarah.id).execute(&s.db).await.unwrap();
    hooks::send_lead_event(&s.state, s.acme, "meeting_booked", &sarah, None).await;
    hooks::send_lead_event(&s.state, s.acme, "deal.won", &sarah, Some(("deal", json!({"id": "d1"})))).await;
    hooks::send_lead_event(&s.state, s.acme, "hot_lead", &sarah, Some(("reply", json!({"body": "Again"})))).await;
    // Another org's lead never reaches Acme's sheet.
    let theirs = s.lead(s.other, "zed@other.example", "Zed").await;
    hooks::send_lead_event(&s.state, s.other, "hot_lead", &theirs, None).await;

    s.fake.world.lock().unwrap().append_delay = std::time::Duration::from_millis(300);
    let (a, b) = tokio::join!(tick(&s.state), tick(&s.state));
    a.unwrap();
    b.unwrap();
    tick(&s.state).await.unwrap();

    let link = format!("http://localhost:3404/app/p/{}/inbox?lead={}", s.acme, sarah.id);
    let rows = s.fake.rows("s2", "Hot");
    assert_eq!(rows[0], export::HEADER, "an empty tab gets the header first");
    let cells: Vec<Vec<&str>> = rows[1..].iter().map(|r| r[1..].iter().map(String::as_str).collect()).collect();
    let id = |event| export::splee_id(sarah.id, event);
    let (hot_id, booked_id, won_id) = (id("hot_lead"), id("meeting_booked"), id("deal.won"));
    assert_eq!(cells, [
        vec!["Hot lead", "Sarah Chen", "sarah@brightline.example", "Brightline", "Spring", "Hot", "Sounds good, call me =1+1", &link, &hot_id],
        vec!["Meeting booked", "Sarah Chen", "sarah@brightline.example", "Brightline", "Spring", "Meeting booked", "Tuesday works.", &link, &booked_id],
        vec!["Deal won", "Sarah Chen", "sarah@brightline.example", "Brightline", "Spring", "Meeting booked", "Tuesday works.", &link, &won_id],
    ]);
    assert_eq!(s.fake.rows("s2", "Mine").len(), 2, "a tab with something in its first row gets no header, and only the events it picked");
    assert_eq!(s.fake.appends(), 2, "one append per tab, however many ticks");
    assert!(s.fake.requests().iter().filter(|r| r.method == "POST").all(|r| r.path.ends_with("!A1")));
    assert_eq!(s.pending().await, 0);
    let (_, page) = s.call("acme", reqwest::Method::GET, &format!("/projects/{}/sheets", s.acme), None).await;
    let shown = page["connections"].as_array().unwrap().iter().find(|c| c["id"] == json!(hot)).unwrap().clone();
    assert_eq!((shown["rows_added"].as_i64(), &shown["error"]), (Some(3), &Value::Null));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_tab_gets_one_export_since_two_would_write_each_event_twice(db: PgPool) {
    let s = Server::start(db).await;
    s.seed_account("still-good").await;
    s.fake.sheet("s2", "Pipeline", &[("Hot", vec![]), ("Won", vec![])]);
    s.export("s2", "Hot").await;
    let path = format!("/projects/{}/sheets/connections", s.acme);
    let again = json!({"kind": "export", "url": "https://docs.google.com/spreadsheets/d/s2/edit", "tab": "Hot", "events": ["deal.won"]});
    let (status, body) = s.call("acme", reqwest::Method::POST, &path, Some(again)).await;
    assert_eq!((status, body["error"].as_str()), (409, Some("This tab already gets your lead events.")));
    // Another tab is fine, and so is importing from the same one.
    s.export("s2", "Won").await;
    let import = json!({"kind": "import", "url": "https://docs.google.com/spreadsheets/d/s2/edit", "tab": "Hot", "mapping": {"email": "Email"}});
    assert_eq!(s.call("acme", reqwest::Method::POST, &path, Some(import)).await.0, 200);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_row_google_took_without_saying_so_is_found_in_the_tab_and_not_sent_again(db: PgPool) {
    let s = Server::start(db).await;
    s.seed_account("still-good").await;
    s.fake.sheet("s2", "Pipeline", &[("Hot", vec![])]);
    let hot = s.export("s2", "Hot").await;
    let keys = || s.fake.rows("s2", "Hot").iter().skip(1).map(|r| r.last().unwrap().clone()).collect::<Vec<_>>();

    // Google writes the rows, then answers 503. Sending them again could write them twice, so they wait for the next read.
    let ada = s.lead(s.acme, "ada@brightline.example", "Ada").await;
    hooks::send_lead_event(&s.state, s.acme, "hot_lead", &ada, None).await;
    s.fake.world.lock().unwrap().lost.push_back(Lost::Status(reqwest::StatusCode::SERVICE_UNAVAILABLE));
    tick(&s.state).await.unwrap();
    assert_eq!((s.fake.appends(), keys(), s.pending().await), (1, vec![export::splee_id(ada.id, "hot_lead")], 1));
    let error: Option<String> = sqlx::query_scalar("select error from sheet_connections where id = $1").bind(hot).fetch_one(&s.db).await.unwrap();
    assert!(error.is_some());
    // As if Google were still writing them: the tab doesn't show them yet, and the next tick mustn't send them again.
    let written = s.fake.rows("s2", "Hot");
    let set = |rows: &[Vec<String>]| s.fake.set_rows("s2", "Hot", rows.iter().map(|r| r.iter().map(String::as_str).collect()).collect());
    set(&written[..1]);
    tick(&s.state).await.unwrap();
    assert_eq!((s.fake.appends(), s.pending().await), (1, 1), "nothing is sent while the first try may still land");
    set(&written);
    s.settle().await;
    tick(&s.state).await.unwrap();
    assert_eq!((s.fake.appends(), s.fake.rows("s2", "Hot").len(), s.pending().await), (1, 2, 0), "the read found it, so it's written");

    // Google writes them and answers too late.
    let bo = s.lead(s.acme, "bo@brightline.example", "Bo").await;
    hooks::send_lead_event(&s.state, s.acme, "hot_lead", &bo, None).await;
    s.fake.world.lock().unwrap().lost.push_back(Lost::Late);
    tick(&s.state).await.unwrap();
    assert_eq!((s.fake.appends(), s.pending().await), (2, 1));
    s.settle().await;
    tick(&s.state).await.unwrap();
    assert_eq!((s.fake.appends(), s.pending().await), (2, 0));

    // Google takes them and says so, but Splee can't note it.
    let cy = s.lead(s.acme, "cy@brightline.example", "Cy").await;
    hooks::send_lead_event(&s.state, s.acme, "hot_lead", &cy, None).await;
    sqlx::raw_sql(
        "create function refuse() returns trigger language plpgsql as $$ begin raise exception 'disk full'; end $$;
         create trigger refuse before update on sheet_export_rows for each row when (new.written_at is not null) execute function refuse();",
    )
    .execute(&s.db)
    .await
    .unwrap();
    tick(&s.state).await.unwrap();
    assert_eq!((s.fake.appends(), s.pending().await), (3, 1));
    sqlx::raw_sql("drop trigger refuse on sheet_export_rows").execute(&s.db).await.unwrap();
    s.settle().await;
    tick(&s.state).await.unwrap();
    assert_eq!((s.fake.appends(), s.pending().await), (3, 0));

    // A row that's somewhere in the tab already, like one written before Splee lost track, isn't sent either.
    let dee = s.lead(s.acme, "dee@brightline.example", "Dee").await;
    let mut rows = s.fake.rows("s2", "Hot");
    rows.push(vec!["moved".into(), export::splee_id(dee.id, "hot_lead")]);
    s.fake.set_rows("s2", "Hot", rows.iter().map(|r| r.iter().map(String::as_str).collect()).collect());
    hooks::send_lead_event(&s.state, s.acme, "hot_lead", &dee, None).await;
    tick(&s.state).await.unwrap();
    assert_eq!((s.fake.appends(), s.pending().await), (3, 0));

    let ids = |lead: &Lead| export::splee_id(lead.id, "hot_lead");
    assert_eq!(keys(), [ids(&ada), ids(&bo), ids(&cy), ids(&dee)], "each event once, in the order it happened");
    let (added, error): (i64, Option<String>) = sqlx::query_as("select rows_added, error from sheet_connections where id = $1").bind(hot).fetch_one(&s.db).await.unwrap();
    assert_eq!((added, error), (4, None));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_request_from_before_a_reconnect_failing_after_it_keeps_the_new_sign_in(db: PgPool) {
    let s = Server::start(db).await;
    s.seed_account("still-good").await;
    s.fake.sheet("s1", "Leads sheet", &[("Leads", vec![vec!["Email"]])]);
    // The stored token ran out, so the request gets a fresh one, and Google turning that down would sign the project out.
    sqlx::query("update sheet_accounts set expires_at = now() - interval '1 minute' where project_id = $1").bind(s.acme).execute(&s.db).await.unwrap();
    let server = &s;
    let result = authed(&s.state, s.acme, |c| async move {
        // While it runs, Ana signs in again, and Google drops the token it holds.
        server.reconnect("again").await;
        server.fake.world.lock().unwrap().dead.insert(c.token.clone());
        c.spreadsheet("s1").await
    })
    .await;
    assert!(matches!(result, Err(SheetError::Unauthorized(1))), "{result:?}");

    let (_, page) = s.call("acme", reqwest::Method::GET, &format!("/projects/{}/sheets", s.acme), None).await;
    assert_eq!((page["account"]["needs_reconnect"].as_bool(), &page["account"]["error"]), (Some(false), &Value::Null));
    let open = json!({"url": "https://docs.google.com/spreadsheets/d/s1/edit"});
    assert_eq!(s.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/open", s.acme), Some(open)).await.0, 200);
    assert_eq!(s.fake.requests().last().unwrap().token, "access-from-code");

    // The new sign-in failing still signs out.
    sign_out(&s.db, s.acme, 2).await.unwrap();
    let out: bool = sqlx::query_scalar("select needs_reconnect from sheet_accounts where project_id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
    assert!(out);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn google_asking_to_slow_down_is_retried_then_waited_out(db: PgPool) {
    let s = Server::start(db).await;
    s.seed_account("still-good").await;
    s.fake.sheet("s2", "Pipeline", &[("Hot", vec![vec!["Date"]])]);
    let hot = s.export("s2", "Hot").await;
    let first = s.lead(s.acme, "ana@brightline.example", "Ana").await;
    hooks::send_lead_event(&s.state, s.acme, "hot_lead", &first, None).await;

    // Two 429s, then Google lets the read through.
    let before = s.fake.requests().len();
    s.fake.world.lock().unwrap().busy = 2;
    tick(&s.state).await.unwrap();
    assert_eq!(s.fake.requests().len() - before, 4, "two refused reads, the read, the append");
    assert_eq!(s.fake.rows("s2", "Hot").len(), 2);

    // Google keeps refusing: the account waits, and the row with it.
    let second = s.lead(s.acme, "bo@brightline.example", "Bo").await;
    hooks::send_lead_event(&s.state, s.acme, "hot_lead", &second, None).await;
    s.fake.world.lock().unwrap().busy = 100;
    tick(&s.state).await.unwrap();
    let held: Option<Ts> = sqlx::query_scalar("select held_until from sheet_accounts where project_id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
    assert!(held.unwrap() > Utc::now() + chrono::Duration::seconds(90));
    let error: Option<String> = sqlx::query_scalar("select error from sheet_connections where id = $1").bind(hot).fetch_one(&s.db).await.unwrap();
    assert_eq!(error.as_deref(), Some(SheetError::Limited(None).message().as_str()));
    let calls = s.fake.requests().len();
    tick(&s.state).await.unwrap();
    let open = json!({"url": "https://docs.google.com/spreadsheets/d/s2/edit"});
    assert_eq!(s.call("acme", reqwest::Method::POST, &format!("/projects/{}/sheets/open", s.acme), Some(open)).await.0, 429);
    assert_eq!(s.fake.requests().len(), calls, "nothing calls Google while the account waits");

    sqlx::query("update sheet_accounts set held_until = now() - interval '1 second' where project_id = $1").bind(s.acme).execute(&s.db).await.unwrap();
    s.fake.world.lock().unwrap().busy = 0;
    tick(&s.state).await.unwrap();
    assert_eq!(s.fake.rows("s2", "Hot").len(), 3);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn other_organizations_can_not_see_or_read_a_projects_sheets(db: PgPool) {
    let s = Server::start(db).await;
    s.seed_account("still-good").await;
    s.fake.sheet("s1", "Leads sheet", &[("Leads", vec![vec!["Email"], vec!["ada@acme.test"]])]);
    let conn: Uuid = sqlx::query_scalar("insert into sheet_connections (project_id, kind, spreadsheet_id, tab, mapping) values ($1, 'import', 's1', 'Leads', $2) returning id")
        .bind(s.acme)
        .bind(json!({"email": "Email"}))
        .fetch_one(&s.db)
        .await
        .unwrap();
    let acme = s.acme;
    assert_eq!(s.call("other", reqwest::Method::GET, &format!("/projects/{acme}/sheets"), None).await.0, 404);
    assert_eq!(s.call("other", reqwest::Method::POST, &format!("/projects/{acme}/sheets/oauth"), None).await.0, 404);
    assert_eq!(s.call("other", reqwest::Method::POST, &format!("/projects/{acme}/sheets/connections/{conn}/import"), None).await.0, 404);
    // Their own project, with Acme's connection, finds nothing either.
    let theirs = s.other;
    assert_eq!(s.call("other", reqwest::Method::POST, &format!("/projects/{theirs}/sheets/connections/{conn}/import"), None).await.0, 404);
    assert_eq!(s.call("other", reqwest::Method::PATCH, &format!("/projects/{theirs}/sheets/connections/{conn}"), Some(json!({"keep_importing": true}))).await.0, 404);
    s.call("other", reqwest::Method::DELETE, &format!("/projects/{theirs}/sheets/connections/{conn}"), None).await;
    let (_, page) = s.call("other", reqwest::Method::GET, &format!("/projects/{theirs}/sheets"), None).await;
    assert_eq!((&page["account"], page["connections"].as_array().map(Vec::len)), (&Value::Null, Some(0)));

    // Other's API key: the API and MCP refuse Acme's project.
    let v1 = |path: String| s.client.post(format!("{}{path}", s.base)).bearer_auth(KEY);
    assert_eq!(v1(format!("/v1/projects/{acme}/sheets/{conn}/import")).send().await.unwrap().status(), 404);
    assert_eq!(s.client.get(format!("{}/v1/projects/{acme}/sheets", s.base)).bearer_auth(KEY).send().await.unwrap().status(), 404);
    let (status, list) = {
        let res = s.client.get(format!("{}/v1/projects/{theirs}/sheets", s.base)).bearer_auth(KEY).send().await.unwrap();
        (res.status().as_u16(), res.json::<Value>().await.unwrap())
    };
    assert_eq!((status, list), (200, json!([])));
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "import_from_sheet", "arguments": { "project_id": acme, "connection_id": conn } } });
    let res = v1("/mcp".into()).header("accept", "application/json, text/event-stream").json(&request).send().await.unwrap();
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["result"]["isError"], true, "{body}");

    let still: i64 = sqlx::query_scalar("select count(*) from sheet_connections where id = $1").bind(conn).fetch_one(&s.db).await.unwrap();
    assert_eq!(still, 1);
    assert!(s.fake.requests().is_empty(), "Google was never asked");
    let leads: i64 = sqlx::query_scalar("select count(*) from leads").fetch_one(&s.db).await.unwrap();
    assert_eq!(leads, 0);
}
