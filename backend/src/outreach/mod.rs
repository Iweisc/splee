//! Brings campaigns over from another outreach tool with the user's API key, so someone switching to Splee neither emails the
//! same people twice nor loses track of who replied. Each picked campaign becomes a draft campaign with the tool's emails as
//! its steps (segment_steps) and its leads, with what the tool knows about them: who was contacted, replied, bounced or
//! unsubscribed. Bounces and unsubscribes, and optionally the tool's own blocklist, go on the suppression list.
//!
//! routes/outreach.rs checks the key, lists the campaigns and queues an import. The worker here runs it, one campaign after
//! another, writing each campaign's counts to outreach_imports as it goes. Running one again adds only what's new.

pub mod instantly;
pub mod lemlist;
pub mod smartlead;

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::routes::util_input;
use crate::state::AppState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Instantly,
    Smartlead,
    Lemlist,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Instantly => "instantly",
            Tool::Smartlead => "smartlead",
            Tool::Lemlist => "lemlist",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Instantly => "Instantly",
            Tool::Smartlead => "Smartlead",
            Tool::Lemlist => "lemlist",
        }
    }

    pub fn parse(name: &str) -> Option<Tool> {
        [Tool::Instantly, Tool::Smartlead, Tool::Lemlist].into_iter().find(|t| t.name() == name)
    }

    fn api(self) -> &'static str {
        match self {
            Tool::Instantly => instantly::API,
            Tool::Smartlead => smartlead::API,
            Tool::Lemlist => lemlist::API,
        }
    }
}

/// A campaign in the tool. `leads` is None when the tool wouldn't say.
#[derive(Debug, Clone, Serialize)]
pub struct Campaign {
    pub id: String,
    pub name: String,
    pub status: String,
    pub leads: Option<i64>,
}

/// An email of a campaign's sequence, already turned into Splee's fields and plain text.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    pub delay_days: i32,
    pub subject: String,
    pub body: String,
}

/// What the tool says about a lead.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum State {
    #[default]
    New,
    Contacted,
    Replied,
    Interested,
    MeetingBooked,
    NotInterested,
    Bounced,
    Unsubscribed,
    /// On the tool's blocklist, which Smartlead tells per lead.
    Blocked,
}

impl State {
    /// The lead's status in Splee. Anything past New was emailed, so the writer leaves it alone.
    fn status(self) -> &'static str {
        match self {
            State::New => "new",
            State::Contacted => "contacted",
            State::Replied => "replied",
            State::Interested => "hot",
            State::MeetingBooked => "meeting_booked",
            State::NotInterested => "not_interested",
            State::Bounced => "bounced",
            State::Unsubscribed => "unsubscribed",
            State::Blocked => "do_not_contact",
        }
    }

    /// The reason the address goes on the suppression list, if it does.
    fn suppression(self) -> Option<&'static str> {
        match self {
            State::Bounced => Some("bounce"),
            State::Unsubscribed => Some("unsubscribe"),
            State::Blocked => Some("manual"),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Lead {
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub title: String,
    pub company_name: String,
    pub company_domain: String,
    pub phone: String,
    pub linkedin_url: String,
    pub city: String,
    /// An icebreaker or personalization line written for this lead, which becomes its hook for the writer.
    pub hook: String,
    /// Custom fields Splee has no column for, kept in the lead's notes.
    pub custom: Vec<(String, String)>,
    pub state: State,
    pub last_contacted_at: Option<DateTime<Utc>>,
}

impl Lead {
    /// Puts a tool's field where Splee keeps it, by its name in any of the tools' spellings ("firstName", "first_name",
    /// "linkedin_profile"). A field Splee has no place for, or one already filled, goes to `custom`, unless it's empty.
    pub fn set(&mut self, key: &str, value: &Value) {
        let text = match value {
            Value::String(s) => s.trim().to_string(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => return,
        };
        if text.is_empty() {
            return;
        }
        let slot = match normalize(key).as_str() {
            "email" => &mut self.email,
            "firstname" => &mut self.first_name,
            "lastname" => &mut self.last_name,
            "title" | "jobtitle" | "position" => &mut self.title,
            "company" | "companyname" => &mut self.company_name,
            "website" | "domain" | "companydomain" | "companyurl" | "companywebsite" => &mut self.company_domain,
            "phone" | "phonenumber" | "mobile" => &mut self.phone,
            "linkedin" | "linkedinurl" | "linkedinprofile" => &mut self.linkedin_url,
            "city" | "location" => &mut self.city,
            "icebreaker" | "personalization" => &mut self.hook,
            _ => {
                self.custom.push((key.trim().to_string(), text));
                return;
            }
        };
        if slot.is_empty() {
            *slot = text;
        }
    }
}

/// A field's name in lowercase letters and digits only, so the tools' spellings of it compare equal.
fn normalize(key: &str) -> String {
    key.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// The tool's variables in a subject or body as Splee's fields, like {{firstName}} and {{first_name}} as {first_name}.
/// One with no Splee field stays as written, so the user sees it on the campaign and can rewrite it.
pub fn splee_fields(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start + 2..].find("}}") else { break };
        let inner = &rest[start + 2..start + 2 + len];
        let field = match normalize(inner).as_str() {
            "firstname" => Some("first_name"),
            "lastname" => Some("last_name"),
            "name" | "fullname" => Some("full_name"),
            "company" | "companyname" => Some("company"),
            "title" | "jobtitle" => Some("title"),
            "city" | "location" => Some("city"),
            "sendername" | "sendingaccountfullname" | "sendingaccountfirstname" => Some("sender_name"),
            _ => None,
        };
        out.push_str(&rest[..start]);
        match field {
            Some(f) => {
                out.push('{');
                out.push_str(f);
                out.push('}');
            }
            None => out.push_str(&rest[start..start + 2 + len + 2]),
        }
        rest = &rest[start + 2 + len + 2..];
    }
    out.push_str(rest);
    out
}

/// An email body the tool keeps as HTML, as plain text with its line breaks. Text without tags comes back as it was.
pub fn plain_text(html: &str) -> String {
    const BREAKS: [&str; 6] = ["br", "p", "div", "li", "tr", "h1"];
    let mut text = String::with_capacity(html.len());
    let mut tag: Option<String> = None;
    for c in html.chars() {
        match (&mut tag, c) {
            (None, '<') => tag = Some(String::new()),
            (Some(name), '>') => {
                let closing = name.starts_with('/');
                let name = name.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or_default().to_ascii_lowercase();
                // A closing tag ends a line, so "<div>a</div><div>b</div>" is two lines, and a paragraph leaves a blank line.
                if name == "br" || (closing && BREAKS.contains(&name.as_str())) {
                    text.push_str(if closing && name == "p" { "\n\n" } else { "\n" });
                }
                tag = None;
            }
            (Some(name), c) => name.push(c),
            (None, c) => text.push(c),
        }
    }
    let text = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&rsquo;", "\u{2019}")
        .replace("&amp;", "&");
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let mut out = String::new();
    let mut blanks = 0;
    for line in lines {
        if line.trim().is_empty() {
            blanks += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blanks > 0 { "\n\n" } else { "\n" });
        }
        blanks = 0;
        out.push_str(line.trim_start());
    }
    out
}

/// A step's subject and body as Splee shows and writes them.
pub fn step(delay_days: i64, subject: &str, body: &str) -> Step {
    Step { delay_days: delay_days.clamp(0, 365) as i32, subject: splee_fields(plain_text(subject).trim()), body: splee_fields(&plain_text(body)) }
}

/// How many times a request is tried when the tool says to slow down, and the longest wait between tries.
const TRIES: u32 = 6;
const MAX_WAIT: Duration = Duration::from_secs(60);
/// Lead pages a campaign may take. Instantly and Smartlead send 100 a page, so this is 100,000 leads.
pub const MAX_PAGES: usize = 1000;

/// Talks to one tool with the user's key. The key is only ever held here and, encrypted, on a running import.
pub struct Client {
    pub tool: Tool,
    base: String,
    key: String,
    http: reqwest::Client,
}

impl Client {
    /// `fake` is Config::outreach_api_url: empty for the tools themselves. Tests and sandboxes set it to a fake that answers
    /// for each tool under /{tool}.
    pub fn new(tool: Tool, fake: &str, key: &str) -> AppResult<Client> {
        let base = if fake.is_empty() { tool.api().to_string() } else { format!("{}/{}", fake.trim_end_matches('/'), tool.name()) };
        let http = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().map_err(|e| AppError::Internal(e.into()))?;
        Ok(Client { tool, base, key: key.trim().to_string(), http })
    }

    pub async fn campaigns(&self) -> AppResult<Vec<Campaign>> {
        match self.tool {
            Tool::Instantly => instantly::campaigns(self).await,
            Tool::Smartlead => smartlead::campaigns(self).await,
            Tool::Lemlist => lemlist::campaigns(self).await,
        }
    }

    pub async fn steps(&self, campaign: &str) -> AppResult<Vec<Step>> {
        match self.tool {
            Tool::Instantly => instantly::steps(self, campaign).await,
            Tool::Smartlead => smartlead::steps(self, campaign).await,
            Tool::Lemlist => lemlist::steps(self, campaign).await,
        }
    }

    pub async fn leads(&self, campaign: &str) -> AppResult<Vec<Lead>> {
        match self.tool {
            Tool::Instantly => instantly::leads(self, campaign).await,
            Tool::Smartlead => smartlead::leads(self, campaign).await,
            Tool::Lemlist => lemlist::leads(self, campaign).await,
        }
    }

    /// The account's blocked emails and domains, as the tool writes them.
    pub async fn blocklist(&self) -> AppResult<Vec<String>> {
        match self.tool {
            Tool::Instantly => instantly::blocklist(self).await,
            Tool::Smartlead => smartlead::blocklist(self).await,
            Tool::Lemlist => lemlist::blocklist(self).await,
        }
    }

    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> AppResult<Value> {
        self.send(reqwest::Method::GET, path, query, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> AppResult<Value> {
        self.send(reqwest::Method::POST, path, &[], Some(body)).await
    }

    /// Sends a request with the key the way the tool takes it, and waits and tries again when the tool answers 429 or 503,
    /// for as long as its Retry-After says or else a little longer each time.
    async fn send(&self, method: reqwest::Method, path: &str, query: &[(&str, String)], body: Option<Value>) -> AppResult<Value> {
        let label = self.tool.label();
        for attempt in 0..TRIES {
            let mut url = reqwest::Url::parse(&format!("{}{path}", self.base)).map_err(|e| AppError::Internal(e.into()))?;
            if !query.is_empty() {
                url.query_pairs_mut().extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
            }
            if self.tool == Tool::Smartlead {
                url.query_pairs_mut().append_pair("api_key", &self.key);
            }
            let mut req = self.http.request(method.clone(), url);
            req = match self.tool {
                Tool::Instantly => req.bearer_auth(&self.key),
                Tool::Smartlead => req,
                Tool::Lemlist => req.basic_auth("", Some(&self.key)),
            };
            if let Some(body) = &body {
                req = req.json(body);
            }
            let res = req.send().await.map_err(|_| AppError::BadRequest(format!("Couldn't reach {label}. Try again in a moment.")))?;
            let status = res.status().as_u16();
            let retry_after = res.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
            let text = res.text().await.unwrap_or_default();
            let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            match status {
                200..=299 => return Ok(body),
                429 | 503 if attempt + 1 < TRIES => {
                    let wait = retry_after.or_else(|| body["retry_after"].as_u64()).map_or(Duration::from_secs(2 << attempt), Duration::from_secs);
                    tokio::time::sleep(wait.min(MAX_WAIT)).await;
                }
                401 | 403 => return Err(AppError::BadRequest(format!("{label} didn't accept that API key. Copy it again from {label}'s settings."))),
                404 => return Err(AppError::BadRequest(format!("{label} couldn't find that campaign. It may have been deleted."))),
                429 | 503 => return Err(AppError::BadRequest(format!("{label} is busy. Try again in a few minutes."))),
                _ => {
                    let message = ["message", "error"].iter().find_map(|k| body[*k].as_str()).map(str::to_string).unwrap_or(text);
                    let message = message.trim().chars().take(200).collect::<String>();
                    return Err(AppError::BadRequest(format!("{label}: {}", if message.is_empty() { status.to_string() } else { message })));
                }
            }
        }
        unreachable!("the last try returns")
    }
}

/// A count the tools send as a number or as a string of one.
pub fn count(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
}

/// A tool's status word, like "ACTIVE" or "running", as "Active" or "Running".
pub fn title_case(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut chars = lower.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

pub fn date(value: &Value) -> Option<DateTime<Utc>> {
    value.as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&Utc))
}

/// A campaign as the user picked it, with what the import did to it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Picked {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub leads: Option<i64>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub segment_id: Option<Uuid>,
    #[serde(default)]
    pub steps: i64,
    #[serde(default)]
    pub added: i64,
    #[serde(default)]
    pub duplicates: i64,
    #[serde(default)]
    pub suppressed: i64,
    /// Leads with no address Splee can send to.
    #[serde(default)]
    pub invalid: i64,
    #[serde(default)]
    pub error: String,
}

/// Picks up queued imports. One left running by a server that stopped is queued again: everything it did is kept, and the
/// next run adds only what's still missing.
pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        if let Err(e) = sqlx::query("update outreach_imports set status = 'queued' where status = 'running'").execute(&state.db).await {
            tracing::error!(error = ?e, "outreach imports: requeue failed");
        }
        loop {
            let claimed: Result<Option<Uuid>, sqlx::Error> = sqlx::query_scalar(
                "update outreach_imports set status = 'running'
                 where id = (select id from outreach_imports where status = 'queued' order by created_at limit 1 for update skip locked)
                 returning id",
            )
            .fetch_optional(&state.db)
            .await;
            match claimed {
                Ok(Some(id)) => {
                    if let Err(e) = run(&state, id).await {
                        tracing::error!(error = ?e, %id, "outreach import failed");
                    }
                }
                Ok(None) => tokio::time::sleep(Duration::from_secs(2)).await,
                Err(e) => {
                    tracing::error!(error = ?e, "outreach imports: claim failed");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    });
}

#[derive(sqlx::FromRow)]
struct Job {
    org_id: Uuid,
    project_id: Uuid,
    tool: String,
    api_key_enc: String,
    campaigns: sqlx::types::Json<Vec<Picked>>,
    include_blocklist: bool,
}

/// Runs an import to the end. A campaign that fails is marked so and the next one goes ahead. The key is blanked however it
/// ends, so it's never kept past the import.
pub async fn run(state: &AppState, id: Uuid) -> anyhow::Result<()> {
    let outcome = run_inner(state, id).await;
    let error = match &outcome {
        Ok(()) => String::new(),
        Err(e) => e.downcast_ref::<AppError>().map_or_else(|| "Something went wrong".to_string(), AppError::public_message),
    };
    // Done when any campaign came over. A campaign the run never got to is failed, so the page doesn't wait on it.
    sqlx::query(
        "update outreach_imports set api_key_enc = '', finished_at = now(), error = case when $2 = '' then error else $2 end,
           status = case when $2 = '' and exists (select 1 from jsonb_array_elements(campaigns) c where c ->> 'status' = 'done')
                         then 'done' else 'failed' end,
           campaigns = (select coalesce(jsonb_agg(case when c ->> 'status' = 'done' or c ->> 'status' = 'failed' then c
                                                       else c || '{\"status\": \"failed\"}' end order by i), '[]')
                        from jsonb_array_elements(campaigns) with ordinality as e(c, i))
         where id = $1",
    )
    .bind(id)
    .bind(&error)
    .execute(&state.db)
    .await?;
    outcome
}

async fn run_inner(state: &AppState, id: Uuid) -> anyhow::Result<()> {
    let job: Job = sqlx::query_as("select org_id, project_id, tool, api_key_enc, campaigns, include_blocklist from outreach_imports where id = $1")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    let tool = Tool::parse(&job.tool).ok_or_else(|| anyhow::anyhow!("unknown tool {}", job.tool))?;
    let key = crate::crypto::decrypt(&state.cfg.app_secret, &job.api_key_enc)?;
    let client = Client::new(tool, &state.cfg.outreach_api_url, &key)?;

    // The blocklist goes first, so a campaign's blocked leads are skipped like any other suppressed address. A key may be
    // allowed to read campaigns but not the blocklist, so the campaigns still come over without it.
    if job.include_blocklist {
        match client.blocklist().await {
            Ok(entries) => {
                let added = suppress(&state.db, job.org_id, entries.iter().map(|e| (e.as_str(), "manual"))).await?;
                sqlx::query("update outreach_imports set blocklist_added = $2 where id = $1").bind(id).bind(added as i32).execute(&state.db).await?;
            }
            Err(e) => {
                sqlx::query("update outreach_imports set error = $2 where id = $1")
                    .bind(id)
                    .bind(format!("Couldn't read the blocklist. {}", e.public_message()))
                    .execute(&state.db)
                    .await?;
            }
        }
    }

    for (index, picked) in job.campaigns.0.iter().enumerate() {
        let update = |patch: Value| {
            sqlx::query("update outreach_imports set campaigns = jsonb_set(campaigns, array[$2::text], (campaigns -> $3) || $4) where id = $1")
                .bind(id)
                .bind(index.to_string())
                .bind(index as i32)
                .bind(patch)
                .execute(&state.db)
        };
        update(json!({ "status": "running" })).await?;
        match campaign(state, &client, &job, picked).await {
            Ok(done) => update(serde_json::to_value(Picked { status: "done".into(), ..done })?).await?,
            Err(e) => {
                let message = e.downcast_ref::<AppError>().map_or_else(|| "Something went wrong".to_string(), AppError::public_message);
                tracing::warn!(error = ?e, campaign = picked.id, "outreach import: campaign failed");
                update(json!({ "status": "failed", "error": message })).await?
            }
        };
    }
    Ok(())
}

/// Copies one campaign: its draft, its steps and its leads.
async fn campaign(state: &AppState, client: &Client, job: &Job, picked: &Picked) -> anyhow::Result<Picked> {
    let steps = client.steps(&picked.id).await?;
    let leads = client.leads(&picked.id).await?;
    let name: String = picked.name.trim().chars().take(200).collect();
    let mut tx = state.db.begin().await?;
    let segment_id: Uuid = sqlx::query_scalar(
        "insert into segments (project_id, name, description, status, imported_from, imported_id) values ($1, $2, $3, 'draft', $4, $5)
         on conflict (project_id, imported_from, imported_id) where imported_id is not null do update set imported_id = excluded.imported_id
         returning id",
    )
    .bind(job.project_id)
    .bind(if name.is_empty() { format!("{} campaign", client.tool.label()) } else { name })
    .bind(format!("Brought over from {}.", client.tool.label()))
    .bind(client.tool.name())
    .bind(&picked.id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("delete from segment_steps where segment_id = $1").bind(segment_id).execute(&mut *tx).await?;
    for (position, s) in steps.iter().enumerate() {
        sqlx::query("insert into segment_steps (segment_id, position, delay_days, subject, body) values ($1, $2, $3, $4, $5)")
            .bind(segment_id)
            .bind(position as i32)
            .bind(if position == 0 { 0 } else { s.delay_days })
            .bind(&s.subject)
            .bind(&s.body)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    let counts = insert_leads(&state.db, job.org_id, job.project_id, segment_id, client.tool, leads).await?;
    Ok(Picked { segment_id: Some(segment_id), steps: steps.len() as i64, ..counts.into_picked(picked) })
}

#[derive(Debug, Default, PartialEq)]
pub struct Counts {
    pub added: i64,
    pub duplicates: i64,
    pub suppressed: i64,
    pub invalid: i64,
}

impl Counts {
    fn into_picked(self, picked: &Picked) -> Picked {
        Picked { added: self.added, duplicates: self.duplicates, suppressed: self.suppressed, invalid: self.invalid, ..picked.clone() }
    }
}

/// Adds a campaign's leads to the project. A lead on the suppression list is skipped. A lead the tool emailed gets its status
/// and last-contacted date, so the writer never sends it a first email. A lead already in the project counts as a duplicate
/// and keeps what it has, unless Splee hasn't touched it yet and the tool emailed it, which it then says. Bounced and
/// unsubscribed addresses go on the suppression list, even for a duplicate.
pub async fn insert_leads(db: &PgPool, org_id: Uuid, project_id: Uuid, segment_id: Uuid, tool: Tool, leads: Vec<Lead>) -> anyhow::Result<Counts> {
    let mut tx = db.begin().await?;
    let suppressed: HashSet<String> =
        sqlx::query_scalar("select value::text from suppressions where org_id = $1").bind(org_id).fetch_all(&mut *tx).await?.into_iter().collect();
    let mut seen: HashSet<String> = sqlx::query_scalar("select email::text from leads where project_id = $1 and email is not null")
        .bind(project_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
    let project_country: Option<String> =
        sqlx::query_scalar("select company ->> 'country_code' from projects where id = $1").bind(project_id).fetch_one(&mut *tx).await?;
    let now = Utc::now();

    let mut counts = Counts::default();
    let mut to_suppress: Vec<(String, &'static str)> = Vec::new();
    let mut rows = Rows::default();
    let mut emailed = Rows::default();
    for lead in leads {
        let Some(email) = util_input::email(&lead.email) else {
            counts.invalid += 1;
            continue;
        };
        if let Some(reason) = lead.state.suppression() {
            to_suppress.push((email.clone(), reason));
        }
        let domain = email.rsplit('@').next().unwrap_or_default();
        // lemlist's export has no dates, so an emailed lead counts from the import. Close enough to keep it out of a first email.
        let contacted = (lead.state != State::New).then(|| lead.last_contacted_at.unwrap_or(now));
        if seen.contains(&email) {
            counts.duplicates += 1;
            if contacted.is_some() {
                emailed.email.push(email);
                emailed.status.push(lead.state.status().to_string());
                emailed.contacted.push(contacted);
            }
            continue;
        }
        if suppressed.contains(&email) || suppressed.contains(domain) {
            counts.suppressed += 1;
            continue;
        }
        seen.insert(email.clone());
        let full_name = format!("{} {}", lead.first_name, lead.last_name).trim().to_string();
        let notes = if lead.custom.is_empty() {
            String::new()
        } else {
            let fields = lead.custom.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n");
            format!("From {}:\n{fields}", tool.label())
        };
        rows.email.push(email);
        rows.full_name.push(full_name);
        rows.first_name.push(lead.first_name);
        rows.last_name.push(lead.last_name);
        rows.title.push(lead.title);
        rows.company_name.push(lead.company_name);
        rows.company_domain.push(util_input::domain(&lead.company_domain));
        rows.linkedin_url.push(lead.linkedin_url);
        rows.city.push(lead.city);
        rows.phone.push(crate::phone::normalize(&lead.phone, project_country.as_deref().unwrap_or_default()).unwrap_or_default());
        rows.status.push(lead.state.status().to_string());
        rows.contacted.push(contacted);
        rows.notes.push(notes);
        rows.personalization.push(if lead.hook.is_empty() { json!({}) } else { json!({ "hook": lead.hook }) });
    }

    let added = sqlx::query(
        "insert into leads (project_id, segment_id, source, email, full_name, first_name, last_name, title, company_name, company_domain,
                            linkedin_url, city, phone, status, last_contacted_at, status_changed_at, notes, personalization)
         select $1, $2, 'import', e, f, fi, la, t, c, d, li, ci, ph, s, lc, lc, n, p
         from unnest($3::text[], $4::text[], $5::text[], $6::text[], $7::text[], $8::text[], $9::text[], $10::text[], $11::text[],
                     $12::text[], $13::text[], $14::timestamptz[], $15::text[], $16::jsonb[])
              as u(e, f, fi, la, t, c, d, li, ci, ph, s, lc, n, p)
         on conflict (project_id, email) do nothing",
    )
    .bind(project_id)
    .bind(segment_id)
    .bind(&rows.email)
    .bind(&rows.full_name)
    .bind(&rows.first_name)
    .bind(&rows.last_name)
    .bind(&rows.title)
    .bind(&rows.company_name)
    .bind(&rows.company_domain)
    .bind(&rows.linkedin_url)
    .bind(&rows.city)
    .bind(&rows.phone)
    .bind(&rows.status)
    .bind(&rows.contacted)
    .bind(&rows.notes)
    .bind(&rows.personalization)
    .execute(&mut *tx)
    .await?
    .rows_affected() as i64;
    sqlx::query(
        "update leads l set status = u.s, last_contacted_at = u.lc, status_changed_at = u.lc
         from unnest($2::text[], $3::text[], $4::timestamptz[]) as u(e, s, lc)
         where l.project_id = $1 and l.email = u.e and l.status = 'new' and l.last_contacted_at is null
           and not exists (select 1 from email_messages m where m.lead_id = l.id and m.direction = 'outbound' and m.status <> 'cancelled')",
    )
    .bind(project_id)
    .bind(&emailed.email)
    .bind(&emailed.status)
    .bind(&emailed.contacted)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    counts.duplicates += rows.email.len() as i64 - added;
    counts.added = added;
    counts.suppressed += suppress(db, org_id, to_suppress.iter().map(|(v, r)| (v.as_str(), *r))).await? as i64;
    Ok(counts)
}

#[derive(Default)]
struct Rows {
    email: Vec<String>,
    full_name: Vec<String>,
    first_name: Vec<String>,
    last_name: Vec<String>,
    title: Vec<String>,
    company_name: Vec<String>,
    company_domain: Vec<Option<String>>,
    linkedin_url: Vec<String>,
    city: Vec<String>,
    phone: Vec<String>,
    status: Vec<String>,
    contacted: Vec<Option<DateTime<Utc>>>,
    notes: Vec<String>,
    personalization: Vec<Value>,
}

/// Adds emails, domains and phone numbers with their country code to the org's suppression list, and returns how many were
/// new. Anything else a tool can block, like a LinkedIn profile, is left out: read as a domain, a profile link would block
/// all of linkedin.com.
pub async fn suppress<'a>(db: &PgPool, org_id: Uuid, entries: impl IntoIterator<Item = (&'a str, &'static str)>) -> anyhow::Result<u64> {
    let mut values = Vec::new();
    let mut kinds = Vec::new();
    let mut reasons = Vec::new();
    for (raw, reason) in entries {
        let raw = raw.trim();
        let parsed = if raw.contains('@') {
            util_input::email(raw).map(|v| (v, "email"))
        } else if raw.starts_with('+') {
            crate::phone::normalize(raw, "").map(|v| (v, "phone"))
        } else if !raw.contains(['/', ':', ' ']) {
            util_input::domain(raw).map(|v| (v, "domain"))
        } else {
            None
        };
        if let Some((value, kind)) = parsed {
            values.push(value);
            kinds.push(kind);
            reasons.push(reason);
        }
    }
    let added = sqlx::query(
        "insert into suppressions (org_id, value, kind, reason) select $1, v, k, r from unnest($2::text[], $3::text[], $4::text[]) as u(v, k, r)
         on conflict (org_id, value) do nothing",
    )
    .bind(org_id)
    .bind(&values)
    .bind(&kinds)
    .bind(&reasons)
    .execute(db)
    .await?
    .rows_affected();
    Ok(added)
}

#[cfg(test)]
pub mod fake;
#[cfg(test)]
mod tests;
