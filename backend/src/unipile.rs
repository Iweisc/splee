//! Unipile sends due LinkedIn steps from the user's own LinkedIn account: the connect note as an invitation, each message
//! once they've accepted. A step it can't send keeps the reason in send_error and stays on the LinkedIn page for the user.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use chrono_tz::Tz;
use reqwest::{StatusCode, Url};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::hooks::{self, Alert};
use crate::models::{Lead, LinkedinMessage, Ts};
use crate::routes::linkedin::mark;
use crate::social::Failure as Posting;
use crate::state::AppState;

/// Sends per LinkedIn account a day, well under the 80 to 100 invitations LinkedIn takes from a paid account.
const DAILY_SENDS: i64 = 25;
/// How long a message waits for the lead to accept the connect note before it's left for the user.
const ACCEPT_DAYS: i64 = 21;
/// LinkedIn's limits, which pause the account's sending for an hour rather than failing the lead.
const ACCOUNT_LIMITS: [&str; 2] = ["errors/limit_exceeded", "errors/cannot_resend_yet"];
/// People search result pages per LinkedIn account a day, counted in linkedin_searches. LinkedIn restricts accounts that search
/// far more than a person would, and a classic page holds 10 people.
pub const DAILY_SEARCH_PAGES: i32 = 100;

pub struct Unipile {
    base: String,
    token: String,
    account: String,
    client: reqwest::Client,
}

#[derive(Debug)]
enum Failure {
    /// Something about this lead, like a profile LinkedIn doesn't find. The step is left for the user.
    Lead { kind: String, reason: String },
    /// The account or Unipile, like an expired LinkedIn login or a rate limit. The account's sending stops for an hour.
    Account(anyhow::Error),
}

impl Failure {
    fn lead(reason: &str) -> Self {
        Failure::Lead { kind: String::new(), reason: reason.into() }
    }

    fn into_error(self) -> anyhow::Error {
        match self {
            Failure::Lead { reason, .. } => anyhow::anyhow!(reason),
            Failure::Account(e) => e,
        }
    }
}

/// What happened to a due step.
#[derive(Debug, PartialEq)]
enum Step {
    Sent,
    /// The invitation was already out, sent by hand.
    AlreadyInvited,
    /// A connect note for someone who's already a connection.
    Connected,
    /// A message for someone who hasn't accepted the connect note yet.
    Waiting,
}

/// The API base URL from the DSN on the Unipile dashboard, like api8.unipile.com:13851, with or without https:// or a
/// path. Only Unipile's own hosts are taken, so the token never goes anywhere else.
pub fn base_url(dsn: &str) -> Option<String> {
    let dsn = dsn.trim();
    let url = Url::parse(&if dsn.contains("://") { dsn.to_string() } else { format!("https://{dsn}") }).ok()?;
    let host = url.host_str()?;
    if url.scheme() != "https" || !(host == "unipile.com" || host.ends_with(".unipile.com")) {
        return None;
    }
    Some(match url.port() {
        Some(port) => format!("https://{host}:{port}/api/v1"),
        None => format!("https://{host}/api/v1"),
    })
}

/// Unipile explains a failure in `detail` or `title`, and names it in `type`, like errors/resource_not_found.
fn explain(status: StatusCode, body: &Value) -> (String, String) {
    let reason = ["detail", "title"].iter().find_map(|k| body[k].as_str().filter(|r| !r.is_empty())).unwrap_or(status.as_str()).to_string();
    (reason, body["type"].as_str().unwrap_or_default().to_string())
}

/// The post a LinkedIn link points at, as the urn Unipile takes: urn:li:activity:<id>, or the ugcPost or share kind. Reads
/// feed/update links, /posts/ share links and bare urns.
pub fn post_urn(link: &str) -> Option<String> {
    let link = link.trim().replace("%3A", ":").replace("%3a", ":");
    let kinds = ["activity", "ugcPost", "share"];
    let urn = |kind: &str, rest: &str| {
        let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (8..=25).contains(&id.len()).then(|| format!("urn:li:{kind}:{id}"))
    };
    let in_text = |s: &str| kinds.iter().find_map(|k| s.find(&format!("urn:li:{k}:")).and_then(|at| urn(k, &s[at + k.len() + 8..])));
    if link.starts_with("urn:li:") {
        return in_text(&link);
    }
    let url = Url::parse(&if link.contains("://") { link.clone() } else { format!("https://{link}") }).ok()?;
    let host = url.host_str()?;
    if !(host == "linkedin.com" || host.ends_with(".linkedin.com")) {
        return None;
    }
    // Share links end in the id, like /posts/sam-lee_some-title-activity-7236734771807019010-AbCd.
    in_text(url.path()).or_else(|| {
        let slug = url.path().strip_prefix("/posts/")?;
        kinds.iter().find_map(|k| slug.rfind(&format!("-{k}-")).and_then(|at| urn(k, &slug[at + k.len() + 2..])))
    })
}

/// The one link Splee keeps for a LinkedIn post.
pub fn post_link(urn: &str) -> String {
    format!("https://www.linkedin.com/feed/update/{urn}/")
}

/// The public id in a profile URL, like sam-lee in https://www.linkedin.com/in/sam-lee/.
pub fn profile_id(url: &str) -> Option<&str> {
    let id = url.split_once("linkedin.com/in/")?.1.split(['/', '?', '#']).next()?;
    (!id.is_empty()).then_some(id)
}

/// A LinkedIn people search. Empty fields are left out.
#[derive(Debug, Default)]
pub struct PeopleSearch {
    pub keywords: String,
    /// A company's name, matched to its LinkedIn page, so only people who work there now are found.
    pub company: String,
    /// Words in their job title. LinkedIn takes OR between titles, like `Owner OR Founder`.
    pub title: String,
    /// A city, region or country, matched to LinkedIn's place.
    pub location: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Person {
    pub name: String,
    pub headline: String,
    pub company: String,
    pub location: String,
    pub linkedin_url: String,
    pub provider_id: String,
}

#[derive(Debug)]
pub struct People {
    pub people: Vec<Person>,
    /// Gives the next page to search_people, None on the last one.
    pub cursor: Option<String>,
}

/// LinkedIn's titles for job roles, ORed together, with the multi-word ones quoted.
pub fn any_title(roles: &[String]) -> String {
    let roles = roles.iter().map(|r| r.trim().replace('"', "")).filter(|r| !r.is_empty());
    roles.map(|r| if r.contains(' ') { format!("\"{r}\"") } else { r }).collect::<Vec<_>>().join(" OR ")
}

/// Only letters and digits, lowercased, so "Smile Dental Ltd." matches "smile dental ltd".
fn squash(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// A LinkedIn link without its tracking query, or "".
fn clean_link(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or_default().trim().to_string()
}

/// One person from a search page. Classic search only gives the headline, so their company is the one searched, else what the
/// headline says after " at ". LinkedIn hides the names of some people outside the account's network, and those are left out.
fn person(item: &Value, company: &str) -> Option<Person> {
    let s = |v: &Value| v.as_str().unwrap_or_default().trim().to_string();
    let name = Some(s(&item["name"])).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("{} {}", s(&item["first_name"]), s(&item["last_name"])).trim().to_string());
    if name.is_empty() || name == "LinkedIn Member" {
        return None;
    }
    let headline = s(&item["headline"]);
    let company = Some(s(&item["current_positions"][0]["company"]))
        .filter(|c| !c.is_empty())
        .or_else(|| (!company.is_empty()).then(|| company.to_string()))
        .or_else(|| headline.rsplit_once(" at ").map(|(_, c)| c.trim().to_string()))
        .unwrap_or_default();
    let link = item["public_profile_url"].as_str().filter(|u| !u.is_empty()).or(item["profile_url"].as_str()).unwrap_or_default();
    Some(Person { name, headline, company, location: s(&item["location"]), linkedin_url: clean_link(link), provider_id: s(&item["id"]) })
}

/// Counts a people search page against the account's day, or says no when it has had DAILY_SEARCH_PAGES.
async fn take_search_page(db: &PgPool, account: &str) -> sqlx::Result<bool> {
    let pages: Option<i32> = sqlx::query_scalar(
        "insert into linkedin_searches (account_id, day, pages) values ($1, (now() at time zone 'utc')::date, 1)
         on conflict (account_id, day) do update set pages = linkedin_searches.pages + 1 where linkedin_searches.pages < $2
         returning pages",
    )
    .bind(account)
    .bind(DAILY_SEARCH_PAGES)
    .fetch_optional(db)
    .await?;
    Ok(pages.is_some())
}

/// The LinkedIn account behind a Unipile token, checked before it's saved: the first one that's running, else the first.
pub async fn find_account(base: &str, token: &str) -> anyhow::Result<(String, String)> {
    let u = Unipile::new(base, token, "")?;
    let list = u.call(u.client.get(format!("{base}/accounts?limit=250"))).await.map_err(Failure::into_error)?;
    let linkedin: Vec<&Value> = list["items"].as_array().into_iter().flatten().filter(|a| a["type"] == "LINKEDIN").collect();
    let running = |a: &&&Value| a["sources"].as_array().is_some_and(|s| s.iter().all(|s| s["status"] == "OK"));
    let account = linkedin.iter().find(running).or(linkedin.first()).context("There's no LinkedIn account on it. Connect one in Unipile first")?;
    let id = account["id"].as_str().filter(|id| !id.is_empty()).context("Unipile didn't return the account's id")?;
    Ok((id.to_string(), account["name"].as_str().unwrap_or_default().to_string()))
}

/// Has Unipile post the account's LinkedIn messages and new connections to `url`. Returns the webhooks' ids.
pub async fn add_webhooks(base: &str, token: &str, account: &str, url: &str) -> anyhow::Result<Vec<String>> {
    let u = Unipile::new(base, token, account)?;
    let mut ids = Vec::new();
    for (source, event) in [("messaging", "message_received"), ("users", "new_relation")] {
        // Without the header Unipile posts without a JSON content type, which the route turns away.
        let hook = json!({
            "request_url": url, "name": "Splee", "source": source, "events": [event], "account_ids": [account], "format": "json", "enabled": true,
            "headers": [{ "key": "Content-Type", "value": "application/json" }],
        });
        let made = u.call(u.client.post(format!("{base}/webhooks")).json(&hook)).await.map_err(Failure::into_error)?;
        ids.push(made["webhook_id"].as_str().context("Unipile didn't return the webhook's id")?.to_string());
    }
    Ok(ids)
}

/// Removes the webhooks add_webhooks made. A failure only leaves posts Splee answers with a 404.
pub async fn remove_webhooks(base: &str, token: &str, ids: &[String]) {
    let Ok(u) = Unipile::new(base, token, "") else { return };
    for id in ids {
        if let Err(e) = u.call(u.client.delete(format!("{base}/webhooks/{id}"))).await.map_err(Failure::into_error) {
            tracing::warn!(error = %e, "couldn't remove a Unipile webhook");
        }
    }
}

impl Unipile {
    pub fn new(base: &str, token: &str, account: &str) -> reqwest::Result<Self> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
        Ok(Self { base: base.into(), token: token.into(), account: account.into(), client })
    }

    async fn request(&self, request: reqwest::RequestBuilder) -> reqwest::Result<(StatusCode, Value)> {
        let res = request.header("X-API-KEY", &self.token).header("accept", "application/json").send().await?;
        let status = res.status();
        Ok((status, res.json().await.unwrap_or_default()))
    }

    async fn call(&self, request: reqwest::RequestBuilder) -> Result<Value, Failure> {
        let (status, body) = self.request(request).await.map_err(|e| Failure::Account(e.without_url().into()))?;
        if status.is_success() {
            return Ok(body);
        }
        let (reason, kind) = explain(status, &body);
        let about_lead = status == StatusCode::NOT_FOUND || (status == StatusCode::UNPROCESSABLE_ENTITY && !ACCOUNT_LIMITS.contains(&kind.as_str()));
        Err(if about_lead { Failure::Lead { kind, reason } } else { Failure::Account(anyhow::anyhow!("{reason}")) })
    }

    /// For social posts and the agent's LinkedIn tools. Unlike `call`, a request that may have reached LinkedIn, one that timed
    /// out or failed inside Unipile, fails the post, so it never goes out twice.
    async fn social(&self, request: reqwest::RequestBuilder) -> Result<Value, Posting> {
        let (status, body) = self.request(request).await.map_err(|e| Posting::Post(format!("Unipile didn't answer: {}", e.without_url())))?;
        if status.is_success() {
            return Ok(body);
        }
        let (reason, kind) = explain(status, &body);
        Err(match status {
            // A 503 isn't here: Unipile may have reached LinkedIn before it failed.
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Posting::Account(format!("Unipile couldn't use the LinkedIn account ({reason})"))
            }
            StatusCode::TOO_MANY_REQUESTS => Posting::Account("LinkedIn is rate limiting the account".into()),
            StatusCode::UNPROCESSABLE_ENTITY if ACCOUNT_LIMITS.contains(&kind.as_str()) => Posting::Account(format!("LinkedIn limited the account ({reason})")),
            StatusCode::NOT_FOUND => Posting::Post("LinkedIn couldn't find it. It may have been deleted".into()),
            _ => Posting::Post(reason),
        })
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url, Posting> {
        Url::parse_with_params(&format!("{}{path}", self.base), [("account_id", self.account.as_str())].iter().chain(query)).map_err(|e| Posting::Post(e.to_string()))
    }

    /// LinkedIn posts from the last month that match `keywords`, newest first.
    pub async fn search_posts(&self, keywords: &str, limit: u64) -> Result<Vec<Value>, Posting> {
        let search = json!({ "api": "classic", "category": "posts", "keywords": keywords, "sort_by": "date", "date_posted": "past_month" });
        let found = self.social(self.client.post(self.url("/linkedin/search", &[("limit", &limit.to_string())])?).json(&search)).await?;
        Ok(found["items"].as_array().cloned().unwrap_or_default())
    }

    /// A post by its urn (see post_urn), and up to 20 of its comments.
    pub async fn linkedin_post(&self, urn: &str) -> Result<(Value, Vec<Value>), Posting> {
        let post = self.social(self.client.get(self.url(&format!("/posts/{urn}"), &[])?)).await?;
        let comments = self.social(self.client.get(self.url(&format!("/posts/{urn}/comments"), &[("limit", "20")])?)).await;
        Ok((post, comments.map(|c| c["items"].as_array().cloned().unwrap_or_default()).unwrap_or_default()))
    }

    /// Posts `text` from the account and returns the post's link, or "" when Unipile doesn't give one.
    pub async fn publish(&self, text: &str) -> Result<String, Posting> {
        let form = reqwest::multipart::Form::new().text("account_id", self.account.clone()).text("text", text.to_string());
        let made = self.social(self.client.post(format!("{}/posts", self.base)).multipart(form)).await?;
        let Some(id) = made["post_id"].as_str().filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == ':')) else {
            return Ok(String::new());
        };
        // It's posted either way, so a failed lookup only leaves the link out.
        let Ok(url) = self.url(&format!("/posts/{id}"), &[]) else { return Ok(String::new()) };
        let share = self.social(self.client.get(url)).await.map(|p| p["share_url"].as_str().unwrap_or_default().to_string()).unwrap_or_default();
        Ok(share.split('?').next().unwrap_or_default().to_string())
    }

    /// Comments `text` on the post with this urn.
    pub async fn comment(&self, urn: &str, text: &str) -> Result<(), Posting> {
        let form = reqwest::multipart::Form::new().text("account_id", self.account.clone()).text("text", text.to_string());
        self.social(self.client.post(format!("{}/posts/{urn}/comments", self.base)).multipart(form)).await.map(|_| ())
    }

    /// LinkedIn's id for a company page or place (`kind` COMPANY or LOCATION), and its name there. A company has to match by name,
    /// so a search never runs on some other company's people.
    async fn search_parameter(&self, kind: &str, name: &str) -> Result<Option<(String, String)>, Posting> {
        let url = self.url("/linkedin/search/parameters", &[("type", kind), ("keywords", name), ("limit", "10")])?;
        let found = self.social(self.client.get(url)).await?;
        let items = found["items"].as_array().cloned().unwrap_or_default();
        let id_title = |i: &Value| Some((i["id"].as_str().filter(|id| !id.is_empty())?.to_string(), i["title"].as_str().unwrap_or_default().to_string()));
        if kind != "COMPANY" {
            return Ok(items.first().and_then(id_title));
        }
        let want = squash(name);
        let exact = items.iter().find(|i| squash(i["title"].as_str().unwrap_or_default()) == want);
        // A legal suffix often differs, like Smile Dental and Smile Dental Ltd.
        let close = || {
            items.iter().find(|i| {
                let title = squash(i["title"].as_str().unwrap_or_default());
                !title.is_empty() && !want.is_empty() && (title.starts_with(&want) || want.starts_with(&title))
            })
        };
        Ok(exact.or_else(close).and_then(id_title))
    }

    /// A page of up to 10 people through classic LinkedIn search, which every account has. `cursor` comes from the page before.
    /// Each page counts against the account's DAILY_SEARCH_PAGES.
    pub async fn search_people(&self, db: &PgPool, search: &PeopleSearch, cursor: Option<&str>) -> Result<People, Posting> {
        // Counted first, so a capped account doesn't look up the company either.
        if !take_search_page(db, &self.account).await.map_err(|e| Posting::Post(format!("Couldn't count today's LinkedIn searches: {e}")))? {
            return Err(Posting::Account(format!(
                "Splee has run {DAILY_SEARCH_PAGES} LinkedIn searches on this account today, the most it runs in a day. More can run tomorrow"
            )));
        }
        let mut body = json!({ "api": "classic", "category": "people" });
        let mut company = String::new();
        if !search.company.trim().is_empty() {
            let Some((id, title)) = self.search_parameter("COMPANY", search.company.trim()).await? else {
                return Err(Posting::Post("LinkedIn has no company page by that name".into()));
            };
            body["company"] = json!([id]);
            company = title;
        }
        if !search.location.trim().is_empty() {
            let Some((id, _)) = self.search_parameter("LOCATION", search.location.trim()).await? else {
                return Err(Posting::Post(format!("LinkedIn doesn't know the place {}", search.location.trim())));
            };
            body["location"] = json!([id]);
        }
        if !search.keywords.trim().is_empty() {
            body["keywords"] = json!(search.keywords.trim());
        }
        if !search.title.trim().is_empty() {
            body["advanced_keywords"] = json!({ "title": search.title.trim() });
        }
        let mut query = vec![("limit", "10")];
        query.extend(cursor.filter(|c| !c.is_empty()).map(|c| ("cursor", c)));
        let found = self.social(self.client.post(self.url("/linkedin/search", &query)?).json(&body)).await?;
        let items = found["items"].as_array().into_iter().flatten();
        let people = items.filter(|i| i["type"].as_str().is_none_or(|t| t == "PEOPLE")).filter_map(|i| person(i, &company)).collect();
        Ok(People { people, cursor: found["cursor"].as_str().filter(|c| !c.is_empty()).map(String::from) })
    }

    /// Someone's LinkedIn profile, with their experience, by profile link, public id or provider id.
    pub async fn person(&self, identifier: &str) -> Result<Value, Posting> {
        let id = profile_id(identifier).unwrap_or(identifier.trim());
        if id.is_empty() || !id.chars().all(|c| c.is_alphanumeric() || "-_%".contains(c)) {
            return Err(Posting::Post(format!("{identifier} isn't a LinkedIn profile link or id")));
        }
        self.social(self.client.get(self.url(&format!("/users/{id}"), &[("linkedin_sections", "experience_preview")])?)).await
    }

    /// The lead's LinkedIn profile, with their `provider_id`.
    async fn profile(&self, url: &str) -> Result<Value, Failure> {
        let id = profile_id(url).ok_or_else(|| Failure::lead("The LinkedIn URL isn't a profile link"))?;
        let profile = self.call(self.client.get(format!("{}/users/{id}?account_id={}", self.base, self.account))).await?;
        if profile["provider_id"].as_str().is_none_or(str::is_empty) {
            return Err(Failure::Account(anyhow::anyhow!("Unipile didn't return the profile's id")));
        }
        Ok(profile)
    }

    /// `invited_at` is when the lead's connect note went out, for a message step.
    async fn send(&self, step: &LinkedinMessage, profile: &Value, invited_at: Option<Ts>) -> Result<Step, Failure> {
        let provider_id = profile["provider_id"].as_str().unwrap_or_default();
        let connected = profile["network_distance"] == "FIRST_DEGREE" || profile["is_relationship"] == true;

        if step.kind == "connect" {
            if connected {
                return Ok(Step::Connected);
            }
            if profile["invitation"]["type"] == "SENT" && profile["invitation"]["status"] == "PENDING" {
                return Ok(Step::AlreadyInvited);
            }
            let invite = json!({ "account_id": self.account, "provider_id": provider_id, "message": step.body.chars().take(300).collect::<String>() });
            return match self.call(self.client.post(format!("{}/users/invite", self.base)).json(&invite)).await {
                Err(Failure::Lead { kind, .. }) if kind == "errors/already_invited_recently" => Ok(Step::AlreadyInvited),
                result => result.map(|_| Step::Sent),
            };
        }

        if !connected {
            return match invited_at {
                Some(at) if at > Utc::now() - chrono::Duration::days(ACCEPT_DAYS) => Ok(Step::Waiting),
                Some(_) => Err(Failure::lead("They haven't accepted the connect note in 3 weeks")),
                None => Err(Failure::lead("You aren't connected on LinkedIn, and the connect note wasn't sent")),
            };
        }
        let form = reqwest::multipart::Form::new()
            .text("account_id", self.account.clone())
            .text("attendees_ids", provider_id.to_string())
            .text("text", step.body.clone());
        self.call(self.client.post(format!("{}/chats", self.base)).multipart(form)).await.map(|_| Step::Sent)
    }
}

#[derive(sqlx::FromRow)]
struct Sender {
    project_id: Uuid,
    unipile_dsn: String,
    unipile_token_enc: String,
    unipile_account_id: String,
    timezone: String,
    send_days: Vec<i32>,
    send_start_hour: i32,
    send_end_hour: i32,
}

/// What a webhook post from Unipile means for the project's leads: someone writing back stops their sequence and emails,
/// like an email reply, and someone accepting the connect note has their first message come due. Returns the lead who
/// wrote back, and what they wrote, when it stopped them.
pub async fn on_event(db: &PgPool, project: Uuid, account: &str, body: &Value) -> sqlx::Result<Option<(Lead, String)>> {
    if body["account_id"] != account {
        return Ok(None);
    }
    match body["event"].as_str() {
        Some("message_received") => {
            let sender = &body["sender"];
            let from = sender["attendee_provider_id"].as_str().unwrap_or_default();
            // The account's own messages come through too, sent by Splee or from LinkedIn.
            if from.is_empty() || body["account_info"]["user_id"] == from {
                return Ok(None);
            }
            let Some(lead) = find_lead(db, project, from, sender["attendee_profile_url"].as_str().and_then(profile_id)).await? else {
                return Ok(None);
            };
            if !["new", "queued", "contacted"].contains(&lead.status.as_str()) {
                return Ok(None);
            }
            let stopped = crate::routes::linkedin::stop_for_reply(db, &lead).await?;
            Ok(Some((stopped, body["message"].as_str().unwrap_or_default().to_string())))
        }
        Some("new_relation") => {
            let (from, public_id) = (body["user_provider_id"].as_str().unwrap_or_default(), body["user_public_identifier"].as_str());
            if let Some(lead) = find_lead(db, project, from, public_id).await? {
                sqlx::query(
                    "update linkedin_messages set due_at = now()
                     where id = (select id from linkedin_messages where lead_id = $1 and status = 'todo' order by step limit 1)
                       and kind = 'message' and due_at > now()",
                )
                .bind(lead.id)
                .execute(db)
                .await?;
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// The project's lead with this LinkedIn id, else the one whose profile URL has this public id.
async fn find_lead(db: &PgPool, project: Uuid, provider_id: &str, public_id: Option<&str>) -> sqlx::Result<Option<Lead>> {
    if !provider_id.is_empty() {
        let lead = sqlx::query_as::<_, Lead>("select * from leads where project_id = $1 and linkedin_provider_id = $2 order by created_at limit 1")
            .bind(project)
            .bind(provider_id)
            .fetch_optional(db)
            .await?;
        if lead.is_some() {
            return Ok(lead);
        }
    }
    let Some(public_id) = public_id else { return Ok(None) };
    let leads = sqlx::query_as::<_, Lead>("select * from leads where project_id = $1 and linkedin_url ilike '%/in/' || $2 || '%' order by created_at")
        .bind(project)
        .bind(public_id)
        .fetch_all(db)
        .await?;
    // The match above also takes sam-lee for sam.
    Ok(leads.into_iter().find(|l| profile_id(&l.linkedin_url).is_some_and(|id| id.eq_ignore_ascii_case(public_id))))
}

/// Connected, unpaused projects whose LinkedIn account hasn't had a Unipile failure in the last hour.
async fn senders(db: &PgPool) -> sqlx::Result<Vec<Sender>> {
    sqlx::query_as::<_, Sender>(
        "select s.project_id, s.unipile_dsn, s.unipile_token_enc, s.unipile_account_id, s.timezone, s.send_days, s.send_start_hour,
                s.send_end_hour
         from project_settings s join projects p on p.id = s.project_id
         where s.unipile_token_enc <> '' and not s.paused and p.org_id is not null
           and not exists (select 1 from project_settings o where o.unipile_account_id = s.unipile_account_id
                           and o.unipile_failed_at > now() - interval '1 hour')",
    )
    .fetch_all(db)
    .await
}

/// One due step per connected project each tick, inside its send window, so a day's sends spread out instead of going
/// in a burst. A Unipile failure stops every project on that LinkedIn account for an hour.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    // Projects can share a LinkedIn account, and a failure on one holds them all for the hour.
    let mut failed = HashSet::new();
    for s in senders(&state.db).await? {
        if failed.contains(&s.unipile_account_id) {
            continue;
        }
        if !crate::mail::in_window(s.timezone.parse().unwrap_or(Tz::UTC), &s.send_days, s.send_start_hour, s.send_end_hour, Utc::now()) {
            continue;
        }
        let token = match crate::crypto::decrypt(&state.cfg.app_secret, &s.unipile_token_enc) {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!(project_id = %s.project_id, error = %e, "couldn't decrypt the Unipile token");
                continue;
            }
        };
        let unipile = Unipile::new(&s.unipile_dsn, &token, &s.unipile_account_id)?;
        let result = match send_next(&state.db, &unipile, s.project_id).await {
            Ok(None) => continue,
            Ok(Some(result)) => result,
            Err(e) => {
                tracing::error!(project_id = %s.project_id, error = ?e, "linkedin sending failed");
                continue;
            }
        };
        if result.is_err() {
            failed.insert(s.unipile_account_id.clone());
        }
        hooks::record(&state.db, s.project_id, Alert::Unipile, &s.unipile_token_enc, &result).await;
    }
    Ok(())
}

/// Sends the project's longest-due step. None when there's nothing to send, else whether Unipile worked.
async fn send_next(db: &PgPool, unipile: &Unipile, project: Uuid) -> anyhow::Result<Option<anyhow::Result<()>>> {
    let sent_today: i64 = sqlx::query_scalar(
        "select count(*) from linkedin_messages m join project_settings s on s.project_id = m.project_id
         where s.unipile_account_id = $1 and m.sent_via = 'unipile' and m.done_at > now() - interval '1 day'",
    )
    .bind(&unipile.account)
    .fetch_one(db)
    .await?;
    if sent_today >= DAILY_SENDS {
        return Ok(None);
    }
    let Some(step) = sqlx::query_as::<_, LinkedinMessage>(
        "select m.* from linkedin_messages m join leads l on l.id = m.lead_id
         where m.project_id = $1 and m.status = 'todo' and m.due_at <= now() and m.send_error is null
           and l.linkedin_url <> '' and l.status in ('new', 'queued', 'contacted')
         order by m.due_at limit 1",
    )
    .bind(project)
    .fetch_optional(db)
    .await?
    else {
        return Ok(None);
    };
    let url: String = sqlx::query_scalar("select linkedin_url from leads where id = $1").bind(step.lead_id).fetch_one(db).await?;
    let invited_at: Option<Ts> =
        sqlx::query_scalar("select max(done_at) from linkedin_messages where lead_id = $1 and kind = 'connect' and status = 'done'")
            .bind(step.lead_id)
            .fetch_one(db)
            .await?;

    let sent = match unipile.profile(&url).await {
        Ok(profile) => {
            // How Unipile names the lead when they write back.
            sqlx::query("update leads set linkedin_provider_id = $2 where id = $1")
                .bind(step.lead_id)
                .bind(profile["provider_id"].as_str())
                .execute(db)
                .await?;
            unipile.send(&step, &profile, invited_at).await
        }
        Err(e) => Err(e),
    };
    match sent {
        Ok(Step::Sent) => {
            mark(db, &step, "done", None).await?;
            sqlx::query("update linkedin_messages set sent_via = 'unipile' where id = $1").bind(step.id).execute(db).await?;
        }
        Ok(Step::AlreadyInvited) => {
            mark(db, &step, "done", None).await?;
        }
        // Already connected, so the first message comes due now.
        Ok(Step::Connected) => {
            mark(db, &step, "skipped", None).await?;
        }
        Ok(Step::Waiting) => {
            sqlx::query("update linkedin_messages set due_at = now() + interval '1 day' where id = $1").bind(step.id).execute(db).await?;
        }
        Err(Failure::Lead { reason, .. }) => {
            sqlx::query("update linkedin_messages set send_error = $2 where id = $1").bind(step.id).bind(reason).execute(db).await?;
        }
        Err(Failure::Account(e)) => return Ok(Some(Err(e))),
    }
    Ok(Some(Ok(())))
}

/// Unipile's people search as the docs describe it, for tests here and in the routes and agent tools that use it. Smile Dental
/// has two pages of people, Blocked Dental one person, and any other company has no LinkedIn page.
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, Query, State};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::{Value, json};

    pub type Calls = Arc<Mutex<Vec<String>>>;

    pub async fn people() -> (String, Calls) {
        async fn parameters(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            let items = match (q["type"].as_str(), q["keywords"].as_str()) {
                ("COMPANY", "Smile Dental") => json!([{ "id": "111", "title": "Smile Dental Ltd" }, { "id": "999", "title": "Smiles Inc" }]),
                ("COMPANY", "Blocked Dental") => json!([{ "id": "222", "title": "Blocked Dental" }]),
                ("COMPANY", _) => json!([{ "id": "5", "title": "Totally Other" }]),
                ("LOCATION", "Leeds") => json!([{ "id": "102", "title": "Leeds, England, United Kingdom" }]),
                _ => json!([]),
            };
            Json(json!({ "object": "LinkedinSearchParametersList", "items": items }))
        }
        async fn search(State(calls): State<Calls>, Query(q): Query<HashMap<String, String>>, Json(body): Json<Value>) -> Json<Value> {
            let cursor = q.get("cursor").cloned().unwrap_or_default();
            calls.lock().unwrap().push(format!("search {} {} {cursor} {body}", q["account_id"], q["limit"]));
            let person = |id: &str, name: &str, headline: &str, slug: &str| {
                json!({ "type": "PEOPLE", "id": id, "name": name, "headline": headline, "location": "Leeds, England, United Kingdom",
                        "network_distance": "DISTANCE_2", "profile_url": format!("https://www.linkedin.com/in/{slug}?miniProfileUrn=x") })
            };
            let (items, next) = match (body["company"][0].as_str(), cursor.as_str()) {
                (Some("111"), "") => (
                    json!([person("P-SAM", "Sam Lee", "Owner at Smile Dental", "sam-lee"), person("P-HID", "LinkedIn Member", "Dentist", "x"),
                           person("P-ANA", "Ana Diaz", "Practice Manager", "ana-diaz")]),
                    json!("C2"),
                ),
                (Some("111"), "C2") => (json!([person("P-BO", "Bo Chen", "Dentist and Owner at Smile Dental", "bo-chen")]), Value::Null),
                (Some("222"), _) => (json!([person("P-CY", "Cy Doe", "Founder at Blocked Dental", "cy-doe")]), Value::Null),
                _ => (json!([]), Value::Null),
            };
            Json(json!({ "object": "LinkedinSearch", "items": items, "config": {}, "paging": { "start": 0, "page_count": 10, "total_count": 4 },
                         "cursor": next }))
        }
        async fn user(Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            assert_eq!((id.as_str(), q["linkedin_sections"].as_str()), ("sam-lee", "experience_preview"));
            Json(json!({ "provider_id": "P-SAM", "public_identifier": "sam-lee", "first_name": "Sam", "last_name": "Lee", "headline": "Owner",
                         "location": "Leeds", "websites": ["https://smile.co.uk"],
                         "work_experience": [{ "company": "Smile Dental", "position": "Owner", "start": "1/2019", "end": null, "skills": [] },
                                             { "company": "NHS", "position": "Dentist", "start": "1/2010", "end": "1/2019", "skills": [] }] }))
        }
        let calls = Calls::default();
        let app = Router::new()
            .route("/api/v1/linkedin/search/parameters", get(parameters))
            .route("/api/v1/linkedin/search", post(search))
            .route("/api/v1/users/{id}", get(user))
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, calls)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::{Multipart, Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::{Json, Router};

    use super::*;

    #[test]
    fn only_unipile_hosts_are_taken() {
        let base = Some("https://api8.unipile.com:13851/api/v1".to_string());
        assert_eq!(base_url("api8.unipile.com:13851"), base);
        assert_eq!(base_url(" https://API8.unipile.com:13851/api/v1/accounts "), base);
        for dsn in ["http://api8.unipile.com:13851", "127.0.0.1:8080", "unipile.com.example.com", "https://example.com/unipile.com", ""] {
            assert_eq!(base_url(dsn), None, "{dsn}");
        }
    }

    #[test]
    fn profile_ids_come_from_the_url() {
        assert_eq!(profile_id("https://www.linkedin.com/in/sam-lee/"), Some("sam-lee"));
        assert_eq!(profile_id("linkedin.com/in/sam-lee?trk=x"), Some("sam-lee"));
        assert_eq!(profile_id("https://www.linkedin.com/company/acme"), None);
        assert_eq!(profile_id("https://www.linkedin.com/in/"), None);
    }

    #[test]
    fn linkedin_post_links_become_urns() {
        let activity = Some("urn:li:activity:7236734771807019010".to_string());
        for link in [
            "https://www.linkedin.com/feed/update/urn:li:activity:7236734771807019010/",
            "https://www.linkedin.com/feed/update/urn%3Ali%3Aactivity%3A7236734771807019010?utm_source=share",
            "https://www.linkedin.com/posts/sam-lee_cold-email-activity-7236734771807019010-AbCd?utm_source=share",
            "linkedin.com/posts/sam-lee_a-b-activity-7236734771807019010-x",
            "urn:li:activity:7236734771807019010",
        ] {
            assert_eq!(post_urn(link), activity, "{link}");
        }
        assert_eq!(post_urn("https://www.linkedin.com/feed/update/urn:li:ugcPost:7336013677930006817/").as_deref(), Some("urn:li:ugcPost:7336013677930006817"));
        for link in ["https://www.linkedin.com/in/sam-lee/", "https://example.com/feed/update/urn:li:activity:7236734771807019010/", "urn:li:activity:12", ""] {
            assert_eq!(post_urn(link), None, "{link}");
        }
        assert_eq!(post_link("urn:li:activity:1"), "https://www.linkedin.com/feed/update/urn:li:activity:1/");
    }

    /// Unipile's post routes as the docs describe them. The text picks how posting goes.
    async fn fake_unipile_posts() -> (String, Calls) {
        async fn publish(State(calls): State<Calls>, mut form: Multipart) -> (StatusCode, Json<Value>) {
            let mut fields = Vec::new();
            while let Some(field) = form.next_field().await.unwrap() {
                let name = field.name().unwrap().to_owned();
                fields.push(format!("{name}={}", field.text().await.unwrap()));
            }
            match fields.last().map(String::as_str) {
                Some("text=expired") => (StatusCode::UNAUTHORIZED, Json(json!({ "status": 401, "type": "errors/disconnected_account", "title": "Disconnected account" }))),
                Some("text=slow") => (StatusCode::GATEWAY_TIMEOUT, Json(json!({ "status": 504, "type": "errors/request_timeout", "title": "Request timed out" }))),
                Some("text=down") => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": 503, "type": "errors/provider_error", "title": "Provider error" }))),
                Some("text=limit") => (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "status": 422, "type": "errors/limit_exceeded", "title": "Limit exceeded" }))),
                _ => {
                    calls.lock().unwrap().push(format!("post {}", fields.join(" ")));
                    (StatusCode::CREATED, Json(json!({ "object": "PostCreated", "post_id": "7400000000000000001" })))
                }
            }
        }
        async fn one(Path(id): Path<String>) -> (StatusCode, Json<Value>) {
            match id.as_str() {
                "7400000000000000001" => (StatusCode::OK, Json(json!({ "share_url": "https://www.linkedin.com/posts/acme_a-lesson-activity-7400000000000000001-Ab?utm_source=x" }))),
                _ => (StatusCode::NOT_FOUND, Json(json!({ "status": 404, "type": "errors/resource_not_found", "title": "Post not found" }))),
            }
        }
        async fn comment(State(calls): State<Calls>, Path(id): Path<String>, mut form: Multipart) -> (StatusCode, Json<Value>) {
            if id != "urn:li:activity:7236734771807019010" {
                return (StatusCode::NOT_FOUND, Json(json!({ "status": 404, "type": "errors/resource_not_found", "title": "Post not found" })));
            }
            let mut fields = Vec::new();
            while let Some(field) = form.next_field().await.unwrap() {
                let name = field.name().unwrap().to_owned();
                fields.push(format!("{name}={}", field.text().await.unwrap()));
            }
            calls.lock().unwrap().push(format!("comment {id} {}", fields.join(" ")));
            (StatusCode::CREATED, Json(json!({ "object": "CommentSent" })))
        }
        async fn search(State(calls): State<Calls>, axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>, Json(body): Json<Value>) -> Json<Value> {
            calls.lock().unwrap().push(format!("search {} {} {} {}", q["account_id"], q["limit"], body["category"], body["keywords"]));
            Json(json!({ "object": "LinkedinSearch", "items": [{ "type": "POST", "social_id": "urn:li:activity:7236734771807019010", "text": "Any tips?" }] }))
        }
        let calls = Calls::default();
        let app = Router::new()
            .route("/api/v1/posts", post(publish))
            .route("/api/v1/posts/{id}", get(one))
            .route("/api/v1/posts/{id}/comments", post(comment))
            .route("/api/v1/linkedin/search", post(search))
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, calls)
    }

    #[tokio::test]
    async fn linkedin_posts_and_comments_go_out_and_failures_say_whose_problem_it_is() {
        let (base, calls) = fake_unipile_posts().await;
        let unipile = Unipile::new(&base, "TOKEN", "ACC").unwrap();
        assert_eq!(unipile.search_posts("cold email", 10).await.ok().unwrap()[0]["text"], "Any tips?");
        assert_eq!(unipile.publish("A lesson").await.ok().unwrap(), "https://www.linkedin.com/posts/acme_a-lesson-activity-7400000000000000001-Ab");
        unipile.comment("urn:li:activity:7236734771807019010", "Try this").await.ok().unwrap();
        assert_eq!(*calls.lock().unwrap(), [
            "search ACC 10 \"posts\" \"cold email\"",
            "post account_id=ACC text=A lesson",
            "comment urn:li:activity:7236734771807019010 account_id=ACC text=Try this",
        ]);

        assert!(matches!(unipile.comment("urn:li:activity:7236734771807019099", "Hi").await, Err(Posting::Post(m)) if m.contains("couldn't find")));
        assert!(matches!(unipile.publish("expired").await, Err(Posting::Account(_))));
        assert!(matches!(unipile.publish("limit").await, Err(Posting::Account(_))));
        // It may have gone out, so it fails rather than waiting to go out again.
        assert!(matches!(unipile.publish("slow").await, Err(Posting::Post(m)) if m == "Request timed out"));
        assert!(matches!(unipile.publish("down").await, Err(Posting::Post(_))));
    }

    type Calls = Arc<Mutex<Vec<String>>>;

    /// Unipile as the docs describe it: sam hasn't accepted, ana is a connection, anyone else isn't found.
    async fn fake_unipile() -> (String, Calls) {
        async fn user(Path(id): Path<String>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
            assert_eq!(headers["x-api-key"], "TOKEN");
            match id.as_str() {
                "sam" => (StatusCode::OK, Json(json!({ "provider_id": "P-SAM", "network_distance": "SECOND_DEGREE" }))),
                "ana" => (StatusCode::OK, Json(json!({ "provider_id": "P-ANA", "network_distance": "FIRST_DEGREE" }))),
                _ => (StatusCode::NOT_FOUND, Json(json!({ "status": 404, "type": "errors/resource_not_found", "title": "User not found" }))),
            }
        }
        async fn invite(State(calls): State<Calls>, Json(body): Json<Value>) -> Json<Value> {
            calls.lock().unwrap().push(format!("invite {} {}", body["provider_id"], body["message"]));
            Json(json!({ "object": "UserInvitationSent" }))
        }
        async fn chat(State(calls): State<Calls>, mut form: Multipart) -> Json<Value> {
            let mut fields = Vec::new();
            while let Some(field) = form.next_field().await.unwrap() {
                let name = field.name().unwrap().to_owned();
                fields.push(format!("{name}={}", field.text().await.unwrap()));
            }
            calls.lock().unwrap().push(format!("chat {}", fields.join(" ")));
            Json(json!({ "object": "ChatStarted", "chat_id": "c1" }))
        }
        async fn webhook(State(calls): State<Calls>, Json(body): Json<Value>) -> Json<Value> {
            let mut calls = calls.lock().unwrap();
            calls.push(format!("webhook {} {} {} {} {}", body["source"], body["events"], body["account_ids"], body["request_url"], body["headers"]));
            Json(json!({ "object": "WebhookCreated", "webhook_id": format!("wh{}", calls.len()) }))
        }
        async fn unhook(State(calls): State<Calls>, Path(id): Path<String>) -> Json<Value> {
            calls.lock().unwrap().push(format!("unhook {id}"));
            Json(json!({ "object": "WebhookDeleted" }))
        }
        let calls = Calls::default();
        let app = Router::new()
            .route("/api/v1/users/{id}", get(user))
            .route("/api/v1/users/invite", post(invite))
            .route("/api/v1/chats", post(chat))
            .route("/api/v1/webhooks", post(webhook))
            .route("/api/v1/webhooks/{id}", axum::routing::delete(unhook))
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, calls)
    }

    async fn lead_with_sequence(db: &PgPool, project: Uuid, profile: &str) -> Vec<Uuid> {
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, linkedin_url, status) values ($1, $2, 'contacted') returning id")
            .bind(project)
            .bind(format!("https://www.linkedin.com/in/{profile}/"))
            .fetch_one(db)
            .await
            .unwrap();
        sqlx::query_scalar(
            "insert into linkedin_messages (lead_id, project_id, step, kind, body, due_at)
             select $1, $2, s, case when s = 1 then 'connect' else 'message' end, 'Hi ' || s, case when s = 1 then now() end
             from generate_series(1, 3) s returning id",
        )
        .bind(lead)
        .bind(project)
        .fetch_all(db)
        .await
        .unwrap()
    }

    async fn step(db: &PgPool, id: Uuid) -> LinkedinMessage {
        sqlx::query_as("select * from linkedin_messages where id = $1").bind(id).fetch_one(db).await.unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn due_steps_go_out_through_unipile_and_messages_wait_for_the_connection(db: PgPool) {
        let (base, calls) = fake_unipile().await;
        let unipile = Unipile::new(&base, "TOKEN", "ACC").unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let sent = || async { send_next(&db, &unipile, project).await.unwrap().map(|r| r.is_ok()) };

        // Sam's connect note goes out as an invitation, and his first message comes due 3 days later.
        let sam = lead_with_sequence(&db, project, "sam").await;
        assert_eq!(sent().await, Some(true));
        let (connect, message) = (step(&db, sam[0]).await, step(&db, sam[1]).await);
        assert_eq!((connect.status.as_str(), connect.sent_via.as_str()), ("done", "unipile"));
        assert!(message.due_at.unwrap() > Utc::now() + chrono::Duration::days(2));
        assert_eq!(sent().await, None);

        // He hasn't accepted, so the message waits a day instead of going out.
        sqlx::query("update linkedin_messages set due_at = now() where id = $1").bind(sam[1]).execute(&db).await.unwrap();
        assert_eq!(sent().await, Some(true));
        let message = step(&db, sam[1]).await;
        assert_eq!(message.status, "todo");
        assert!(message.due_at.unwrap() > Utc::now() + chrono::Duration::hours(23));

        // Ana is already a connection: her connect note is skipped and her message goes out as a chat.
        let ana = lead_with_sequence(&db, project, "ana").await;
        assert_eq!(sent().await, Some(true));
        assert_eq!(step(&db, ana[0]).await.status, "skipped");
        assert_eq!(sent().await, Some(true));
        assert_eq!(step(&db, ana[1]).await.sent_via, "unipile");

        // A profile LinkedIn doesn't find is left for the user, with the reason.
        let gone = lead_with_sequence(&db, project, "gone").await;
        assert_eq!(sent().await, Some(true));
        let failed = step(&db, gone[0]).await;
        assert_eq!((failed.status.as_str(), failed.send_error.as_deref()), ("todo", Some("User not found")));
        assert_eq!(sent().await, None);

        assert_eq!(*calls.lock().unwrap(), ["invite \"P-SAM\" \"Hi 1\"", "chat account_id=ACC attendees_ids=P-ANA text=Hi 2"]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_failure_holds_every_project_on_the_same_linkedin_account(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let mut projects = Vec::new();
        for account in ["ACC", "ACC", "OTHER"] {
            let project: Uuid =
                sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
            sqlx::query("insert into project_settings (project_id, paused, unipile_token_enc, unipile_account_id) values ($1, false, 'x', $2)")
                .bind(project)
                .bind(account)
                .execute(&db)
                .await
                .unwrap();
            projects.push(project);
        }
        let sending = || async { senders(&db).await.unwrap().into_iter().map(|s| s.project_id).collect::<Vec<_>>() };
        assert_eq!(sending().await.len(), 3);

        sqlx::query("update project_settings set unipile_failed_at = now() - interval '10 minutes' where project_id = $1")
            .bind(projects[0])
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(sending().await, [projects[2]]);
    }

    #[tokio::test]
    async fn webhooks_cover_replies_and_new_connections_on_the_one_account() {
        let (base, calls) = fake_unipile().await;
        let ids = add_webhooks(&base, "TOKEN", "ACC", "https://app.example/api/unipile/abc").await.unwrap();
        remove_webhooks(&base, "TOKEN", &ids).await;
        assert_eq!(*calls.lock().unwrap(), [
            r#"webhook "messaging" ["message_received"] ["ACC"] "https://app.example/api/unipile/abc" [{"key":"Content-Type","value":"application/json"}]"#,
            r#"webhook "users" ["new_relation"] ["ACC"] "https://app.example/api/unipile/abc" [{"key":"Content-Type","value":"application/json"}]"#,
            "unhook wh1",
            "unhook wh2",
        ]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_linkedin_reply_stops_the_lead_and_a_new_connection_brings_the_message_due(db: PgPool) {
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let sam = lead_with_sequence(&db, project, "sam").await;
        let lead: Uuid = sqlx::query_scalar("select lead_id from linkedin_messages where id = $1").bind(sam[0]).fetch_one(&db).await.unwrap();
        sqlx::query("update linkedin_messages set status = 'done', done_at = now() where id = $1").bind(sam[0]).execute(&db).await.unwrap();
        sqlx::query("update linkedin_messages set due_at = now() + interval '3 days' where id = $1").bind(sam[1]).execute(&db).await.unwrap();

        // Accepting the connect note: Unipile names them by public id, before Splee has their LinkedIn id.
        let accepted = json!({ "event": "new_relation", "account_id": "ACC", "user_provider_id": "P-SAM", "user_public_identifier": "Sam" });
        assert!(on_event(&db, project, "ACC", &accepted).await.unwrap().is_none());
        assert!(step(&db, sam[1]).await.due_at.unwrap() <= Utc::now());

        sqlx::query("update leads set linkedin_provider_id = 'P-SAM' where id = $1").bind(lead).execute(&db).await.unwrap();
        let message = |from: &str, account: &str| {
            json!({ "event": "message_received", "account_id": account, "account_info": { "user_id": "P-ME" },
                    "sender": { "attendee_provider_id": from, "attendee_profile_url": "https://www.linkedin.com/in/ACoAAB" },
                    "message": "Sure, send it over" })
        };
        // The account's own messages, and posts for another account, change nothing.
        for (from, account) in [("P-ME", "ACC"), ("P-SAM", "OTHER")] {
            assert!(on_event(&db, project, "ACC", &message(from, account)).await.unwrap().is_none());
        }
        assert_eq!(step(&db, sam[1]).await.status, "todo");

        let (stopped, text) = on_event(&db, project, "ACC", &message("P-SAM", "ACC")).await.unwrap().unwrap();
        assert_eq!((stopped.status.as_str(), text.as_str()), ("replied", "Sure, send it over"));
        assert_eq!((step(&db, sam[1]).await.status, step(&db, sam[2]).await.status), ("skipped".into(), "skipped".into()));
        // A second message from someone who already replied doesn't alert again.
        assert!(on_event(&db, project, "ACC", &message("P-SAM", "ACC")).await.unwrap().is_none());
    }

    #[test]
    fn roles_become_one_linkedin_title_search() {
        assert_eq!(any_title(&["Owner".into(), " Practice Manager ".into(), "".into(), "\"CEO\"".into()]), "Owner OR \"Practice Manager\" OR CEO");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn people_search_pages_through_classic_linkedin_search(db: PgPool) {
        let (base, calls) = super::fake::people().await;
        let unipile = Unipile::new(&base, "TOKEN", "ACC").unwrap();
        let search = PeopleSearch { company: "Smile Dental".into(), title: "Owner OR Founder".into(), location: "Leeds".into(), ..PeopleSearch::default() };

        let first = unipile.search_people(&db, &search, None).await.ok().unwrap();
        // LinkedIn hides the name of someone outside the network, so they're left out.
        assert_eq!(first.people.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Sam Lee", "Ana Diaz"]);
        assert_eq!(first.people[0], Person {
            name: "Sam Lee".into(),
            headline: "Owner at Smile Dental".into(),
            company: "Smile Dental Ltd".into(),
            location: "Leeds, England, United Kingdom".into(),
            linkedin_url: "https://www.linkedin.com/in/sam-lee".into(),
            provider_id: "P-SAM".into(),
        });
        let second = unipile.search_people(&db, &search, first.cursor.as_deref()).await.ok().unwrap();
        assert_eq!((second.people[0].name.as_str(), second.cursor), ("Bo Chen", None));
        let body = r#"{"advanced_keywords":{"title":"Owner OR Founder"},"api":"classic","category":"people","company":["111"],"location":["102"]}"#;
        assert_eq!(*calls.lock().unwrap(), [format!("search ACC 10  {body}"), format!("search ACC 10 C2 {body}")]);

        // A company LinkedIn has no page for is never searched as some other company.
        let nowhere = PeopleSearch { company: "Nowhere Ltd".into(), ..PeopleSearch::default() };
        assert!(matches!(unipile.search_people(&db, &nowhere, None).await, Err(Posting::Post(m)) if m == "LinkedIn has no company page by that name"));
        assert_eq!(calls.lock().unwrap().len(), 2);

        let profile = unipile.person("https://www.linkedin.com/in/sam-lee/?trk=x").await.ok().unwrap();
        assert_eq!(profile["provider_id"], "P-SAM");
        assert!(matches!(unipile.person("../accounts").await, Err(Posting::Post(_))));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn people_search_stops_at_the_daily_cap_for_the_account(db: PgPool) {
        let (base, calls) = super::fake::people().await;
        let unipile = Unipile::new(&base, "TOKEN", "ACC").unwrap();
        let search = PeopleSearch { company: "Blocked Dental".into(), ..PeopleSearch::default() };
        sqlx::query("insert into linkedin_searches (account_id, day, pages) values ('ACC', (now() at time zone 'utc')::date, $1), ('ACC', '2020-01-01', 0)")
            .bind(DAILY_SEARCH_PAGES - 1)
            .execute(&db)
            .await
            .unwrap();

        assert_eq!(unipile.search_people(&db, &search, None).await.ok().unwrap().people.len(), 1);
        let capped = unipile.search_people(&db, &search, None).await;
        assert!(matches!(capped, Err(Posting::Account(m)) if m.contains("100 LinkedIn searches on this account today")));
        assert_eq!(calls.lock().unwrap().len(), 1);

        // Another account has its own day.
        let other = Unipile::new(&base, "TOKEN", "OTHER").unwrap();
        assert!(other.search_people(&db, &search, None).await.is_ok());
        let pages: Vec<i32> = sqlx::query_scalar("select pages from linkedin_searches order by account_id, day").fetch_all(&db).await.unwrap();
        assert_eq!(pages, [0, DAILY_SEARCH_PAGES, 1]);
    }
}
