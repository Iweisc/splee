use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::*;

/// Unipile's profile and company pages as its docs describe them. Anyone not set up isn't found, and "down" is Unipile failing.
#[derive(Clone, Default)]
struct Fake {
    profiles: Arc<Mutex<HashMap<String, Value>>>,
    websites: Arc<Mutex<HashMap<String, String>>>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    /// Their positions, the current one first, as (company, LinkedIn company id, title).
    fn person(&self, slug: &str, current: (&str, &str, &str)) {
        let (company, company_id, position) = current;
        let profile = json!({
            "provider_id": format!("P-{slug}"), "public_identifier": slug, "first_name": "Dana", "last_name": "Reyes",
            "work_experience": [
                { "company": company, "company_id": company_id, "position": position, "start": "9/2026", "end": null },
                { "company": "Old Place", "company_id": "1", "position": "Analyst", "start": "1/2015", "end": "1/2019" },
            ],
        });
        self.profiles.lock().unwrap().insert(slug.into(), profile);
    }

    fn company(&self, id: &str, website: &str) {
        self.websites.lock().unwrap().insert(id.into(), website.into());
    }

    fn calls(&self) -> Vec<String> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

async fn serve(fake: Fake) -> String {
    fn not_found(what: &str) -> (StatusCode, Json<Value>) {
        (StatusCode::NOT_FOUND, Json(json!({ "status": 404, "type": "errors/resource_not_found", "title": format!("{what} not found") })))
    }
    async fn user(State(f): State<Fake>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
        assert_eq!((headers["x-api-key"].to_str().unwrap(), q["account_id"].as_str()), ("TOKEN", "ACC"));
        f.calls.lock().unwrap().push(format!("user {id}"));
        if id == "down" {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": 503, "type": "errors/provider_error", "title": "Provider error" })));
        }
        match f.profiles.lock().unwrap().get(&id) {
            Some(p) => (StatusCode::OK, Json(p.clone())),
            None => not_found("User"),
        }
    }
    async fn company(State(f): State<Fake>, Path(id): Path<String>) -> (StatusCode, Json<Value>) {
        f.calls.lock().unwrap().push(format!("company {id}"));
        match f.websites.lock().unwrap().get(&id) {
            Some(site) => (StatusCode::OK, Json(json!({ "object": "CompanyProfile", "id": id, "website": site }))),
            None => not_found("Company"),
        }
    }
    let app = Router::new()
        .route("/api/v1/users/{id}", get(user))
        .route("/api/v1/linkedin/company/{id}", get(company))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

struct Setup {
    state: AppState,
    org: Uuid,
    project: Uuid,
    fake: Fake,
}

/// An organization whose project has LinkedIn connected through the fake, on the account ACC.
async fn setup(db: &PgPool) -> Setup {
    let fake = Fake::default();
    let base = serve(fake.clone()).await;
    let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(db).await.unwrap();
    let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(db).await.unwrap();
    sqlx::query("insert into project_settings (project_id, unipile_dsn, unipile_token_enc, unipile_account_id) values ($1, $2, $3, 'ACC')")
        .bind(project)
        .bind(base)
        .bind(crate::crypto::encrypt("", "TOKEN"))
        .execute(db)
        .await
        .unwrap();
    Setup { state: crate::autopilot::tests::state(db.clone()), org, project, fake }
}

/// Someone on the watch list, last seen at `company`.
async fn watch(db: &PgPool, project: Uuid, slug: &str, company: (&str, Option<&str>, &str), lead: Option<Uuid>) -> Uuid {
    sqlx::query_scalar(
        "insert into watched_people (project_id, lead_id, relationship, full_name, linkedin_url, linkedin_key, company_name, company_domain, company_linkedin_id)
         values ($1, $2, 'won', 'Dana Reyes', 'https://www.linkedin.com/in/' || $3 || '/', $3, $4, $5, $6) returning id",
    )
    .bind(project)
    .bind(lead)
    .bind(slug)
    .bind(company.0)
    .bind(company.1)
    .bind(company.2)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn watched(db: &PgPool, id: Uuid) -> Watched {
    sqlx::query_as("select * from watched_people where id = $1").bind(id).fetch_one(db).await.unwrap()
}

async fn moves(db: &PgPool, project: Uuid) -> Vec<(String, String, Option<String>, String)> {
    sqlx::query_as("select old_company, new_company, new_domain::text, new_title from job_changes where project_id = $1 order by seen_at, new_company")
        .bind(project)
        .fetch_all(db)
        .await
        .unwrap()
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn leads_that_bought_booked_or_replied_hot_are_watched_once(db: PgPool) {
    let s = setup(&db).await;
    crate::deals::ensure_stages(&db, s.org).await.unwrap();
    let lead = |name: &'static str, status: &'static str| {
        sqlx::query_scalar::<_, Uuid>(
            "insert into leads (project_id, full_name, email, status, company_name, company_domain, linkedin_url)
             values ($1, $2, lower($2) || '@acme.example', $3, 'Acme', 'acme.example', 'https://www.linkedin.com/in/' || lower($2) || '?trk=x')
             returning id",
        )
        .bind(s.project)
        .bind(name)
        .bind(status)
        .fetch_one(&db)
    };
    let won = lead("Wes", "replied").await.unwrap();
    let booked = lead("Bea", "meeting_booked").await.unwrap();
    let hot = lead("Hal", "hot").await.unwrap();
    let replied_hot = lead("Ria", "contacted").await.unwrap();
    let cold = lead("Cal", "contacted").await.unwrap();
    sqlx::query(
        "insert into deals (org_id, project_id, lead_id, stage_id, status, closed_at)
         select $1, $2, $3, id, 'won', now() from deal_stages where org_id = $1 and kind = 'won'",
    )
    .bind(s.org)
    .bind(s.project)
    .bind(won)
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(
        "insert into email_messages (project_id, lead_id, direction, kind, status, classification)
         values ($1, $2, 'inbound', 'inbound', 'received', 'hot')",
    )
        .bind(s.project)
        .bind(replied_hot)
        .execute(&db)
        .await
        .unwrap();
    // A demo project has no organization and no watch list.
    let demo: Uuid = sqlx::query_scalar("insert into projects default values returning id").fetch_one(&db).await.unwrap();
    sqlx::query("insert into leads (project_id, full_name, status) values ($1, 'Demo', 'hot')").bind(demo).execute(&db).await.unwrap();

    tick(&s.state).await.unwrap();
    tick(&s.state).await.unwrap();
    let list: Vec<(Uuid, String, String, String)> =
        sqlx::query_as("select lead_id, relationship, linkedin_key, company_name from watched_people order by full_name").fetch_all(&db).await.unwrap();
    let ids: Vec<(Uuid, &str)> = list.iter().map(|(id, r, ..)| (*id, r.as_str())).collect();
    assert_eq!(ids, [(booked, "booked"), (hot, "hot"), (replied_hot, "hot"), (won, "won")]);
    assert_eq!((list[0].2.as_str(), list[0].3.as_str()), ("bea", "Acme"));
    assert!(!list.iter().any(|(id, ..)| *id == cold));

    // A hot lead who books moves up, and is still one entry.
    sqlx::query("update leads set status = 'meeting_booked' where id = $1").bind(hot).execute(&db).await.unwrap();
    enroll(&db, s.project).await.unwrap();
    let hal: Vec<String> = sqlx::query_scalar("select relationship from watched_people where lead_id = $1").bind(hot).fetch_all(&db).await.unwrap();
    assert_eq!(hal, ["booked"]);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_csv_of_past_customers_is_added_once(db: PgPool) {
    let s = setup(&db).await;
    sqlx::query("insert into watched_people (project_id, relationship, full_name, email) values ($1, 'hot', 'Ana Diaz', 'ana@acme.example')")
        .bind(s.project)
        .execute(&db)
        .await
        .unwrap();
    let csv = "\u{feff}Name,Email,LinkedIn URL,Company,Job Title\n\
               Ana Diaz,ANA@acme.example,https://www.linkedin.com/in/ana-diaz/?trk=x,Acme,COO\n\
               Bo Chen,bo@bright.example,https://linkedin.com/in/bo-chen,Brightline,VP Sales\n\
               Bo Chen again,,https://www.linkedin.com/in/Bo-Chen/,Brightline,\n\
               Cy Doe,not-an-email,,Cyco,\n\
               Di No Link,,,Dico,\n\
               Ed Park,,https://www.linkedin.com/company/edco,Edco,\n\
               Fay Lin,fay@fay.example,,Fayco,Owner\n";
    let body = AddBody { relationship: "won".into(), people: vec![], csv: Some(csv.into()) };
    let added = add(&db, s.project, body).await.unwrap();
    assert_eq!((added.added, added.updated, added.skipped), (2, 1, 1));
    let errors: Vec<(usize, &str)> = added.errors.iter().map(|e| (e.row, e.error.as_str())).collect();
    assert_eq!(errors, [
        (3, "Listed twice"),
        (4, "Invalid email: not-an-email"),
        (5, "Needs an email or a LinkedIn link"),
        (6, "The LinkedIn link isn't a profile link"),
    ]);
    let list: Vec<(String, Option<String>, String, String, String, String)> = sqlx::query_as(
        "select full_name, email::text, linkedin_url, linkedin_key, relationship, title from watched_people where project_id = $1 order by full_name",
    )
    .bind(s.project)
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(list, [
        // Already watched: the link and title fill in, and buying ranks above a hot reply.
        ("Ana Diaz".into(), Some("ana@acme.example".into()), "https://www.linkedin.com/in/ana-diaz/".into(), "ana-diaz".into(), "won".into(), "COO".into()),
        ("Bo Chen".into(), Some("bo@bright.example".into()), "https://linkedin.com/in/bo-chen".into(), "bo-chen".into(), "won".into(), "VP Sales".into()),
        ("Fay Lin".into(), Some("fay@fay.example".into()), String::new(), String::new(), "won".into(), "Owner".into()),
    ]);

    // The same list again changes nothing, and a list without an email or LinkedIn column is turned down.
    let again = add(&db, s.project, AddBody { relationship: "won".into(), people: vec![], csv: Some(csv.into()) }).await.unwrap();
    assert_eq!((again.added, again.updated), (0, 0));
    let bad = add(&db, s.project, AddBody { relationship: "won".into(), people: vec![], csv: Some("Name,Company\nAna,Acme\n".into()) }).await;
    assert!(matches!(bad, Err(AppError::BadRequest(m)) if m.contains("email or a LinkedIn URL column")));
    let people = vec![Person { email: "x@y.example".into(), ..Person::default() }];
    let wrong = add(&db, s.project, AddBody { relationship: "friend".into(), people, csv: None }).await;
    assert!(matches!(wrong, Err(AppError::BadRequest(_))));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_move_makes_a_lead_at_the_new_company_and_a_rename_does_not(db: PgPool) {
    let s = setup(&db).await;
    let owner: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('owner@acme.example', 'x') returning id").fetch_one(&db).await.unwrap();
    sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(s.org).bind(owner).execute(&db).await.unwrap();
    let old_lead: Uuid = sqlx::query_scalar(
        "insert into leads (project_id, full_name, email, company_name, company_domain, owner_id, status)
         values ($1, 'Dana Reyes', 'dana@acme.example', 'Acme', 'acme.example', $2, 'meeting_booked') returning id",
    )
    .bind(s.project)
    .bind(owner)
    .fetch_one(&db)
    .await
    .unwrap();
    // Dana moved to Brightline. Sam's company renamed itself and kept its page. Lee's had no id yet, but its new page has the
    // old website. Ray is between jobs, and Gone's profile was deleted.
    let dana = watch(&db, s.project, "dana-reyes", ("Acme", Some("acme.example"), "100"), Some(old_lead)).await;
    let sam = watch(&db, s.project, "sam-lee", ("Acme", None, "100"), None).await;
    let lee = watch(&db, s.project, "lee-wu", ("Smile Dental", Some("smile.example"), ""), None).await;
    let ray = watch(&db, s.project, "ray-ng", ("Acme", None, ""), None).await;
    let gone = watch(&db, s.project, "gone", ("Acme", None, ""), None).await;
    s.fake.person("dana-reyes", ("Brightline", "200", "VP Sales"));
    s.fake.company("200", "https://www.brightline.example/about");
    s.fake.person("sam-lee", ("Acme Group", "100", "CFO"));
    s.fake.person("lee-wu", ("Smile Dental Group", "300", "Owner"));
    s.fake.company("300", "smile.example");
    s.fake.profiles.lock().unwrap().insert("ray-ng".into(), json!({ "provider_id": "P-RAY", "work_experience": [
        { "company": "Acme", "company_id": "100", "position": "AE", "start": "1/2020", "end": "5/2026" }] }));

    tick(&s.state).await.unwrap();
    assert_eq!(moves(&db, s.project).await, [("Acme".into(), "Brightline".into(), Some("brightline.example".into()), "VP Sales".into())]);
    let mut calls = s.fake.calls();
    calls.sort();
    assert_eq!(calls, ["company 200", "company 300", "user dana-reyes", "user gone", "user lee-wu", "user ray-ng", "user sam-lee"]);

    let (sam, lee, gone, dana) = (watched(&db, sam).await, watched(&db, lee).await, watched(&db, gone).await, watched(&db, dana).await);
    assert_eq!((sam.company_name.as_str(), sam.title.as_str()), ("Acme Group", "CFO"));
    assert_eq!((lee.company_name.as_str(), lee.company_linkedin_id.as_str()), ("Smile Dental Group", "300"));
    assert_eq!((gone.check_error.as_deref(), gone.checked_at.is_some()), (Some("User not found"), true));
    assert_eq!((dana.company_name.as_str(), dana.company_domain.as_deref(), dana.company_linkedin_id.as_str()), ("Brightline", Some("brightline.example"), "200"));
    assert_eq!(watched(&db, ray).await.company_name, "Acme");

    // The new lead is at Brightline, with no email yet, the old lead's owner, and the move as its strongest reason.
    let lead: crate::models::Lead =
        sqlx::query_as("select l.* from leads l join job_changes j on j.new_lead_id = l.id").fetch_one(&db).await.unwrap();
    assert_eq!(
        (lead.full_name.as_str(), lead.first_name.as_str(), lead.title.as_str(), lead.company_name.as_str(), lead.company_domain.as_deref()),
        ("Dana Reyes", "Dana", "VP Sales", "Brightline", Some("brightline.example"))
    );
    assert_eq!((lead.email.as_deref(), lead.owner_id, lead.source.as_str(), lead.segment_id), (None, Some(owner), "linkedin", None));
    assert_eq!(lead.linkedin_url, "https://www.linkedin.com/in/dana-reyes/");
    let company: (Uuid, String) = sqlx::query_as("select id, name from companies where domain = 'brightline.example'").fetch_one(&db).await.unwrap();
    assert_eq!((lead.company_id, company.1.as_str()), (Some(company.0), "Brightline"));
    assert_eq!(lead.score_reasons.0[0].text, "Moved from Acme, where they bought");
    assert!(lead.score_intent > 0);
}

/// Lets the next check read everyone again, on a fresh day of reads.
async fn a_month_later(db: &PgPool) {
    sqlx::query("update watched_people set checked_at = now() - interval '31 days'").execute(db).await.unwrap();
    sqlx::query("update linkedin_profile_reads set day = day - 1").execute(db).await.unwrap();
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn going_back_and_forth_between_two_companies_is_a_move_each_time(db: PgPool) {
    let s = setup(&db).await;
    let dana = watch(&db, s.project, "dana-reyes", ("Acme", Some("acme.example"), "100"), None).await;
    s.fake.company("100", "acme.example");
    s.fake.company("200", "brightline.example");
    let mut changes = vec![];
    for (company, id) in [("Brightline", "200"), ("Acme", "100"), ("Brightline", "200")] {
        s.fake.person("dana-reyes", (company, id, "VP Sales"));
        tick(&s.state).await.unwrap();
        let change: Uuid = sqlx::query_scalar("select id from job_changes order by move_number desc limit 1").fetch_one(&db).await.unwrap();
        changes.push(change);
        a_month_later(&db).await;
    }
    assert_eq!(moves(&db, s.project).await.iter().map(|m| (m.0.as_str(), m.1.as_str())).collect::<Vec<_>>(), [
        ("Acme", "Brightline"),
        ("Brightline", "Acme"),
        ("Acme", "Brightline"),
    ]);
    assert_eq!(watched(&db, dana).await.moves, 3);

    // Both moves to Brightline lead to the one lead there, and each is announced.
    let leads: Vec<Option<Uuid>> =
        sqlx::query_scalar("select new_lead_id from job_changes where new_domain = 'brightline.example' order by move_number").fetch_all(&db).await.unwrap();
    assert!(leads[0].is_some());
    assert_eq!(leads[0], leads[1]);
    let there: i64 = sqlx::query_scalar("select count(*) from leads where company_domain = 'brightline.example'").fetch_one(&db).await.unwrap();
    assert_eq!(there, 1);
    for change in [changes[0], changes[2]] {
        let a = announcement(&db, "https://app.example", change).await.unwrap().unwrap();
        assert_eq!(a.subject, "Job change: Dana Reyes is now at Brightline");
    }
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_move_found_on_the_days_last_read_is_finished_the_next_day_without_reading_the_profile_again(db: PgPool) {
    let s = setup(&db).await;
    let dana = watch(&db, s.project, "dana-reyes", ("Acme", Some("acme.example"), "100"), None).await;
    s.fake.person("dana-reyes", ("Brightline", "200", "VP Sales"));
    s.fake.company("200", "brightline.example");
    sqlx::query("insert into linkedin_profile_reads (account_id, day, reads) values ('ACC', (now() at time zone 'utc')::date, $1)")
        .bind(DAILY_READS - 1)
        .execute(&db)
        .await
        .unwrap();

    tick(&s.state).await.unwrap();
    assert_eq!(s.fake.calls(), ["user dana-reyes"]);
    assert!(moves(&db, s.project).await.is_empty());
    let w = watched(&db, dana).await;
    assert!(w.checked_at.is_some() && w.pending_profile.is_some());
    tick(&s.state).await.unwrap();
    assert!(s.fake.calls().is_empty(), "the day's reads are spent");

    sqlx::query("update linkedin_profile_reads set day = day - 1").execute(&db).await.unwrap();
    tick(&s.state).await.unwrap();
    assert_eq!(s.fake.calls(), ["company 200"]);
    assert_eq!(moves(&db, s.project).await, [("Acme".into(), "Brightline".into(), Some("brightline.example".into()), "VP Sales".into())]);
    assert!(watched(&db, dana).await.pending_profile.is_none());
    tick(&s.state).await.unwrap();
    assert!(s.fake.calls().is_empty(), "read this month already");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_new_company_page_on_the_same_website_is_not_a_move(db: PgPool) {
    let s = setup(&db).await;
    let sam = watch(&db, s.project, "sam-lee", ("Acme", Some("acme.example"), "100"), None).await;
    s.fake.person("sam-lee", ("Acme Holdings", "999", "CFO"));
    s.fake.company("999", "https://www.acme.example/");

    tick(&s.state).await.unwrap();
    assert!(moves(&db, s.project).await.is_empty());
    let sam = watched(&db, sam).await;
    assert_eq!((sam.company_name.as_str(), sam.company_linkedin_id.as_str(), sam.moves), ("Acme Holdings", "999", 0));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn profiles_are_read_every_30_days_within_the_accounts_daily_budget(db: PgPool) {
    let s = setup(&db).await;
    for slug in ["a", "b", "c", "d"] {
        watch(&db, s.project, slug, ("Acme", None, ""), None).await;
        s.fake.person(slug, ("Acme", "100", "AE"));
    }
    // A second project on the same LinkedIn account shares its day.
    let other: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Other') returning id").bind(s.org).fetch_one(&db).await.unwrap();
    let dsn: String = sqlx::query_scalar("select unipile_dsn from project_settings where project_id = $1").bind(s.project).fetch_one(&db).await.unwrap();
    sqlx::query("insert into project_settings (project_id, unipile_dsn, unipile_token_enc, unipile_account_id) values ($1, $2, $3, 'ACC')")
        .bind(other)
        .bind(dsn)
        .bind(crate::crypto::encrypt("", "TOKEN"))
        .execute(&db)
        .await
        .unwrap();
    watch(&db, other, "e", ("Acme", None, ""), None).await;
    s.fake.person("e", ("Acme", "100", "AE"));
    sqlx::query("insert into linkedin_profile_reads (account_id, day, reads) values ('ACC', (now() at time zone 'utc')::date, $1)")
        .bind(DAILY_READS - 2)
        .execute(&db)
        .await
        .unwrap();
    let checked = || sqlx::query_scalar::<_, i64>("select count(*) from watched_people where checked_at is not null").fetch_one(&db);

    tick(&s.state).await.unwrap();
    assert_eq!((s.fake.calls().len(), checked().await.unwrap()), (2, 2));
    tick(&s.state).await.unwrap();
    assert!(s.fake.calls().is_empty(), "the day's reads are spent");

    // The next day the rest are read, and nobody is read again within 30 days.
    sqlx::query("update linkedin_profile_reads set day = day - 1").execute(&db).await.unwrap();
    tick(&s.state).await.unwrap();
    assert_eq!((s.fake.calls().len(), checked().await.unwrap()), (3, 5));
    sqlx::query("update linkedin_profile_reads set day = day - 1").execute(&db).await.unwrap();
    sqlx::query("update watched_people set checked_at = now() - interval '29 days'").execute(&db).await.unwrap();
    tick(&s.state).await.unwrap();
    assert!(s.fake.calls().is_empty());
    sqlx::query("update watched_people set checked_at = now() - interval '31 days' where linkedin_key = 'a'").execute(&db).await.unwrap();
    tick(&s.state).await.unwrap();
    assert_eq!(s.fake.calls(), ["user a"]);

    // Unipile failing holds the account for an hour, like sending, and the person isn't marked read.
    sqlx::query("update watched_people set checked_at = null, linkedin_key = 'down', linkedin_url = 'https://www.linkedin.com/in/down' where linkedin_key = 'b'")
        .execute(&db)
        .await
        .unwrap();
    tick(&s.state).await.unwrap();
    assert_eq!(s.fake.calls(), ["user down"]);
    let failed: Option<String> =
        sqlx::query_scalar("select unipile_error from project_settings where project_id = $1").bind(s.project).fetch_one(&db).await.unwrap();
    assert_eq!(failed.as_deref(), Some("Provider error"));
    tick(&s.state).await.unwrap();
    assert!(s.fake.calls().is_empty());
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn two_workers_seeing_one_move_make_one_lead(db: PgPool) {
    let s = setup(&db).await;
    let id = watch(&db, s.project, "dana-reyes", ("Acme", Some("acme.example"), "100"), None).await;
    let w = watched(&db, id).await;
    let now = Position { company: "Brightline".into(), company_id: "200".into(), title: "VP Sales".into() };
    let profile = json!({ "first_name": "Dana", "last_name": "Reyes" });
    let (a, b) = tokio::join!(
        record_move(&db, &w, &now, Some("brightline.example"), &profile),
        record_move(&db, &w, &now, Some("brightline.example"), &profile)
    );
    assert_eq!([a.unwrap().is_some(), b.unwrap().is_some()].iter().filter(|new| **new).count(), 1);
    let counts: (i64, i64, i64) = sqlx::query_as(
        "select (select count(*) from job_changes), (select count(*) from leads where company_domain = 'brightline.example'),
                (select count(*) from companies where domain = 'brightline.example')",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(counts, (1, 1, 1));

    // A lead someone already added at the new company is used rather than a second one.
    let id = watch(&db, s.project, "bo-chen", ("Acme", None, "100"), None).await;
    let bo: Uuid = sqlx::query_scalar(
        "insert into leads (project_id, full_name, linkedin_url, company_name, company_domain)
         values ($1, 'Bo Chen', 'https://linkedin.com/in/Bo-Chen', 'Brightline', 'brightline.example') returning id",
    )
    .bind(s.project)
    .fetch_one(&db)
    .await
    .unwrap();
    let change = record_move(&db, &watched(&db, id).await, &now, Some("brightline.example"), &json!({})).await.unwrap().unwrap();
    let lead: Option<Uuid> = sqlx::query_scalar("select new_lead_id from job_changes where id = $1").bind(change).fetch_one(&db).await.unwrap();
    assert_eq!(lead, Some(bo));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_alert_goes_to_the_old_leads_owner_and_the_lead_joins_the_campaign(db: PgPool) {
    let s = setup(&db).await;
    let mut people = vec![];
    for email in ["ana@acme.example", "ben@acme.example"] {
        let id: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ($1, 'x') returning id").bind(email).fetch_one(&db).await.unwrap();
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(s.org).bind(id).execute(&db).await.unwrap();
        people.push(id);
    }
    let segment: Uuid =
        sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Old friends') returning id").bind(s.project).fetch_one(&db).await.unwrap();
    set_campaign(&db, s.project, Some(segment)).await.unwrap();
    let old: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, owner_id) values ($1, 'Dana Reyes', $2) returning id")
        .bind(s.project)
        .bind(people[1])
        .fetch_one(&db)
        .await
        .unwrap();
    let dana = watched(&db, watch(&db, s.project, "dana-reyes", ("Acme", None, "100"), Some(old)).await).await;
    let now = Position { company: "Brightline".into(), company_id: "200".into(), title: "VP Sales".into() };
    let change = record_move(&db, &dana, &now, Some("brightline.example"), &json!({})).await.unwrap().unwrap();

    let a = announcement(&db, "https://app.example", change).await.unwrap().unwrap();
    assert_eq!(a.recipients, ["ben@acme.example"]);
    assert_eq!(a.subject, "Job change: Dana Reyes is now at Brightline");
    assert_eq!(a.summary, "Dana Reyes moved from Acme to Brightline as VP Sales");
    assert_eq!(a.detail, "They bought from you at Acme. Splee made them a lead at Brightline and added them to Old friends.");
    let lead: (Uuid, Option<Uuid>) =
        sqlx::query_as("select l.id, l.segment_id from leads l join job_changes j on j.new_lead_id = l.id").fetch_one(&db).await.unwrap();
    assert_eq!(lead.1, Some(segment));
    assert_eq!(a.link, format!("https://app.example/app/p/{}/leads?lead={}", s.project, lead.0));

    // Someone added by hand has no lead or owner, so everyone hears about it.
    let fay = watched(&db, watch(&db, s.project, "fay-lin", ("Fayco", None, ""), None).await).await;
    let change = record_move(&db, &fay, &now, None, &json!({})).await.unwrap().unwrap();
    assert_eq!(announcement(&db, "https://app.example", change).await.unwrap().unwrap().recipients, ["ana@acme.example", "ben@acme.example"]);

    // Added to another campaign by hand, then dismissed: the lead keeps the campaign, and the move leaves the tab.
    let other: Uuid =
        sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Later') returning id").bind(s.project).fetch_one(&db).await.unwrap();
    add_to_campaign(&db, s.project, change, other).await.unwrap();
    let overview = overview(&db, s.project).await.unwrap();
    let shown: Vec<(&str, Option<&str>)> = overview.changes.iter().map(|c| (c.full_name.as_str(), c.segment_name.as_deref())).collect();
    assert_eq!(shown.len(), 2);
    assert!(shown.contains(&("Dana Reyes", Some("Old friends"))) && shown.contains(&("Dana Reyes", Some("Later"))), "{shown:?}");
    assert_eq!((overview.connected, overview.segment_id, overview.watching), (true, Some(segment), 2));
    dismiss(&db, s.project, change).await.unwrap();
    assert_eq!(super::overview(&db, s.project).await.unwrap().changes.len(), 1);
    assert!(matches!(dismiss(&db, s.project, change).await, Err(AppError::NotFound)));

    // Another organization's project can't touch it.
    let theirs = setup(&db).await;
    assert!(matches!(dismiss(&db, theirs.project, change).await, Err(AppError::NotFound)));
    assert!(matches!(add_to_campaign(&db, theirs.project, change, other).await, Err(AppError::NotFound)));
    assert!(super::overview(&db, theirs.project).await.unwrap().changes.is_empty());
}

#[test]
fn the_current_position_and_company_names_are_read_plainly() {
    let profile = json!({ "work_experience": [
        { "company": "Gone", "company_id": "1", "position": "AE", "end": "1/2024" },
        { "company": " Brightline ", "company_id": "200", "position": "VP Sales", "end": null },
    ] });
    assert_eq!(current_position(&profile), Some(Position { company: "Brightline".into(), company_id: "200".into(), title: "VP Sales".into() }));
    assert_eq!(current_position(&json!({ "work_experience": [] })), None);
    assert_eq!(company_key("Acme, Inc."), company_key("ACME"));
    assert_ne!(company_key("Acme Labs"), company_key("Acme"));
    assert_eq!(company_key("Co"), "co");
}
