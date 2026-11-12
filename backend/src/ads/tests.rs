use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use axum::Router;
use reqwest::Method;
use serde_json::json;

use super::fake::{Fake, GOOGLE_ACCOUNT, META_ACCOUNT, Seen};
use super::*;
use crate::mail::oauth;
use crate::routes::ads::UNKNOWN_STATE;

#[test]
fn addresses_and_numbers_are_normalized_then_hashed_with_sha256() {
    assert_eq!(sha256_hex("test@example.com"), "973dfe463ec85785f5f95af5ba3906eedb2d931c24e69824a89ea65dba4e813b", "the vector the platforms' docs use");
    assert_eq!(normalized_email("  Ana@Acme.TEST \n").as_deref(), Some("ana@acme.test"));
    for bad in ["", "ana", "ana@", "@acme.test", "ana smith@acme.test", "ana@localhost"] {
        assert_eq!(normalized_email(bad), None, "{bad:?}");
    }
    assert_eq!(e164("+44 (7700) 900-123").as_deref(), Some("+447700900123"));
    for bad in ["07700 900123", "+0447700900123", "+44", "+44 7700 abc"] {
        assert_eq!(e164(bad), None, "{bad:?}");
    }

    let id = Uuid::nil();
    let email = "5374360461a1c32b6eb1eb01b213b54be2bfbc7e0465051e2bdbea956052b063";
    let linkedin = member(Platform::LinkedIn, id, " Ana@Acme.TEST", "+44 7700 900123").unwrap();
    assert_eq!((linkedin.email_hash.as_str(), linkedin.phone_hash.as_str()), (email, ""), "LinkedIn only takes addresses");
    let google = member(Platform::Google, id, "ana@acme.test", "+44 7700 900123").unwrap();
    assert_eq!((google.email_hash.as_str(), google.phone_hash.as_str()), (email, "a8acc3a90a7b4e4dc65e93db9240ed26523050ef754d63b75b5161de76781436"), "+447700900123");
    let meta = member(Platform::Meta, id, "ANA@acme.test", "+447700900123").unwrap();
    assert_eq!((meta.email_hash.as_str(), meta.phone_hash.as_str()), (email, "033134b911b137918338415ee3d20a064b24773d36a3b02e8b99fdd3fcd6b4cd"), "447700900123");
    assert_eq!(member(Platform::Google, id, "ana@acme.test", "07700 900123").unwrap().phone_hash, "", "a number without its country code is left out");
    assert!(member(Platform::Meta, id, "not an address", "+447700900123").is_none());
}

#[test]
fn a_sync_only_sends_the_difference() {
    let m = |n: u128, email: &str, phone: &str| Member { lead_id: Uuid::from_u128(n), email_hash: email.into(), phone_hash: phone.into() };
    let sent = |member: Member, confirmed: bool| Sent { member, confirmed };
    let ledger = [sent(m(1, "a", ""), true), sent(m(2, "b", ""), true), sent(m(3, "c", "x"), true), sent(m(5, "e", ""), false), sent(m(6, "f", ""), false)];
    let wanted = [m(1, "a", ""), m(3, "c", "y"), m(4, "d", ""), m(6, "f", "")];
    let (remove, add) = diff(&ledger, &wanted);
    assert_eq!(remove, [m(2, "b", ""), m(3, "c", "x"), m(5, "e", "")], "who left, the old number of who changed it, and who may have gone in unconfirmed");
    assert_eq!(add, [m(3, "c", "y"), m(4, "d", ""), m(6, "f", "")], "who's new, and who went in unconfirmed");
    let confirmed: Vec<Sent> = wanted.iter().map(|w| sent(w.clone(), true)).collect();
    assert_eq!(diff(&confirmed, &wanted), (vec![], vec![]));
    assert_eq!(audience_name(Some("  Spring outreach ")), "Splee: Spring outreach");
    assert_eq!(audience_name(None), "Splee: Hot leads");
}

struct Server {
    base: String,
    client: reqwest::Client,
    db: PgPool,
    fake: Fake,
    state: AppState,
    /// Ana's project in Acme, with two campaigns. Beta is her other organization, and Cy's org has its own project and campaign.
    acme: Uuid,
    campaign: Uuid,
    second: Uuid,
    theirs: Uuid,
    their_campaign: Uuid,
}

impl Server {
    async fn start(db: PgPool) -> Self {
        let fake = Fake::start().await;
        let state = AppState { db: db.clone(), cfg: Arc::new(fake.cfg()), events: tokio::sync::broadcast::channel(1).0 };
        let mut projects = vec![];
        for (email, orgs) in [("ana@acme.test", vec!["acme", "beta"]), ("cy@other.test", vec!["other"])] {
            let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, 'x', 'x') returning id").bind(email).fetch_one(&db).await.unwrap();
            for name in orgs {
                let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap();
                sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(user).execute(&db).await.unwrap();
                sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                    .bind(sha256_hex(&format!("session-{name}")))
                    .bind(user)
                    .bind(org)
                    .execute(&db)
                    .await
                    .unwrap();
                let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(&db).await.unwrap();
                projects.push(project);
            }
        }
        let segment = |project: Uuid, name: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into segments (project_id, name) values ($1, $2) returning id").bind(project).bind(name).fetch_one(&db)
        };
        let (campaign, second, their_campaign) =
            (segment(projects[0], "Spring outreach").await.unwrap(), segment(projects[0], "Second").await.unwrap(), segment(projects[2], "Cy's campaign").await.unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let router = Router::new().nest("/api", crate::routes::router()).with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, router).await });
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
        Self { base, client, db, fake, state, acme: projects[0], campaign, second, theirs: projects[2], their_campaign }
    }

    fn call(&self, method: Method, org: &str, path: &str) -> reqwest::RequestBuilder {
        self.client.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session=session-{org}"))
    }

    /// The sign-in page's query for a sign-in started by `org` in `project`.
    async fn authorize(&self, org: &str, project: Uuid, platform: &str) -> HashMap<String, String> {
        let res = self.call(Method::POST, org, &format!("/projects/{project}/ads/{platform}/oauth")).send().await.unwrap();
        assert_eq!(res.status(), 200);
        let body: Value = res.json().await.unwrap();
        Url::parse(body["url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect()
    }

    /// Where the callback sent the browser, and its query.
    async fn callback(&self, org: &str, params: &[(&str, &str)]) -> (String, HashMap<String, String>) {
        let url = Url::parse_with_params(&format!("{}/ads/oauth/callback", self.base), params).unwrap();
        let res = self.client.get(url).header("cookie", format!("splee_session=session-{org}")).send().await.unwrap();
        assert_eq!(res.status(), 303);
        let location = res.headers()["location"].to_str().unwrap().to_string();
        let url = Url::parse(&format!("http://x{location}")).unwrap();
        (url.path().to_string(), url.query_pairs().into_owned().collect())
    }

    async fn sign_in_as(&self, org: &str, project: Uuid, platform: &str) {
        let q = self.authorize(org, project, platform).await;
        let code = if platform == "meta" { "the-code".to_string() } else { format!("code-{platform}-{org}") };
        let (_, back) = self.callback(org, &[("code", &code), ("state", &q["state"])]).await;
        assert_eq!(back.get("ads").map(String::as_str), Some(platform), "{back:?}");
    }

    /// Signs Acme in to all three and picks LinkedIn's first account, which is the only one not picked for it.
    async fn connect_all(&self) {
        for platform in ["linkedin", "google", "meta"] {
            self.sign_in_as("acme", self.acme, platform).await;
        }
        let res = self.call(Method::PUT, "acme", &format!("/projects/{}/ads/linkedin/account", self.acme)).json(&json!({"account_id": "5001"})).send().await.unwrap();
        assert_eq!(res.status(), 200);
    }

    async fn status(&self) -> Value {
        self.call(Method::GET, "acme", &format!("/projects/{}/ads", self.acme)).send().await.unwrap().json().await.unwrap()
    }

    async fn toggle(&self, org: &str, segment: Uuid, platform: &str, on: bool) -> reqwest::Response {
        self.call(Method::PUT, org, &format!("/segments/{segment}/audiences/{platform}")).json(&json!({"on": on})).send().await.unwrap()
    }

    async fn turn_on_all(&self, segment: Uuid) -> Value {
        let mut last = Value::Null;
        for platform in ["linkedin", "google", "meta"] {
            let res = self.toggle("acme", segment, platform, true).await;
            assert_eq!(res.status(), 200, "{platform}");
            last = res.json().await.unwrap();
        }
        last
    }

    async fn lead(&self, project: Uuid, segment: Uuid, email: &str, status: &str, phone: &str) -> Uuid {
        sqlx::query_scalar("insert into leads (project_id, segment_id, email, status, phone) values ($1, $2, $3, $4, $5) returning id")
            .bind(project)
            .bind(segment)
            .bind(email)
            .bind(status)
            .bind(phone)
            .fetch_one(&self.db)
            .await
            .unwrap()
    }

    async fn suppress(&self, project: Uuid, value: &str, kind: &str) {
        sqlx::query("insert into suppressions (org_id, value, kind, reason) select org_id, $2, $3, 'manual' from projects where id = $1")
            .bind(project)
            .bind(value)
            .bind(kind)
            .execute(&self.db)
            .await
            .unwrap();
    }

    /// Makes every audience due, as an hour later.
    async fn tick(&self) -> usize {
        sqlx::query("update ad_audiences set tried_at = now() - interval '2 hours'").execute(&self.db).await.unwrap();
        tick(&self.state).await.unwrap()
    }

    async fn connection(&self, project: Uuid, platform: &str) -> (bool, Option<String>, Option<Ts>) {
        sqlx::query_as("select needs_reconnect, error, held_until from ad_connections where project_id = $1 and platform = $2")
            .bind(project)
            .bind(platform)
            .fetch_one(&self.db)
            .await
            .unwrap()
    }
}

fn hashed(emails: &[&str]) -> BTreeSet<String> {
    emails.iter().map(|e| sha256_hex(e)).collect()
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn signing_in_lists_the_ad_accounts_keeps_the_tokens_encrypted_and_picks_one(db: PgPool) {
    let s = Server::start(db).await;
    let linkedin = s.authorize("acme", s.acme, "linkedin").await;
    assert_eq!((linkedin["scope"].as_str(), linkedin["redirect_uri"].as_str()), ("r_ads rw_dmp_segments", "http://localhost:3404/api/ads/oauth/callback"));
    assert!(!linkedin.contains_key("code_challenge"));
    let back = s.callback("acme", &[("code", "li-code"), ("state", &linkedin["state"])]).await;
    assert_eq!(back, (format!("/app/p/{}/integrations", s.acme), HashMap::from([("ads".to_string(), "linkedin".to_string())])));
    let form = s.fake.token_requests()[0].body.clone();
    assert_eq!((form["grant_type"].as_str(), form["code"].as_str(), form["client_secret"].as_str()), (Some("authorization_code"), Some("li-code"), Some("shh")));
    assert_eq!(form["redirect_uri"], "http://localhost:3404/api/ads/oauth/callback");
    // Used once only.
    assert_eq!(s.callback("acme", &[("code", "li-code"), ("state", &linkedin["state"])]).await.1["ads_error"], UNKNOWN_STATE);

    let google = s.authorize("acme", s.acme, "google").await;
    assert_eq!((google["scope"].as_str(), google["access_type"].as_str(), google["prompt"].as_str()), ("https://www.googleapis.com/auth/adwords", "offline", "consent"));
    s.callback("acme", &[("code", "g-code"), ("state", &google["state"])]).await;
    let form = s.fake.token_requests()[1].body.clone();
    assert_eq!(pkce_challenge(form["code_verifier"].as_str().unwrap()), google["code_challenge"], "the verifier matches the challenge");

    let meta = s.authorize("acme", s.acme, "meta").await;
    assert_eq!(meta["scope"], "ads_management", "only what audiences need, not the Pages sign-in's scope");
    s.callback("acme", &[("code", "the-code"), ("state", &meta["state"])]).await;

    let status = s.status().await;
    assert_eq!(status["available"], json!(["linkedin", "google", "meta"]));
    let c = &status["connections"];
    assert_eq!(c[0]["ad_accounts"], json!([{"id": "5001", "name": "Acme Ads"}, {"id": "5002", "name": "Acme EU"}]), "the closed account is left out");
    assert_eq!(c[0]["ad_account_id"], "", "two accounts, so one is picked by hand");
    assert_eq!((c[1]["ad_account_id"].as_str(), c[1]["ad_account_name"].as_str()), (Some(GOOGLE_ACCOUNT), Some("Acme Search")), "the manager account is left out");
    assert_eq!((c[2]["ad_account_id"].as_str(), c[2]["ad_account_name"].as_str()), (Some(META_ACCOUNT), Some("Acme Meta")));
    assert!(c[0].get("access_enc").is_none() && c[0].get("refresh_enc").is_none());

    let stored: String = sqlx::query_scalar("select json_agg(c)::text from ad_connections c").fetch_one(&s.db).await.unwrap();
    assert!(!stored.contains("refresh-for") && !stored.contains("access-") && !stored.contains("meta-long"), "{stored}");
    let (refresh, meta_refresh, meta_access): (String, String, String) = sqlx::query_as(
        "select max(refresh_enc) filter (where platform = 'linkedin'), max(refresh_enc) filter (where platform = 'meta'), max(access_enc) filter (where platform = 'meta') from ad_connections",
    )
    .fetch_one(&s.db)
    .await
    .unwrap();
    assert_eq!(crypto::decrypt("test-secret", &refresh).unwrap(), "refresh-for-li-code");
    assert_eq!((meta_refresh.as_str(), crypto::decrypt("test-secret", &meta_access).unwrap().as_str()), ("", "meta-long"), "Meta has no refresh token");

    // A campaign can't have a LinkedIn audience until an account is picked, and only a listed one can be.
    let res = s.toggle("acme", s.campaign, "linkedin", true).await;
    assert_eq!(res.status(), 400);
    assert!(res.json::<Value>().await.unwrap()["error"].as_str().unwrap().starts_with("Pick a LinkedIn ad account"));
    let pick = |id: &str| s.call(Method::PUT, "acme", &format!("/projects/{}/ads/linkedin/account", s.acme)).json(&json!({"account_id": id})).send();
    assert_eq!(pick("5003").await.unwrap().status(), 400);
    let picked: Value = pick("5001").await.unwrap().json().await.unwrap();
    assert_eq!((picked["connections"][0]["ad_account_id"].as_str(), picked["connections"][0]["ad_account_name"].as_str()), (Some("5001"), Some("Acme Ads")));
    assert!(s.fake.api_requests().iter().all(|r| r.method == "GET" || r.path.ends_with(":search")), "signing in only reads");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_callback_refuses_bad_states_other_orgs_and_denials(db: PgPool) {
    let s = Server::start(db).await;
    let error = |back: (String, HashMap<String, String>)| back.1.get("ads_error").cloned().unwrap_or_default();
    assert_eq!(s.callback("acme", &[("code", "c"), ("state", "made-up.state")]).await.0, "/app");
    assert_eq!(s.call(Method::POST, "acme", &format!("/projects/{}/ads/linkedin/oauth", s.theirs)).send().await.unwrap().status(), 404);
    assert_eq!(s.call(Method::POST, "acme", &format!("/projects/{}/ads/tiktok/oauth", s.acme)).send().await.unwrap().status(), 404);

    let q = s.authorize("acme", s.acme, "linkedin").await;
    assert!(error(s.callback("beta", &[("code", "c"), ("state", &q["state"])]).await).starts_with("You switched organizations"));
    let q = s.authorize("acme", s.acme, "linkedin").await;
    let denied = s.callback("acme", &[("error", "user_cancelled_authorize"), ("state", &q["state"])]).await;
    assert_eq!(error(denied), "You didn't allow Splee into LinkedIn, so nothing was connected.");
    let q = s.authorize("acme", s.acme, "meta").await;
    let denied = s.callback("acme", &[("error", "access_denied"), ("error_reason", "user_denied"), ("state", &q["state"])]).await;
    assert_eq!(error(denied), "You didn't allow Splee into Meta, so nothing was connected.");

    // A CRM sign-in's state can't finish this one.
    let q = s.authorize("acme", s.acme, "google").await;
    let (nonce, _) = q["state"].split_once('.').unwrap();
    let (user, org): (Uuid, Uuid) =
        sqlx::query_as("select u.id, p.org_id from users u, projects p where u.email = 'ana@acme.test' and p.id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
    let crm = oauth::sign_state("test-secret", oauth::CRM, nonce, user, org);
    assert_eq!(error(s.callback("acme", &[("code", "c"), ("state", &crm)]).await), "This sign-in expired or was already used. Connect Google Ads again.");

    assert!(s.fake.requests().is_empty(), "no code was traded");
    assert_eq!(s.status().await["connections"], json!([]));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn campaign_audiences_hold_contacted_leads_and_drop_those_who_opt_out(db: PgPool) {
    let s = Server::start(db).await;
    s.connect_all().await;
    let (acme, campaign) = (s.acme, s.campaign);
    s.lead(acme, campaign, "Ana@Acme.test", "contacted", "+447700900123").await;
    let bo = s.lead(acme, campaign, "bo@beta.test", "replied", "").await;
    s.lead(acme, campaign, "dee@acme.test", "hot", "").await;
    let eve = s.lead(acme, campaign, "eve@acme.test", "not_interested", "").await;
    // Never in any audience: not emailed yet, opted out, or on the do-not-contact list by address, domain or number.
    let never = ["new@acme.test", "queued@acme.test", "unsub@acme.test", "bounce@acme.test", "dnc@acme.test", "supp@acme.test", "lee@blocked.test", "pat@acme.test"];
    for (email, status) in never.iter().zip(["new", "queued", "unsubscribed", "bounced", "do_not_contact", "contacted", "contacted", "contacted"]) {
        let phone = if *email == "pat@acme.test" { "+15550001111" } else { "" };
        s.lead(acme, campaign, email, status, phone).await;
    }
    s.suppress(acme, "supp@acme.test", "email").await;
    s.suppress(acme, "blocked.test", "domain").await;
    s.suppress(acme, "+15550001111", "phone").await;
    s.lead(acme, s.second, "other@acme.test", "contacted", "").await;

    let status = s.turn_on_all(campaign).await;
    let audiences = status["audiences"].as_array().unwrap();
    assert_eq!(audiences.iter().map(|a| a["platform"].as_str().unwrap()).collect::<Vec<_>>(), ["linkedin", "google", "meta"]);
    assert!(audiences.iter().all(|a| a["name"] == "Splee: Spring outreach" && a["uploaded"] == 0 && a["synced_at"].is_null()));
    assert_eq!(s.fake.audience("linkedin").unwrap().0, "5001");
    assert_eq!(s.fake.audience("google").unwrap().0, GOOGLE_ACCOUNT);
    assert_eq!(s.fake.audience("meta").unwrap().0, META_ACCOUNT);
    // Turning it on again changes nothing.
    assert_eq!(s.toggle("acme", campaign, "meta", true).await.status(), 200);
    assert_eq!(s.fake.emails("meta").len(), 1);

    assert_eq!(tick(&s.state).await.unwrap(), 3, "a new audience syncs on the next tick");
    let first = hashed(&["ana@acme.test", "bo@beta.test", "dee@acme.test", "eve@acme.test"]);
    for platform in ["linkedin", "google", "meta"] {
        assert_eq!(s.fake.emails(platform), std::slice::from_ref(&first), "{platform}");
    }
    assert_eq!(s.fake.audience("linkedin").unwrap().3, BTreeSet::new());
    assert_eq!(s.fake.audience("google").unwrap().3, hashed(&["+447700900123"]));
    assert_eq!(s.fake.audience("meta").unwrap().3, hashed(&["447700900123"]));
    let sent: String = s.fake.requests().iter().map(|r| r.body.to_string()).collect();
    for email in never {
        assert!(!sent.contains(&sha256_hex(email)), "{email} was never sent");
    }
    assert!(!sent.contains("acme.test") && !sent.contains("beta.test") && !sent.contains("7700900123"), "only hashes leave Splee");

    let status: Value = s.call(Method::GET, "acme", &format!("/segments/{campaign}/audiences")).send().await.unwrap().json().await.unwrap();
    let sizes: Vec<(i64, i64)> = status["audiences"].as_array().unwrap().iter().map(|a| (a["size"].as_i64().unwrap(), a["uploaded"].as_i64().unwrap())).collect();
    assert_eq!(sizes, [(12, 4), (4, 4), (8, 4)], "each platform's own count, and what Splee sent");
    assert!(status["audiences"].as_array().unwrap().iter().all(|a| a["synced_at"].is_string() && a["error"].is_null()));
    assert_eq!(tick(&s.state).await.unwrap(), 0, "nothing is due again within the hour");

    // Ana unsubscribes, Bo moves to the other campaign, Dee goes on the do-not-contact list, Eve is deleted, and Carl is emailed.
    let ana: Uuid = sqlx::query_scalar("select id from leads where email = 'ana@acme.test'").fetch_one(&s.db).await.unwrap();
    sqlx::query("update leads set status = 'unsubscribed' where id = $1").bind(ana).execute(&s.db).await.unwrap();
    sqlx::query("update leads set segment_id = $2 where id = $1").bind(bo).bind(s.second).execute(&s.db).await.unwrap();
    s.suppress(acme, "dee@acme.test", "email").await;
    sqlx::query("delete from leads where id = $1").bind(eve).execute(&s.db).await.unwrap();
    s.lead(acme, campaign, "carl@acme.test", "contacted", "").await;
    let before = s.fake.requests().len();
    assert_eq!(s.tick().await, 3);
    for platform in ["linkedin", "meta"] {
        assert_eq!(s.fake.emails(platform), [hashed(&["carl@acme.test"])], "{platform}");
    }
    // Google's removals go in a job of their own, and Carl in the next once Google says it went through.
    assert_eq!(s.fake.emails("google"), [BTreeSet::new()]);
    s.tick().await;
    assert_eq!(s.fake.emails("google"), [hashed(&["carl@acme.test"])]);
    assert!(s.fake.audience("google").unwrap().3.is_empty() && s.fake.audience("meta").unwrap().3.is_empty(), "Ana's number went with her");
    let linkedin: Vec<Value> = s.fake.requests()[before..].iter().filter(|r| r.platform == "linkedin" && r.path.ends_with("/users")).map(|r| r.body.clone()).collect();
    let actions: Vec<(String, usize)> =
        linkedin.iter().map(|b| (b["elements"][0]["action"].as_str().unwrap().to_string(), b["elements"].as_array().unwrap().len())).collect();
    assert_eq!(actions, [("REMOVE".to_string(), 4), ("ADD".to_string(), 1)], "removals first, and only the difference");
    // A sync with nothing new only reads, once Google confirms its last job.
    s.tick().await;
    let members: Vec<(String, i64)> = sqlx::query_as("select state, count(*) from ad_audience_members group by state").fetch_all(&s.db).await.unwrap();
    assert_eq!(members, [("added".to_string(), 3)]);
    let before = s.fake.requests().len();
    s.tick().await;
    assert!(s.fake.requests()[before..].iter().all(|r| r.method == "GET" || r.path.ends_with(":search")));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn hot_leads_from_every_campaign_get_their_own_audience(db: PgPool) {
    let s = Server::start(db).await;
    s.connect_all().await;
    s.lead(s.acme, s.campaign, "hot@acme.test", "hot", "").await;
    s.lead(s.acme, s.second, "booked@acme.test", "meeting_booked", "").await;
    s.lead(s.acme, s.second, "warm@acme.test", "replied", "").await;
    s.lead(s.acme, s.second, "gone@acme.test", "hot", "").await;
    s.suppress(s.acme, "gone@acme.test", "email").await;
    s.lead(s.theirs, s.their_campaign, "theirs@other.test", "hot", "").await;

    let res = s.call(Method::PUT, "acme", &format!("/projects/{}/ads/meta/hot", s.acme)).json(&json!({"on": true})).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let status: Value = res.json().await.unwrap();
    assert_eq!((status["audiences"][0]["name"].as_str(), status["audiences"][0]["platform"].as_str()), (Some("Splee: Hot leads"), Some("meta")));
    s.tick().await;
    assert_eq!(s.fake.emails("meta"), [hashed(&["hot@acme.test", "booked@acme.test"])]);
    // The campaign page doesn't list it.
    let campaign: Value = s.call(Method::GET, "acme", &format!("/segments/{}/audiences", s.campaign)).send().await.unwrap().json().await.unwrap();
    assert_eq!(campaign["audiences"], json!([]));

    let res = s.call(Method::PUT, "acme", &format!("/projects/{}/ads/meta/hot", s.acme)).json(&json!({"on": false})).send().await.unwrap();
    assert_eq!(res.json::<Value>().await.unwrap()["audiences"], json!([]));
    assert!(s.fake.emails("meta").is_empty(), "turning it off deletes it on Meta");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn batches_stay_within_each_platforms_limits(db: PgPool) {
    let s = Server::start(db).await;
    s.connect_all().await;
    sqlx::query("insert into leads (project_id, segment_id, email, status) select $1, $2, 'lead' || g || '@bulk.test', 'contacted' from generate_series(1, 10050) g")
        .bind(s.acme)
        .bind(s.campaign)
        .execute(&s.db)
        .await
        .unwrap();
    s.turn_on_all(s.campaign).await;
    assert_eq!(tick(&s.state).await.unwrap(), 3);
    let sizes = |platform: &str, path: &str, rows: fn(&Value) -> usize| -> Vec<usize> {
        s.fake.requests().iter().filter(|r| r.platform == platform && r.path.ends_with(path)).map(|r| rows(&r.body)).collect()
    };
    assert_eq!(sizes("linkedin", "/users", |b| b["elements"].as_array().unwrap().len()), [5000, 5000, 50]);
    assert_eq!(sizes("google", ":addOperations", |b| b["operations"].as_array().unwrap().len()), [10_000, 50]);
    assert_eq!(sizes("google", ":run", |_| 1).len(), 1, "one job, filled a batch at a time");
    let meta = |b: &Value| serde_json::from_str::<Value>(b["payload"].as_str().unwrap()).unwrap()["data"].as_array().unwrap().len();
    assert_eq!(sizes("meta", "/users", meta), [10_000, 50]);
    for platform in ["linkedin", "google", "meta"] {
        assert_eq!(s.fake.emails(platform)[0].len(), 10_050, "{platform}");
    }
    let members: i64 = sqlx::query_scalar("select count(*) from ad_audience_members").fetch_one(&s.db).await.unwrap();
    assert_eq!(members, 3 * 10_050);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn tokens_are_renewed_and_a_revoked_sign_in_asks_for_a_reconnect(db: PgPool) {
    let s = Server::start(db).await;
    s.connect_all().await;
    s.lead(s.acme, s.campaign, "ana@acme.test", "contacted", "").await;
    s.turn_on_all(s.campaign).await;
    let secret = "test-secret";
    // LinkedIn's ran out, and Google turns its down before it runs out.
    sqlx::query("update ad_connections set expires_at = now() - interval '1 minute' where platform = 'linkedin'").execute(&s.db).await.unwrap();
    sqlx::query("update ad_connections set access_enc = $1 where platform = 'google'").bind(crypto::encrypt(secret, "stale")).execute(&s.db).await.unwrap();
    let before = s.fake.token_requests().len();
    assert_eq!(s.tick().await, 3);
    let renewals: Vec<Seen> = s.fake.token_requests()[before..].to_vec();
    let renewed: Vec<(&str, &str)> = renewals.iter().map(|r| (r.platform, r.body["refresh_token"].as_str().unwrap())).collect();
    assert_eq!(renewed, [("linkedin", "refresh-for-code-linkedin-acme"), ("google", "refresh-for-code-google-acme")]);
    assert!(renewals.iter().all(|r| r.body["grant_type"] == "refresh_token"));
    for platform in ["linkedin", "google", "meta"] {
        assert_eq!(s.fake.emails(platform), [hashed(&["ana@acme.test"])], "{platform}");
    }
    let google: String = sqlx::query_scalar("select access_enc from ad_connections where platform = 'google'").fetch_one(&s.db).await.unwrap();
    assert!(crypto::decrypt(secret, &google).unwrap().starts_with("access-"), "the new token is kept");

    // LinkedIn revokes the refresh token, and Meta stops taking its token.
    sqlx::query("update ad_connections set refresh_enc = $1, expires_at = now() - interval '1 minute' where platform = 'linkedin'")
        .bind(crypto::encrypt(secret, "revoked"))
        .execute(&s.db)
        .await
        .unwrap();
    s.fake.world.lock().unwrap().live.remove("meta-long");
    s.tick().await;
    let signed_out = "LinkedIn signed Splee out, so audiences there stopped syncing. Reconnect LinkedIn.";
    let (reconnect, error, _) = s.connection(s.acme, "linkedin").await;
    assert!(reconnect);
    assert_eq!(error.as_deref(), Some(signed_out));
    assert!(s.connection(s.acme, "meta").await.0 && !s.connection(s.acme, "google").await.0);
    let errors: Vec<Option<String>> = sqlx::query_scalar("select a.error from ad_audiences a join ad_connections c on c.id = a.connection_id order by c.platform")
        .fetch_all(&s.db)
        .await
        .unwrap();
    assert_eq!(errors, [None, Some(signed_out.to_string()), Some("Meta signed Splee out, so audiences there stopped syncing. Reconnect Meta.".to_string())]);

    // Nothing more goes to either until a reconnect, and the campaign page can't add one there.
    let (tokens, calls) = (s.fake.token_requests().len(), s.fake.api_requests().iter().filter(|r| r.platform != "google").count());
    s.tick().await;
    assert_eq!((s.fake.token_requests().len() - tokens, s.fake.api_requests().iter().filter(|r| r.platform != "google").count()), (0, calls), "nothing goes to LinkedIn or Meta");
    let res = s.toggle("acme", s.second, "linkedin", true).await;
    assert_eq!(res.status(), 400);
    assert_eq!(res.json::<Value>().await.unwrap()["error"], "LinkedIn signed Splee out. Reconnect it on the integrations page.");
    assert_eq!(s.status().await["connections"][0]["needs_reconnect"], true);

    // Reconnecting picks up where it left off, with the same account and audience.
    s.sign_in_as("acme", s.acme, "linkedin").await;
    s.sign_in_as("acme", s.acme, "meta").await;
    let status = s.status().await;
    assert_eq!((status["connections"][0]["needs_reconnect"].as_bool(), status["connections"][0]["ad_account_id"].as_str()), (Some(false), Some("5001")));
    s.tick().await;
    assert_eq!(s.fake.emails("linkedin"), [hashed(&["ana@acme.test"])]);
    let errors: i64 = sqlx::query_scalar("select count(*) from ad_audiences where error is not null").fetch_one(&s.db).await.unwrap();
    assert_eq!(errors, 0);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn rate_limits_are_waited_out_then_hold_the_connection(db: PgPool) {
    let s = Server::start(db).await;
    s.sign_in_as("acme", s.acme, "linkedin").await;
    s.call(Method::PUT, "acme", &format!("/projects/{}/ads/linkedin/account", s.acme)).json(&json!({"account_id": "5001"})).send().await.unwrap();
    s.lead(s.acme, s.campaign, "ana@acme.test", "contacted", "").await;
    assert_eq!(s.toggle("acme", s.campaign, "linkedin", true).await.status(), 200);

    // Told to slow down once, the request is sent again after the wait LinkedIn asked for.
    s.fake.world.lock().unwrap().busy = 1;
    tick(&s.state).await.unwrap();
    let users = s.fake.requests().iter().filter(|r| r.path.ends_with("/users")).count();
    assert_eq!(users, 2);
    assert_eq!(s.fake.emails("linkedin"), [hashed(&["ana@acme.test"])]);
    assert_eq!(s.connection(s.acme, "linkedin").await.2, None);

    // Told every time, the sync stops and the connection waits.
    s.lead(s.acme, s.campaign, "bo@acme.test", "contacted", "").await;
    s.fake.world.lock().unwrap().busy = 100;
    s.tick().await;
    let (_, _, held) = s.connection(s.acme, "linkedin").await;
    let wait = (held.unwrap() - Utc::now()).num_seconds();
    assert!((50..=60).contains(&wait), "at least a minute, {wait}");
    let error: Option<String> = sqlx::query_scalar("select error from ad_audiences").fetch_one(&s.db).await.unwrap();
    assert_eq!(error.as_deref(), Some("LinkedIn asked Splee to slow down, so audiences there sync again in a while."));
    let asked = s.fake.requests().len();
    s.tick().await;
    assert_eq!(s.fake.requests().len(), asked, "nothing is sent while it waits");

    s.fake.world.lock().unwrap().busy = 0;
    sqlx::query("update ad_connections set held_until = now() - interval '1 second'").execute(&s.db).await.unwrap();
    s.tick().await;
    assert_eq!(s.fake.emails("linkedin"), [hashed(&["ana@acme.test", "bo@acme.test"])]);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn other_orgs_cant_see_or_change_a_projects_audiences_and_each_syncs_with_its_own_sign_in(db: PgPool) {
    let s = Server::start(db).await;
    s.connect_all().await;
    s.lead(s.acme, s.campaign, "ana@acme.test", "contacted", "").await;
    assert_eq!(s.toggle("acme", s.campaign, "linkedin", true).await.status(), 200);

    let acme = s.acme;
    for (method, path) in [
        (Method::GET, format!("/projects/{acme}/ads")),
        (Method::DELETE, format!("/projects/{acme}/ads/linkedin")),
        (Method::GET, format!("/segments/{}/audiences", s.campaign)),
    ] {
        assert_eq!(s.call(method.clone(), "other", &path).send().await.unwrap().status(), 404, "{method} {path}");
    }
    assert_eq!(s.call(Method::PUT, "other", &format!("/projects/{acme}/ads/linkedin/account")).json(&json!({"account_id": "5002"})).send().await.unwrap().status(), 404);
    assert_eq!(s.call(Method::PUT, "other", &format!("/projects/{acme}/ads/meta/hot")).json(&json!({"on": true})).send().await.unwrap().status(), 404);
    assert_eq!(s.toggle("other", s.campaign, "linkedin", false).await.status(), 404);
    assert_eq!(s.fake.emails("linkedin").len(), 1, "Acme's audience is still there");
    assert_eq!(s.status().await["connections"][0]["ad_account_id"], "5001");
    // Cy's own campaign has no LinkedIn sign-in to use.
    let res = s.toggle("other", s.their_campaign, "linkedin", true).await;
    assert_eq!(res.status(), 400);
    assert_eq!(res.json::<Value>().await.unwrap()["error"], "Connect LinkedIn on the integrations page first.");

    // Once Cy signs in, each project's audience gets its own leads with its own token.
    s.sign_in_as("other", s.theirs, "linkedin").await;
    s.call(Method::PUT, "other", &format!("/projects/{}/ads/linkedin/account", s.theirs)).json(&json!({"account_id": "5002"})).send().await.unwrap();
    s.lead(s.theirs, s.their_campaign, "cy@other.test", "contacted", "").await;
    assert_eq!(s.toggle("other", s.their_campaign, "linkedin", true).await.status(), 200);
    assert_eq!(s.tick().await, 2);
    let world = s.fake.world.lock().unwrap();
    let by_name: HashMap<&str, (&str, &BTreeSet<String>)> = world.audiences.values().map(|a| (a.name.as_str(), (a.account.as_str(), &a.emails))).collect();
    assert_eq!(by_name["Splee: Spring outreach"], ("5001", &hashed(&["ana@acme.test"])));
    assert_eq!(by_name["Splee: Cy's campaign"], ("5002", &hashed(&["cy@other.test"])));
    let tokens: BTreeSet<(String, String)> = world
        .requests
        .iter()
        .filter(|r| r.path.ends_with("/users"))
        .map(|r| (r.body["elements"][0]["userIds"][0]["idValue"].as_str().unwrap().to_string(), r.token.clone()))
        .collect();
    assert_eq!(tokens.len(), 2);
    assert_ne!(tokens.iter().next().unwrap().1, tokens.iter().nth(1).unwrap().1, "each project used its own token");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn audiences_move_with_the_ad_account_come_back_when_deleted_and_go_on_disconnect(db: PgPool) {
    let s = Server::start(db).await;
    s.connect_all().await;
    s.lead(s.acme, s.campaign, "ana@acme.test", "contacted", "").await;
    s.toggle("acme", s.campaign, "linkedin", true).await;
    s.tick().await;
    assert_eq!(s.fake.audience("linkedin").unwrap().0, "5001");

    // Another ad account: deleted in the old one, made again in the new one on the next tick.
    s.call(Method::PUT, "acme", &format!("/projects/{}/ads/linkedin/account", s.acme)).json(&json!({"account_id": "5002"})).send().await.unwrap();
    assert!(s.fake.audience("linkedin").is_none());
    assert_eq!(tick(&s.state).await.unwrap(), 1);
    let (account, name, emails, _) = s.fake.audience("linkedin").unwrap();
    assert_eq!((account.as_str(), name.as_str(), emails), ("5002", "Splee: Spring outreach", hashed(&["ana@acme.test"])));

    // Deleted in Campaign Manager, it's made again and filled.
    s.fake.world.lock().unwrap().audiences.clear();
    s.tick().await;
    assert_eq!(s.fake.emails("linkedin"), [hashed(&["ana@acme.test"])]);

    // Turned off, it's deleted there. Disconnecting deletes the rest and forgets the sign-in.
    assert_eq!(s.toggle("acme", s.campaign, "linkedin", false).await.status(), 200);
    assert!(s.fake.emails("linkedin").is_empty());
    s.toggle("acme", s.campaign, "linkedin", true).await;
    s.toggle("acme", s.campaign, "meta", true).await;
    let res = s.call(Method::DELETE, "acme", &format!("/projects/{}/ads/linkedin", s.acme)).send().await.unwrap();
    let status: Value = res.json().await.unwrap();
    assert_eq!(status["connections"].as_array().unwrap().iter().map(|c| c["platform"].as_str().unwrap()).collect::<Vec<_>>(), ["google", "meta"]);
    assert!(s.fake.emails("linkedin").is_empty());
    assert_eq!(s.fake.emails("meta").len(), 1, "the other platforms keep theirs");
    let left: i64 = sqlx::query_scalar("select count(*) from ad_audiences").fetch_one(&s.db).await.unwrap();
    assert_eq!(left, 1);
}

/// Each row of the ledger, as (address, state, job).
async fn ledger(db: &PgPool) -> Vec<(String, String, String)> {
    sqlx::query_as("select l.email::text, m.state, m.job from ad_audience_members m join leads l on l.id = m.lead_id order by 1").fetch_all(db).await.unwrap()
}

async fn audience_error(db: &PgPool) -> Option<String> {
    sqlx::query_scalar("select error from ad_audiences").fetch_one(db).await.unwrap()
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_lead_from_a_partly_failed_upload_still_comes_out_when_they_opt_out(db: PgPool) {
    let s = Server::start(db).await;
    s.sign_in_as("acme", s.acme, "meta").await;
    let ana = s.lead(s.acme, s.campaign, "ana@acme.test", "contacted", "+447700900123").await;
    s.lead(s.acme, s.campaign, "bo@acme.test", "contacted", "").await;
    assert_eq!(s.toggle("acme", s.campaign, "meta", true).await.status(), 200);

    // Meta takes Ana, who has a number, then turns down the request for Bo, who hasn't.
    s.fake.world.lock().unwrap().refuse_email_only = true;
    s.tick().await;
    assert_eq!(s.fake.emails("meta"), [hashed(&["ana@acme.test"])]);
    assert_eq!(audience_error(&s.db).await.as_deref(), Some("Meta said: The payload couldn't be read."));
    let adding = |email: &str| (email.to_string(), "adding".to_string(), String::new());
    assert_eq!(ledger(&s.db).await, [adding("ana@acme.test"), adding("bo@acme.test")], "both were written before they went, and neither is confirmed");

    // Ana unsubscribes before the next sync, which takes her out though Meta never confirmed her, and puts Bo in.
    s.fake.world.lock().unwrap().refuse_email_only = false;
    sqlx::query("update leads set status = 'unsubscribed' where id = $1").bind(ana).execute(&s.db).await.unwrap();
    s.tick().await;
    assert_eq!(s.fake.emails("meta"), [hashed(&["bo@acme.test"])]);
    assert!(s.fake.audience("meta").unwrap().3.is_empty(), "her number went too");
    assert_eq!(ledger(&s.db).await, [("bo@acme.test".to_string(), "added".to_string(), String::new())]);
    assert_eq!(audience_error(&s.db).await, None);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn google_jobs_are_confirmed_once_they_go_through_and_sent_again_when_they_fail(db: PgPool) {
    let s = Server::start(db).await;
    s.sign_in_as("acme", s.acme, "google").await;
    let ana = s.lead(s.acme, s.campaign, "ana@acme.test", "contacted", "").await;
    assert_eq!(s.toggle("acme", s.campaign, "google", true).await.status(), 200);
    let state = |rows: Vec<(String, String, String)>| rows.into_iter().map(|(_, state, job)| (state, !job.is_empty())).collect::<Vec<_>>();

    // While the job runs, Ana isn't confirmed and no other job starts.
    s.fake.world.lock().unwrap().job_outcome = "RUNNING";
    s.tick().await;
    assert_eq!((s.fake.job_count(), state(ledger(&s.db).await)), (1, vec![("adding".to_string(), true)]));
    s.tick().await;
    assert_eq!(s.fake.job_count(), 1, "nothing else starts while it runs");

    // It fails. The page says so and the next sync sends Ana again in a new job.
    s.fake.finish_jobs("FAILED");
    s.tick().await;
    let failed = "Google Ads couldn't apply Splee's last change to this list (INVALID_USER_LIST). Splee sends it again within the hour.";
    assert_eq!(audience_error(&s.db).await.as_deref(), Some(failed));
    assert_eq!((s.fake.job_count(), state(ledger(&s.db).await)), (1, vec![("adding".to_string(), false)]));
    s.fake.world.lock().unwrap().job_outcome = "SUCCESS";
    s.tick().await;
    assert_eq!(s.fake.job_count(), 2);
    assert_eq!(s.fake.emails("google"), [hashed(&["ana@acme.test"])]);
    assert_eq!(state(ledger(&s.db).await), [("adding".to_string(), true)], "confirmed only once a sync reads the job");
    s.tick().await;
    assert_eq!((s.fake.job_count(), state(ledger(&s.db).await)), (2, vec![("added".to_string(), false)]));
    assert_eq!(audience_error(&s.db).await, None);

    // A removal stays in the ledger until Google says it went through.
    s.fake.world.lock().unwrap().job_outcome = "RUNNING";
    sqlx::query("update leads set status = 'unsubscribed' where id = $1").bind(ana).execute(&s.db).await.unwrap();
    s.tick().await;
    assert_eq!(state(ledger(&s.db).await), [("removing".to_string(), true)]);
    s.fake.finish_jobs("SUCCESS");
    assert_eq!(s.fake.emails("google"), [BTreeSet::new()]);
    s.tick().await;
    assert_eq!(ledger(&s.db).await, []);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn picking_another_account_waits_for_a_running_sync_and_leaves_no_stale_ledger(db: PgPool) {
    let s = Server::start(db).await;
    s.sign_in_as("acme", s.acme, "linkedin").await;
    let pick = |account: &str| s.call(Method::PUT, "acme", &format!("/projects/{}/ads/linkedin/account", s.acme)).json(&json!({"account_id": account})).send();
    assert_eq!(pick("5001").await.unwrap().status(), 200);
    s.lead(s.acme, s.campaign, "ana@acme.test", "contacted", "").await;
    assert_eq!(s.toggle("acme", s.campaign, "linkedin", true).await.status(), 200);

    // The sync has written Ana to the ledger and waits on LinkedIn when the account changes.
    s.fake.stall.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    let state = s.state.clone();
    let sync = tokio::spawn(async move { tick(&state).await.unwrap() });
    s.fake.stall.reached.notified().await;
    let picked = tokio::spawn(pick("5002"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!picked.is_finished(), "the account change waits for the sync");
    s.fake.stall.release.notify_one();
    assert_eq!(sync.await.unwrap(), 1);
    assert_eq!(picked.await.unwrap().unwrap().status(), 200);

    assert_eq!(ledger(&s.db).await, [], "nothing from the old account is left");
    let (external, synced): (String, Option<Ts>) = sqlx::query_as("select external_id, synced_at from ad_audiences").fetch_one(&s.db).await.unwrap();
    assert_eq!((external.as_str(), synced), ("", None));
    assert!(s.fake.audience("linkedin").is_none(), "deleted in the old account");
    s.tick().await;
    let (account, _, emails, _) = s.fake.audience("linkedin").unwrap();
    assert_eq!((account.as_str(), emails), ("5002", hashed(&["ana@acme.test"])));
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn only_a_few_turns_hold_their_own_connection_at_once(db: PgPool) {
    let mut held = vec![];
    for _ in 0..TURNS {
        held.push(lock(&db, Uuid::new_v4(), Platform::Meta).await.unwrap());
    }
    let waiting = tokio::spawn({
        let db = db.clone();
        async move { lock(&db, Uuid::new_v4(), Platform::Meta).await.map(drop) }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiting.is_finished(), "a fifth waits for a slot before it connects");
    held.pop();
    tokio::time::timeout(Duration::from_secs(5), waiting).await.unwrap().unwrap().unwrap();
}
