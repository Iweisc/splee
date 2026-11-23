use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use chrono::{NaiveDate, TimeZone, Utc};
use flate2::read::GzDecoder;
use rusty_s3::{Credentials, S3Action};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::fake::{Fake, KEY_ID};
use super::s3::{self, PART_SIZE, Target};
use super::*;
use crate::config::Config;
use crate::crypto::sha256_hex;

fn target(endpoint: &str, region: &str, bucket: &str, path_style: bool) -> Target {
    Target { endpoint: endpoint.into(), region: region.into(), bucket: bucket.into(), path_style, access_key_id: KEY_ID.into(), secret: "s".into() }
}

/// The presigned GET in AWS's docs, "Example: Signature Calculations for the Query String Parameters", signed through the same
/// bucket the export builds. https://docs.aws.amazon.com/AmazonS3/latest/API/sigv4-query-string-auth.html
#[test]
fn requests_are_signed_like_the_aws_documented_example() {
    let t = Target {
        access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
        secret: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
        ..target("https://s3.amazonaws.com", "us-east-1", "examplebucket", false)
    };
    let bucket = s3::bucket(&t).unwrap();
    let credentials = Credentials::new(t.access_key_id.clone(), t.secret.clone());
    let at: jiff::Timestamp = "2013-05-24T00:00:00Z".parse().unwrap();
    let url = bucket.get_object(Some(&credentials), "test.txt").sign_with_time(Duration::from_secs(86400), &at);
    assert_eq!(
        url.as_str(),
        "https://examplebucket.s3.amazonaws.com/test.txt?X-Amz-Algorithm=AWS4-HMAC-SHA256\
         &X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20130524T000000Z\
         &X-Amz-Expires=86400&X-Amz-SignedHeaders=host&X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
    );
}

#[test]
fn the_bucket_goes_in_the_host_or_in_the_path() {
    let object = |t: &Target| s3::bucket(t).unwrap().object_url("raw/leads/dt=2026-11-13/part-0000.jsonl.gz").to_string();
    assert_eq!(object(&target("", "eu-west-1", "warehouse", false)), "https://warehouse.s3.eu-west-1.amazonaws.com/raw/leads/dt%3D2026-11-13/part-0000.jsonl.gz");
    assert_eq!(
        object(&target("https://acc123.r2.cloudflarestorage.com/", "auto", "warehouse", false)),
        "https://warehouse.acc123.r2.cloudflarestorage.com/raw/leads/dt%3D2026-11-13/part-0000.jsonl.gz"
    );
    assert_eq!(object(&target("minio.example.com:9000", "us-east-1", "warehouse", true)), "https://minio.example.com:9000/warehouse/raw/leads/dt%3D2026-11-13/part-0000.jsonl.gz");
    assert_eq!(object(&target("http://minio.example.com", "us-east-1", "warehouse", true)), "http://minio.example.com/warehouse/raw/leads/dt%3D2026-11-13/part-0000.jsonl.gz");
    assert!(s3::endpoint_url("https://storage.example.com/warehouse", "auto").unwrap_err().contains("Leave the bucket"));
    assert!(s3::endpoint_url("ftp://storage.example.com", "auto").is_err());
    assert_eq!(file_key("/raw/", "leads", NaiveDate::from_ymd_opt(2026, 11, 13).unwrap()), "raw/leads/dt=2026-11-13/part-0000.jsonl.gz");
    assert_eq!(manifest_key("", NaiveDate::from_ymd_opt(2026, 11, 13).unwrap()), "_manifest/dt=2026-11-13.json");
}

#[test]
fn refusals_say_what_to_fix() {
    let xml = |code: &str| format!("<?xml version=\"1.0\"?><Error><Code>{code}</Code><Message>x</Message></Error>");
    assert!(s3::refused(403, &xml("SignatureDoesNotMatch"), "w").contains("Paste the secret again"));
    assert_eq!(
        s3::refused(400, "<Error><Code>AuthorizationQueryParametersError</Code><Region>eu-central-1</Region></Error>", "w"),
        "The bucket is in eu-central-1. Set the region to eu-central-1."
    );
    assert_eq!(s3::refused(503, &xml("SlowDown"), "w"), "The storage service answered 503: x");
}

fn config(s3_api_url: &str) -> Config {
    Config { public_url: "https://splee.example".into(), s3_api_url: s3_api_url.into(), ..Config::test() }
}

const SECRET: &str = "wJalrXUtnFEMI-splee-test-secret-key";
const API_KEY: &str = "splee_test_exports_acme";
const OTHER_KEY: &str = "splee_test_exports_other";
const PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$not-a-real-hash";

struct Server {
    base: String,
    client: reqwest::Client,
    db: PgPool,
    fake: Fake,
    state: AppState,
    /// Bodies the team chat got.
    chats: Arc<Mutex<Vec<Value>>>,
    chat_url: String,
    /// Acme has two projects. Ana is its admin and Ben a member. Cy is Other's admin.
    acme: Uuid,
    projects: [Uuid; 2],
    other: Uuid,
    other_project: Uuid,
}

impl Server {
    async fn start(db: PgPool) -> Self {
        let fake = Fake::start().await;
        fake.bucket("warehouse");
        let state = AppState {
            db: db.clone(),
            cfg: Arc::new(config(&fake.base)),
            events: tokio::sync::broadcast::channel(16).0,
            rate_limits: Default::default(),
        };

        let chats: Arc<Mutex<Vec<Value>>> = Arc::default();
        let got = chats.clone();
        let chat = Router::new().route(
            "/hook/{id}",
            post(move |axum::Json(body): axum::Json<Value>| {
                let got = got.clone();
                async move { got.lock().unwrap().push(body) }
            }),
        );
        let port = crate::hooks::serve_test_receiver(chat).await;

        let mut orgs = vec![];
        for name in ["acme", "other"] {
            let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap();
            orgs.push(org);
        }
        for (email, org, role, session) in [
            ("ana@acme.example", orgs[0], "admin", "acme"),
            ("ben@acme.example", orgs[0], "member", "acme-member"),
            ("cy@other.example", orgs[1], "owner", "other"),
        ] {
            let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, 'x', $2) returning id")
                .bind(email)
                .bind(PASSWORD_HASH)
                .fetch_one(&db)
                .await
                .unwrap();
            sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, $3)").bind(org).bind(user).bind(role).execute(&db).await.unwrap();
            crate::auth::sign_in(&db, user, org, &format!("session-{session}")).await;
        }
        for (org, key) in [(orgs[0], API_KEY), (orgs[1], OTHER_KEY)] {
            sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'exports', 'splee_test_', $2)").bind(org).bind(sha256_hex(key)).execute(&db).await.unwrap();
        }
        let mut projects = vec![];
        for (org, name) in [(orgs[0], "Acme EU"), (orgs[0], "Acme US"), (orgs[1], "Other")] {
            projects.push(sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(&db).await.unwrap());
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", crate::routes::router(&state)).with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self {
            base,
            client: reqwest::Client::new(),
            db,
            fake,
            state,
            chats,
            chat_url: format!("http://127.0.0.1:{port}/hook"),
            acme: orgs[0],
            projects: [projects[0], projects[1]],
            other: orgs[1],
            other_project: projects[2],
        }
    }

    async fn call(&self, session: &str, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.client.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session=session-{session}"));
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    async fn save(&self, body: Value) -> Value {
        let (status, saved) = self.call("acme", reqwest::Method::PUT, "/organization/exports", Some(body)).await;
        assert_eq!(status, 200, "{saved}");
        saved
    }

    /// A lead, a campaign, a company, a sent email with an open, a click a scanner made and a reply, a deal, a funding filing and
    /// a job opening in the project, all marked with `tag`.
    async fn seed(&self, org: Uuid, project: Uuid, tag: &str) {
        let db = &self.db;
        let campaign: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, $2) returning id")
            .bind(project)
            .bind(format!("{tag} campaign"))
            .fetch_one(db)
            .await
            .unwrap();
        let company: Uuid = sqlx::query_scalar("insert into companies (project_id, segment_id, domain, name, score) values ($1, $2, $3, $4, 61) returning id")
            .bind(project)
            .bind(campaign)
            .bind(format!("{tag}.example"))
            .bind(format!("{tag} Ltd"))
            .fetch_one(db)
            .await
            .unwrap();
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, segment_id, company_id, email, full_name, company_name, company_domain, status, score)
             values ($1, $2, $3, $4, $5, $6, $7, 'replied', 72) returning id",
        )
        .bind(project)
        .bind(campaign)
        .bind(company)
        .bind(format!("jo@{tag}.example"))
        .bind(format!("Jo {tag}"))
        .bind(format!("{tag} Ltd"))
        .bind(format!("{tag}.example"))
        .fetch_one(db)
        .await
        .unwrap();
        let sent: Uuid = sqlx::query_scalar(
            "insert into email_messages (project_id, lead_id, segment_id, direction, kind, subject, body, status, sent_at, message_id, open_token, to_email)
             values ($1, $2, $3, 'outbound', 'initial', 'Quick question', $4, 'sent', now(), $5, $6, $7) returning id",
        )
        .bind(project)
        .bind(lead)
        .bind(campaign)
        .bind(format!("Hi Jo, {tag} body text"))
        .bind(format!("<{tag}@mail.splee.example>"))
        .bind(format!("open-token-{tag}"))
        .bind(format!("jo@{tag}.example"))
        .fetch_one(db)
        .await
        .unwrap();
        sqlx::query(
            "insert into email_messages (project_id, lead_id, segment_id, direction, kind, subject, body, status, in_reply_to, from_email, classification)
             values ($1, $2, $3, 'inbound', 'inbound', 'Re: Quick question', $4, 'received', $5, $6, 'interested')",
        )
        .bind(project)
        .bind(lead)
        .bind(campaign)
        .bind(format!("Sounds good, {tag} reply text"))
        .bind(format!("<{tag}@mail.splee.example>"))
        .bind(format!("jo@{tag}.example"))
        .execute(db)
        .await
        .unwrap();
        for (kind, filter) in [("open", None), ("click", Some("scanner"))] {
            sqlx::query("insert into tracking_hits (project_id, email_id, lead_id, kind, filter_reason) values ($1, $2, $3, $4, $5)")
                .bind(project)
                .bind(sent)
                .bind(lead)
                .bind(kind)
                .bind(filter)
                .execute(db)
                .await
                .unwrap();
        }
        let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, position) values ($1, 'Proposal', 1) returning id").bind(org).fetch_one(db).await.unwrap();
        sqlx::query("insert into deals (org_id, project_id, lead_id, stage_id, value_cents) values ($1, $2, $3, $4, 1200000)")
            .bind(org)
            .bind(project)
            .bind(lead)
            .bind(stage)
            .execute(db)
            .await
            .unwrap();
        sqlx::query(
            "insert into company_signals (project_id, domain, kind, source, key, happened_on, title, amount, url)
             values ($1, $2, 'funding_filing', 'sec', $3, current_date, 'Form D', 5000000, 'https://sec.example/filing')",
        )
        .bind(project)
        .bind(format!("{tag}.example"))
        .bind(format!("sec:{tag}"))
        .execute(db)
        .await
        .unwrap();
        sqlx::query("insert into job_openings (project_id, domain, provider, title, url) values ($1, $2, 'greenhouse', 'Head of Data', 'https://jobs.example/1')")
            .bind(project)
            .bind(format!("{tag}.example"))
            .execute(db)
            .await
            .unwrap();
    }

    async fn runs(&self) -> Vec<Run> {
        recent(&self.db, self.acme, 50).await.unwrap()
    }

    /// Waits for the Export now the API started. It looks for the newest manual run rather than the newest run, because the daily
    /// runs here start at made-up hours of today, which sort after a manual run started now whenever the tests run before them.
    async fn settled(&self) -> Run {
        for _ in 0..200 {
            let runs = self.runs().await;
            if let Some(run) = runs.iter().find(|r| r.trigger == "manual")
                && run.status != "running"
            {
                return run.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the export didn't finish");
    }
}

fn settings(tables: &[&str]) -> Value {
    json!({
        "endpoint": "https://s3.storage.example", "region": "eu-west-1", "bucket": "warehouse", "prefix": "/splee/raw/",
        "access_key_id": KEY_ID, "secret_access_key": SECRET, "tables": tables,
    })
}

const ALL: [&str; 9] = ["leads", "companies", "campaigns", "emails", "replies", "deals", "company_signals", "job_openings", "scores"];

fn at(date: NaiveDate, hour: u32) -> Ts {
    Utc.from_utc_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
}

fn gunzip(bytes: &[u8]) -> String {
    let mut text = String::new();
    GzDecoder::new(bytes).read_to_string(&mut text).unwrap();
    text
}

/// The keys of a raw JSON line in the order they're written, which serde_json's map doesn't keep.
fn keys(line: &str) -> Vec<String> {
    struct Keys(Vec<String>);
    impl<'de> serde::Deserialize<'de> for Keys {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct Visit;
            impl<'de> serde::de::Visitor<'de> for Visit {
                type Value = Keys;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("an object")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Keys, A::Error> {
                    let mut keys = vec![];
                    while let Some((key, _)) = map.next_entry::<String, serde::de::IgnoredAny>()? {
                        keys.push(key);
                    }
                    Ok(Keys(keys))
                }
            }
            d.deserialize_map(Visit)
        }
    }
    serde_json::from_str::<Keys>(line).unwrap().0
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn only_admins_see_the_settings_and_the_secret_never_comes_back(db: PgPool) {
    let s = Server::start(db).await;
    for (method, path) in [
        (reqwest::Method::GET, "/organization/exports"),
        (reqwest::Method::PUT, "/organization/exports"),
        (reqwest::Method::POST, "/organization/exports/test"),
        (reqwest::Method::POST, "/organization/exports/run"),
        (reqwest::Method::DELETE, "/organization/exports"),
    ] {
        let (status, _) = s.call("acme-member", method.clone(), path, Some(settings(&["leads"]))).await;
        assert_eq!(status, 403, "{method} {path} is for admins");
    }

    let saved = s.save(settings(&["leads", "emails"])).await;
    let settings_view = &saved["settings"];
    assert_eq!((settings_view["bucket"].as_str(), settings_view["prefix"].as_str()), (Some("warehouse"), Some("splee/raw")));
    assert_eq!(settings_view["tables"], json!(["leads", "emails"]));
    let (_, shown) = s.call("acme", reqwest::Method::GET, "/organization/exports", None).await;
    for body in [&saved, &shown] {
        let text = body.to_string();
        assert!(!text.contains(SECRET) && !text.contains("secret"), "{text}");
    }
    let enc: String = sqlx::query_scalar("select secret_key_enc from export_settings where org_id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
    assert_ne!(enc, SECRET);
    assert_eq!(crypto::decrypt("test-secret", &enc).unwrap(), SECRET);

    // Saving without a secret keeps the saved one.
    let mut again = settings(&["leads"]);
    again["secret_access_key"] = json!("");
    s.save(again).await;
    let enc: String = sqlx::query_scalar("select secret_key_enc from export_settings where org_id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
    assert_eq!(crypto::decrypt("test-secret", &enc).unwrap(), SECRET);

    let (status, body) = s.call("acme", reqwest::Method::PUT, "/organization/exports", Some(settings(&["users"]))).await;
    assert_eq!((status, body["error"].as_str()), (400, Some("users isn't a table Splee exports.")));
    let (status, _) = s.call("acme", reqwest::Method::PUT, "/organization/exports", Some(settings(&[]))).await;
    assert_eq!(status, 400);
    let (_, theirs) = s.call("other", reqwest::Method::GET, "/organization/exports", None).await;
    assert!(theirs["settings"].is_null(), "another organization's settings stay apart");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn test_connection_writes_and_deletes_a_file_or_says_what_is_wrong(db: PgPool) {
    let s = Server::start(db).await;
    s.fake.bucket("locked");
    s.fake.world.lock().unwrap().denied.insert("locked".into());
    let srv = &s;
    let test = move |body: Value| async move { srv.call("acme", reqwest::Method::POST, "/organization/exports/test", Some(body)).await };

    let (status, ok) = test(settings(&ALL)).await;
    assert_eq!((status, &ok), (200, &json!({"ok": true, "error": null})));
    assert!(s.fake.keys("warehouse").is_empty(), "the test file is gone");
    let seen = s.fake.requests();
    assert_eq!(seen.iter().map(|r| r.method.as_str()).collect::<Vec<_>>(), ["PUT", "DELETE"]);
    assert_eq!(seen[0].host, "warehouse.s3.storage.example", "virtual-hosted: the bucket is in the host");
    assert!(seen[0].path.starts_with("/splee/raw/_splee/connection-test-"), "{}", seen[0].path);

    let mut path_style = settings(&ALL);
    path_style["path_style"] = json!(true);
    assert_eq!(test(path_style).await.1["ok"], true);
    let seen = s.fake.requests().pop().unwrap();
    assert_eq!(seen.host, "s3.storage.example");
    assert!(seen.path.starts_with("/warehouse/splee/raw/_splee/"), "path-style: the bucket is in the path");

    let error = move |body: Value| async move { test(body).await.1["error"].as_str().unwrap_or_default().to_string() };
    let mut wrong_key = settings(&ALL);
    wrong_key["access_key_id"] = json!("AKIDNOTKNOWN");
    assert_eq!(error(wrong_key).await, "The storage service doesn't know this access key ID. Check it, or make a new key.");
    let mut missing = settings(&ALL);
    missing["bucket"] = json!("nope");
    assert_eq!(error(missing).await, "There's no bucket called nope at this endpoint. Check the bucket name, the endpoint and the region.");
    let mut locked = settings(&ALL);
    locked["bucket"] = json!("locked");
    assert_eq!(error(locked).await, "Access denied: this key isn't allowed to write to locked. Give it permission to put and delete objects there.");

    // Nothing listens on a port that was just freed.
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
    let state = AppState { cfg: Arc::new(config(&format!("http://{closed}"))), ..s.state.clone() };
    let unreachable = test_connection(&state, &target("https://s3.storage.example", "eu-west-1", "warehouse", false), "").await.unwrap_err();
    assert!(unreachable.starts_with("Couldn't reach warehouse.s3.storage.example: ") && unreachable.ends_with("Check the endpoint."), "{unreachable}");
    // Without the fake, an endpoint on this machine is turned away before anything is sent.
    let state = AppState { cfg: Arc::new(config("")), ..s.state.clone() };
    let local = test_connection(&state, &target("http://127.0.0.1:9", "us-east-1", "warehouse", true), "").await.unwrap_err();
    assert!(local.contains("127.0.0.1 is not a public address"), "{local}");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_daily_export_writes_every_table_of_the_organization_then_the_manifest(db: PgPool) {
    let s = Server::start(db).await;
    s.seed(s.acme, s.projects[0], "brightline").await;
    s.seed(s.acme, s.projects[1], "northwind").await;
    s.seed(s.other, s.other_project, "outsider").await;
    // Enough leads that their file outgrows a part and goes up in several.
    sqlx::query(
        "insert into leads (project_id, email, full_name, title, company_name, notes)
         select $1, 'lead' || n || '@bulk.example', 'Lead ' || n, 'Head of ' || md5(n::text), 'Bulk ' || md5(n::text || 'x'), md5(n::text || 'y') || md5(n::text || 'z')
         from generate_series(1, 400) n",
    )
    .bind(s.projects[0])
    .execute(&s.db)
    .await
    .unwrap();
    sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'leaked?', 'splee_x', 'api-key-hash-sentinel')").bind(s.acme).execute(&s.db).await.unwrap();
    s.save(settings(&ALL)).await;

    let today = Utc::now().date_naive();
    tick(&s.state, at(today, QUIET_HOUR - 1)).await.unwrap();
    assert!(s.runs().await.is_empty(), "nothing before the quiet hour");
    tick(&s.state, at(today, QUIET_HOUR + 1)).await.unwrap();
    let runs = s.runs().await;
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!((run.status.as_str(), run.trigger.as_str(), run.error.as_deref(), run.dt), ("done", "daily", None, today));

    let seen = s.fake.requests();
    let leads_key = file_key("splee/raw", "leads", today);
    let leads_path = format!("/{}", leads_key.replace('=', "%3D"));
    let parts = seen.iter().filter(|r| r.path == leads_path && r.query.contains_key("partNumber")).count();
    assert!(parts >= 2, "the leads went up in {parts} part");
    assert!(seen.iter().filter(|r| r.path == leads_path && r.query.contains_key("partNumber")).all(|r| r.bytes >= PART_SIZE || r.query["partNumber"] == parts.to_string()));
    let last = seen.last().unwrap();
    assert_eq!(last.path, format!("/{}", manifest_key("splee/raw", today).replace('=', "%3D")), "the manifest is written last");

    let manifest: Value = serde_json::from_slice(&s.fake.object("warehouse", &manifest_key("splee/raw", today)).unwrap()).unwrap();
    assert_eq!((manifest["schema_version"].as_i64(), manifest["dt"].as_str()), (Some(1), Some(today.to_string().as_str())));
    assert_eq!(manifest["organization_id"], json!(s.acme));
    let listed: Vec<&str> = manifest["files"].as_array().unwrap().iter().map(|f| f["table"].as_str().unwrap()).collect();
    assert_eq!(listed, ALL);

    let mut everything = String::new();
    let mut tables: std::collections::HashMap<String, Vec<Value>> = Default::default();
    let mut raw: std::collections::HashMap<String, String> = Default::default();
    for file in manifest["files"].as_array().unwrap() {
        let key = file["key"].as_str().unwrap();
        let table = file["table"].as_str().unwrap();
        assert_eq!(key, file_key("splee/raw", table, today));
        let bytes = s.fake.object("warehouse", key).unwrap_or_else(|| panic!("{key} is missing"));
        assert_eq!(file["bytes"].as_i64(), Some(bytes.len() as i64));
        let text = gunzip(&bytes);
        assert!(text.ends_with('\n'));
        let rows: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(file["rows"].as_i64(), Some(rows.len() as i64), "{table}");
        for row in &rows {
            let project: Uuid = row["project_id"].as_str().unwrap().parse().unwrap();
            assert!(s.projects.contains(&project), "{table}: a row of another organization's project: {row}");
        }
        everything.push_str(&text);
        tables.insert(table.into(), rows);
        raw.insert(table.into(), text);
    }
    assert_eq!(run.rows, tables.values().map(|r| r.len() as i64).sum::<i64>());

    // The schema SPEC.md documents, key by key and in order.
    let line = |table: &str, has: &str| keys(raw[table].lines().find(|l| l.contains(has) || has.is_empty()).unwrap());
    let lead = tables["leads"].iter().find(|r| r["email"] == "jo@brightline.example").unwrap();
    assert_eq!(
        line("leads", "jo@brightline.example"),
        [
            "id", "project_id", "campaign_id", "company_id", "full_name", "first_name", "last_name", "title", "email", "email_status", "phone", "linkedin_url",
            "x_url", "company_name", "company_domain", "city", "country_code", "language", "source", "status", "status_changed_at", "owner_id", "score", "notes",
            "last_contacted_at", "meeting_at", "created_at"
        ]
    );
    assert_eq!((lead["status"].as_str(), lead["score"].as_i64()), (Some("replied"), Some(72)));
    assert!(lead["created_at"].as_str().unwrap().ends_with("+00:00"), "timestamps are UTC");
    assert_eq!(tables["leads"].len(), 402);
    let email = tables["emails"].iter().find(|r| r["to_email"] == "jo@brightline.example").unwrap();
    assert_eq!(
        line("emails", "jo@brightline.example"),
        [
            "id", "project_id", "lead_id", "campaign_id", "mailbox_id", "channel", "kind", "step", "variant_id", "from_email", "to_email", "subject", "body",
            "language", "status", "error", "sent_at", "opens", "clicks", "replies", "created_at"
        ]
    );
    assert_eq!((email["opens"].as_i64(), email["clicks"].as_i64(), email["replies"].as_i64()), (Some(1), Some(0), Some(1)), "the scanner's click doesn't count");
    assert!(email["body"].is_null(), "no bodies unless asked");
    let reply = tables["replies"].iter().find(|r| r["from_email"] == "jo@brightline.example").unwrap();
    assert_eq!((&reply["email_id"], reply["classification"].as_str(), &reply["body"]), (&email["id"], Some("interested"), &Value::Null));
    let deal = &tables["deals"][0];
    assert_eq!((deal["stage"].as_str(), deal["value_cents"].as_i64()), (Some("Proposal"), Some(1_200_000)));
    assert_eq!(tables["company_signals"][0]["amount"].as_i64(), Some(5_000_000));
    assert_eq!(tables["job_openings"][0]["title"].as_str(), Some("Head of Data"));
    assert_eq!(tables["campaigns"].len(), 2);
    assert_eq!(tables["companies"].len(), 2);
    assert_eq!(line("scores", "\"company\""), ["kind", "id", "project_id", "score", "fit", "intent", "reasons", "scored_at"]);

    // Nothing of the other organization, and nothing secret.
    assert!(!everything.contains("outsider"), "another organization's rows were exported");
    let unsubscribe: String = sqlx::query_scalar("select unsubscribe_token from leads where email = 'jo@brightline.example'").fetch_one(&s.db).await.unwrap();
    for secret in [unsubscribe.as_str(), "open-token-brightline", PASSWORD_HASH, SECRET, "api-key-hash-sentinel", "password", "_enc", "token"] {
        assert!(!everything.contains(secret), "{secret} was exported");
    }

    // The day is done, so the next ticks leave it.
    let requests = s.fake.requests().len();
    tick(&s.state, at(today, QUIET_HOUR + 2)).await.unwrap();
    tick(&s.state, at(today, 23)).await.unwrap();
    assert_eq!((s.runs().await.len(), s.fake.requests().len()), (1, requests), "a day already exported is skipped");

    // Export now runs anyway, and writes bodies once they're asked for.
    let mut bodies = settings(&["emails", "replies"]);
    bodies["include_bodies"] = json!(true);
    s.save(bodies).await;
    let (status, started) = s.call("acme", reqwest::Method::POST, "/organization/exports/run", None).await;
    assert_eq!((status, started["status"].as_str(), started["trigger"].as_str()), (202, Some("running"), Some("manual")));
    let manual = s.settled().await;
    assert_eq!((manual.status.as_str(), manual.files.as_array().unwrap().len()), ("done", 2));
    let rerun = &s.fake.requests()[requests..];
    let manifest_path = format!("/{}", manifest_key("splee/raw", today).replace('=', "%3D"));
    assert_eq!((&rerun[0].method, rerun[0].path.as_str()), (&reqwest::Method::DELETE, manifest_path.as_str()), "the old manifest goes first");
    assert_eq!(rerun.last().unwrap().path, manifest_path);
    let text = gunzip(&s.fake.object("warehouse", &file_key("splee/raw", "emails", today)).unwrap());
    assert!(text.contains("Hi Jo, brightline body text") && !text.contains("outsider"), "{text}");

    let srv = &s;
    let api = move |key: &'static str| async move {
        let res = srv.client.get(format!("{}/v1/exports", srv.base)).bearer_auth(key).send().await.unwrap();
        res.json::<Value>().await.unwrap()
    };
    let listed = api(API_KEY).await;
    // Sorted, since the daily run's made-up start can be later than the manual one's real start (see settled).
    let mut triggers = listed.as_array().unwrap().iter().map(|r| r["trigger"].as_str().unwrap()).collect::<Vec<_>>();
    triggers.sort();
    assert_eq!(triggers, ["daily", "manual"]);
    assert!(!listed.to_string().contains(SECRET));
    assert_eq!(api(OTHER_KEY).await, json!([]));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_team_chat_hears_when_an_export_fails_twice_in_a_row(db: PgPool) {
    let s = Server::start(db).await;
    s.seed(s.acme, s.projects[0], "brightline").await;
    s.save(settings(&["leads", "companies"])).await;
    // Both projects post to one chat, and the second to another as well. Each chat hears once.
    for (project, hook) in [(s.projects[0], "team"), (s.projects[1], "team"), (s.projects[1], "sales")] {
        sqlx::query("insert into chat_channels (project_id, url) values ($1, $2)").bind(project).bind(format!("{}/{hook}", s.chat_url)).execute(&s.db).await.unwrap();
    }
    s.fake.world.lock().unwrap().broken = Some("companies".into());

    let today = Utc::now().date_naive();
    let srv = &s;
    let run_at = move |hour: u32| async move {
        let started = start(&srv.db, srv.acme, "manual", at(today, hour)).await.unwrap();
        finish(&srv.state, srv.acme, started).await;
    };
    run_at(1).await;
    let failed = &s.runs().await[0];
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.error.as_deref(), Some("Companies: The storage service answered 500: We encountered an internal error. Please try again."));
    assert!(s.chats.lock().unwrap().is_empty(), "one failure is no alert");

    run_at(2).await;
    let chats = s.chats.lock().unwrap().clone();
    assert_eq!(chats.len(), 2, "{chats:?}");
    assert!(chats[0].to_string().contains("failed twice in a row") && chats[0].to_string().contains("internal error"), "{}", chats[0]);
    let failed_alerts: i64 = sqlx::query_scalar("select count(*) from chat_channels where error is not null").fetch_one(&s.db).await.unwrap();
    assert_eq!(failed_alerts, 0);

    run_at(3).await;
    assert_eq!(s.chats.lock().unwrap().len(), 2, "a bucket that stays broken alerts once");

    s.fake.world.lock().unwrap().broken = None;
    run_at(4).await;
    assert_eq!(s.runs().await[0].status, "done");

    // A daily run waits an hour after a failure, and stops for the day after DAILY_TRIES.
    s.fake.world.lock().unwrap().broken = Some("leads".into());
    let tomorrow = today.succ_opt().unwrap();
    for hour in [QUIET_HOUR, QUIET_HOUR, QUIET_HOUR + 1, QUIET_HOUR + 2, QUIET_HOUR + 3, QUIET_HOUR + 4] {
        tick(&s.state, at(tomorrow, hour)).await.unwrap();
    }
    let daily: Vec<Run> = s.runs().await.into_iter().filter(|r| r.dt == tomorrow).collect();
    assert_eq!(daily.len() as i64, DAILY_TRIES);
    assert_eq!(s.chats.lock().unwrap().len(), 4, "the second failure after a success alerts again");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_second_backend_neither_starts_nor_closes_a_running_export(db: PgPool) {
    let s = Server::start(db).await;
    s.save(settings(&["leads"])).await;
    let today = Utc::now().date_naive();
    let running = start(&s.db, s.acme, "manual", at(today, 1)).await.unwrap();
    // Another backend's worker and Export now, while this one still holds the organization's lock.
    assert_eq!(start(&s.db, s.acme, "daily", at(today, QUIET_HOUR + 1)).await.err(), Some(StartError::Running));
    assert_eq!(start(&s.db, s.acme, "manual", at(today, 2)).await.err(), Some(StartError::Running));
    tick(&s.state, at(today, QUIET_HOUR + 1)).await.unwrap();
    assert_eq!(s.runs().await.iter().map(|r| r.status.as_str()).collect::<Vec<_>>(), ["running"]);
    finish(&s.state, s.acme, running).await;
    assert_eq!(s.runs().await[0].status, "done");
    // Its daily run then finds the day done.
    assert_eq!(start(&s.db, s.acme, "daily", at(today, QUIET_HOUR + 1)).await.err(), Some(StartError::NotDue));

    // A run whose backend died holds no lock, so the next start closes it.
    sqlx::query("insert into export_runs (org_id, dt, trigger, started_at) values ($1, $2, 'manual', $3)")
        .bind(s.acme)
        .bind(today)
        .bind(at(today, 1) + chrono::Duration::minutes(30))
        .execute(&s.db).await.unwrap();
    let next = start(&s.db, s.acme, "manual", at(today, 2)).await.unwrap();
    let runs = s.runs().await;
    assert_eq!(runs.iter().map(|r| r.status.as_str()).collect::<Vec<_>>(), ["running", "failed", "done"]);
    assert_eq!(runs[1].error.as_deref(), Some(RESTARTED));
    finish(&s.state, s.acme, next).await;
}
