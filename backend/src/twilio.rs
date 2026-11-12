//! Texts through the organization's own Twilio account: the project's text steps after the first email, replies typed in
//! the inbox, and the webhooks Twilio posts for answers and delivery. Twilio is called with an API key, and its webhooks are
//! checked against the auth token, which is what Twilio signs them with.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::Utc;
use chrono_tz::Tz;
use hmac::{Hmac, KeyInit, Mac};
use reqwest::StatusCode;
use serde_json::{Map, Value, json};
use sha1::Sha1;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::agent::{self, kind};
use crate::models::{Lead, TextMessage, Ts};
use crate::phone;
use crate::state::AppState;

pub const API_URL: &str = "https://api.twilio.com";
pub const CONTENT_URL: &str = "https://content.twilio.com";
/// Twilio's opt-out words. Any of them alone in a text puts the number on the do-not-contact list.
const STOP_WORDS: [&str; 6] = ["STOP", "STOPALL", "UNSUBSCRIBE", "CANCEL", "END", "QUIT"];
/// Two SMS segments. Past that a text costs more and reads like a pitch.
pub const SMS_MAX_CHARS: usize = 320;
/// After the lead last wrote on WhatsApp, how long free text can go to them. Outside it only an approved template can.
const WHATSAPP_WINDOW_HOURS: i64 = 24;
/// Lead fields a WhatsApp template's variables can take, and sender_name, the project's.
pub const TEMPLATE_FIELDS: [&str; 7] = ["first_name", "last_name", "full_name", "company_name", "title", "city", "sender_name"];

#[derive(Debug, Clone, FromRow)]
pub struct Account {
    pub account_sid: String,
    pub api_key_sid: String,
    pub api_secret_enc: String,
    pub sms_from: String,
    pub whatsapp_from: String,
    pub whatsapp_template_sid: String,
    pub whatsapp_template_body: String,
    pub whatsapp_template_vars: Value,
}

impl Account {
    pub async fn of_org(db: &PgPool, org_id: Uuid) -> sqlx::Result<Option<Self>> {
        sqlx::query_as("select * from twilio_accounts where org_id = $1").bind(org_id).fetch_optional(db).await
    }
}

#[derive(Debug, PartialEq)]
pub enum Failure {
    /// Something about this text or number, like a number that can't get texts. The text fails.
    Message(String),
    /// The account, like a revoked API key or a rate limit. The org's texts wait an hour.
    Account(String),
    /// No answer, so Twilio may have sent it. The text fails rather than going out twice.
    Unanswered(String),
}

impl Failure {
    pub fn reason(&self) -> &str {
        match self {
            Failure::Message(r) | Failure::Account(r) | Failure::Unanswered(r) => r,
        }
    }
}

pub enum Content<'a> {
    Text(&'a str),
    Template { sid: &'a str, vars: &'a Value },
}

pub struct Client {
    api: String,
    content: String,
    account_sid: String,
    user: String,
    password: String,
    http: reqwest::Client,
}

impl Client {
    /// `base` is Config::twilio_api_url: empty for Twilio itself. `user` and `password` are the API key's SID and secret, or the
    /// account SID and auth token.
    pub fn new(base: &str, account_sid: &str, user: &str, password: &str) -> Self {
        let (api, content) = if base.is_empty() { (API_URL.into(), CONTENT_URL.into()) } else { (base.to_string(), base.to_string()) };
        let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().expect("a plain client builds");
        Self { api, content, account_sid: account_sid.into(), user: user.into(), password: password.into(), http }
    }

    pub fn for_account(state: &AppState, a: &Account) -> anyhow::Result<Self> {
        let secret = crate::crypto::decrypt(&state.cfg.app_secret, &a.api_secret_enc)?;
        Ok(Self::new(&state.cfg.twilio_api_url, &a.account_sid, &a.api_key_sid, &secret))
    }

    async fn call(&self, request: reqwest::RequestBuilder) -> Result<Value, Failure> {
        let res = request
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .map_err(|e| Failure::Unanswered(format!("Twilio didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        if status.is_success() {
            return Ok(body);
        }
        // Twilio explains a failure as {"code": 21211, "message": "...", "status": 400}.
        let said = body["message"].as_str().filter(|m| !m.is_empty()).unwrap_or(status.as_str()).trim_end_matches('.');
        let reason = match body["code"].as_i64() {
            Some(code) => format!("{said} (Twilio error {code})"),
            None => said.to_string(),
        };
        Err(match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Failure::Account(format!("Twilio turned the credentials down: {reason}")),
            StatusCode::TOO_MANY_REQUESTS => Failure::Account(format!("Twilio is rate limiting the account: {reason}")),
            s if s.is_server_error() => Failure::Account(format!("Twilio had a problem: {reason}")),
            _ => Failure::Message(reason),
        })
    }

    /// The account's name. Fetching the account is the cheapest call that proves the credentials work.
    pub async fn check(&self) -> Result<String, Failure> {
        let account = self.call(self.http.get(format!("{}/2010-04-01/Accounts/{}.json", self.api, self.account_sid))).await?;
        Ok(account["friendly_name"].as_str().unwrap_or_default().to_string())
    }

    /// A Content template's text, and the names of its variables, like ["1", "2"].
    pub async fn template(&self, sid: &str) -> Result<(String, Vec<String>), Failure> {
        let found = self.call(self.http.get(format!("{}/v1/Content/{sid}", self.content))).await?;
        let types = found["types"].as_object().into_iter().flat_map(|t| t.values());
        let body = types.into_iter().find_map(|t| t["body"].as_str().or(t["title"].as_str())).unwrap_or_default().to_string();
        let mut vars: Vec<String> = found["variables"].as_object().map(|v| v.keys().cloned().collect()).unwrap_or_default();
        vars.sort_by_key(|v| (v.parse::<u32>().unwrap_or(u32::MAX), v.clone()));
        Ok((body, vars))
    }

    /// Points the TwiML App's Voice URL at Splee when it doesn't already, so calls from the browser ask /api/twilio/voice what
    /// to do. Returns the app's name.
    pub async fn point_app(&self, sid: &str, voice_url: &str) -> Result<String, Failure> {
        let url = format!("{}/2010-04-01/Accounts/{}/Applications/{sid}.json", self.api, self.account_sid);
        let app = self.call(self.http.get(&url)).await?;
        if app["voice_url"].as_str() != Some(voice_url) || !app["voice_method"].as_str().is_some_and(|m| m.eq_ignore_ascii_case("POST")) {
            self.call(self.http.post(&url).form(&[("VoiceUrl", voice_url), ("VoiceMethod", "POST")])).await?;
        }
        Ok(app["friendly_name"].as_str().unwrap_or_default().to_string())
    }

    /// A call recording's audio as MP3. Twilio keeps recordings behind the account's credentials, so the app plays them
    /// through Splee.
    pub async fn recording(&self, sid: &str) -> Result<Vec<u8>, Failure> {
        let url = format!("{}/2010-04-01/Accounts/{}/Recordings/{sid}.mp3", self.api, self.account_sid);
        let res = self
            .http
            .get(url)
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .map_err(|e| Failure::Unanswered(format!("Twilio didn't answer: {}", e.without_url())))?;
        if !res.status().is_success() {
            return Err(Failure::Message(format!("Twilio couldn't find the recording ({})", res.status().as_u16())));
        }
        res.bytes().await.map(|b| b.to_vec()).map_err(|e| Failure::Unanswered(format!("Twilio stopped sending the recording: {}", e.without_url())))
    }

    /// Sends from the account's SMS sender or WhatsApp number and returns the MessageSid.
    pub async fn send(&self, account: &Account, channel: &str, to: &str, content: &Content<'_>, status_url: &str) -> Result<String, Failure> {
        let mut form = vec![("StatusCallback", status_url.to_string())];
        if channel == "whatsapp" {
            if account.whatsapp_from.is_empty() {
                return Err(Failure::Message("There's no WhatsApp sender on the Twilio connection".into()));
            }
            form.push(("To", format!("whatsapp:{to}")));
            form.push(("From", format!("whatsapp:{}", account.whatsapp_from)));
        } else {
            if account.sms_from.is_empty() {
                return Err(Failure::Message("There's no SMS sender on the Twilio connection".into()));
            }
            form.push(("To", to.to_string()));
            form.push((if account.sms_from.starts_with("MG") { "MessagingServiceSid" } else { "From" }, account.sms_from.clone()));
        }
        match content {
            Content::Text(body) => form.push(("Body", body.to_string())),
            Content::Template { sid, vars } => {
                form.push(("ContentSid", sid.to_string()));
                form.push(("ContentVariables", vars.to_string()));
            }
        }
        let url = format!("{}/2010-04-01/Accounts/{}/Messages.json", self.api, self.account_sid);
        let sent = self.call(self.http.post(url).form(&form)).await?;
        sent["sid"].as_str().filter(|s| !s.is_empty()).map(String::from).ok_or_else(|| Failure::Unanswered("Twilio didn't return the message's id".into()))
    }
}

/// X-Twilio-Signature: the base64 HMAC-SHA1, keyed with the auth token, of the URL Twilio posted to with each form parameter's
/// name and value after it, sorted by name.
fn signer(auth_token: &str, url: &str, params: &[(String, String)]) -> Hmac<Sha1> {
    let mut sorted: Vec<&(String, String)> = params.iter().collect();
    sorted.sort();
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(auth_token.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(url.as_bytes());
    for (k, v) in sorted {
        mac.update(k.as_bytes());
        mac.update(v.as_bytes());
    }
    mac
}

/// How Twilio signs, so tests can post like it.
#[cfg(test)]
pub fn signature(auth_token: &str, url: &str, params: &[(String, String)]) -> String {
    STANDARD.encode(signer(auth_token, url, params).finalize().into_bytes())
}

/// Checks the header in constant time.
pub fn verify(auth_token: &str, url: &str, params: &[(String, String)], header: &str) -> bool {
    let Ok(given) = STANDARD.decode(header.trim()) else { return false };
    signer(auth_token, url, params).verify_slice(&given).is_ok()
}

pub fn inbound_url(public_url: &str) -> String {
    format!("{}/api/twilio/inbound", public_url.trim_end_matches('/'))
}

pub fn status_url(public_url: &str) -> String {
    format!("{}/api/twilio/status", public_url.trim_end_matches('/'))
}

/// The template's variables filled with the lead's details, by the org's mapping. Twilio refuses an empty variable, so a
/// missing name reads "there" and anything else missing a neutral word.
pub fn template_values(map: &Value, lead: &Lead, sender_name: &str) -> Value {
    let mut out = Map::new();
    for (var, field) in map.as_object().into_iter().flatten() {
        let value = match field.as_str().unwrap_or_default() {
            "first_name" => lead.first_name.as_str(),
            "last_name" => lead.last_name.as_str(),
            "full_name" => lead.full_name.as_str(),
            "company_name" => lead.company_name.as_str(),
            "title" => lead.title.as_str(),
            "city" => lead.city.as_str(),
            "sender_name" => sender_name,
            _ => "",
        };
        let fallback = if field.as_str().is_some_and(|f| f.ends_with("name") && f != "company_name") { "there" } else { "your team" };
        out.insert(var.clone(), json!(if value.trim().is_empty() { fallback } else { value.trim() }));
    }
    Value::Object(out)
}

/// A template's text with {{1}} and the rest put in, as the lead will read it.
pub fn fill_template(body: &str, values: &Value) -> String {
    let mut text = body.to_string();
    for (var, value) in values.as_object().into_iter().flatten() {
        text = text.replace(&format!("{{{{{var}}}}}", var = var), value.as_str().unwrap_or_default());
    }
    text
}

/// A written SMS made ready to send: it names who it's from, the first text to a number where the law or carriers expect it
/// ends with the STOP line, and it fits SMS_MAX_CHARS, cut at a sentence when the writer ran long.
pub fn finish_sms(text: &str, sender: &str, company: &str, to: &str, first: bool) -> String {
    let text = text.trim();
    let who = [sender.trim(), company.trim()].into_iter().find(|w| !w.is_empty()).unwrap_or_default();
    let mut tail = String::new();
    if !who.is_empty() && !text.to_lowercase().contains(&who.to_lowercase()) {
        tail.push_str(&format!("\n{who}"));
    }
    if first && phone::needs_stop_line(to) && !text.to_uppercase().contains("STOP") {
        tail.push_str(&format!("\n{}", phone::STOP_LINE));
    }
    let room = SMS_MAX_CHARS.saturating_sub(tail.chars().count());
    let mut body: String = text.chars().take(room).collect();
    if body.chars().count() < text.chars().count() {
        let cut = body.rfind(['.', '!', '?']).map(|i| i + 1).or_else(|| body.rfind(char::is_whitespace)).unwrap_or(body.len());
        body.truncate(cut);
    }
    format!("{}{tail}", body.trim_end())
}

pub fn is_stop(body: &str) -> bool {
    let word = body.trim().trim_end_matches(['.', '!']).to_uppercase();
    STOP_WORDS.contains(&word.as_str())
}

/// Takes a lead's unsent sequence texts off, for a reply on any channel. $1 is the lead, $2 the reason shown on them.
pub const STOP_TEXTS: &str =
    "update text_messages set status = 'cancelled', error = $2 where lead_id = $1 and kind = 'sequence' and status in ('draft', 'approved')";

/// A text step that's due within 12 hours and not written yet.
#[derive(Debug, FromRow)]
pub struct DueStep {
    pub project_id: Uuid,
    pub org_id: Uuid,
    pub lead_id: Uuid,
    pub step: i32,
    pub channel: String,
    pub instructions: String,
    pub due_at: Ts,
    /// The lead's campaign is approved, so the text goes out without review.
    pub sending: bool,
    /// No text has gone to this number from the org before.
    pub first: bool,
}

/// Each lead's next text step that comes due within 12 hours: the first its delay after their first email went out, each
/// later one its delay after the text before it was sent. A lead who wrote back, isn't being contacted, or whose number is
/// on the do-not-contact list gets none, and a text that failed or was cancelled ends their sequence. `leads` narrows it.
pub async fn due_steps(db: &PgPool, leads: Option<&[Uuid]>) -> sqlx::Result<Vec<DueStep>> {
    sqlx::query_as(
        "with next as (
           select l.id as lead_id, l.project_id, p.org_id, l.phone, ps.text_steps, coalesce(s.status, 'working') in ('working', 'scaling') as sending,
                  coalesce(last.step, 0) + 1 as step, last.status as last_status,
                  case when last.step is null then first.sent_at else last.sent_at end as anchor
           from leads l
           join projects p on p.id = l.project_id
           join project_settings ps on ps.project_id = l.project_id
           join twilio_accounts a on a.org_id = p.org_id
           left join segments s on s.id = l.segment_id
           cross join lateral (select min(e.sent_at) as sent_at from email_messages e
                               where e.lead_id = l.id and e.direction = 'outbound' and e.kind = 'initial' and e.status = 'sent') first
           left join lateral (select t.step, t.status, t.sent_at from text_messages t where t.lead_id = l.id and t.kind = 'sequence'
                              order by t.step desc limit 1) last on true
           where l.phone <> '' and l.status = 'contacted' and not ps.paused and first.sent_at is not null
             and ($1::uuid[] is null or l.id = any($1))
             and jsonb_typeof(ps.text_steps) = 'array' and jsonb_array_length(ps.text_steps) > coalesce(last.step, 0)
             and coalesce(s.status, 'working') <> 'paused'
             and (last.status is null or last.status in ('sent', 'delivered'))
             and not exists (select 1 from email_messages i where i.lead_id = l.id and i.direction = 'inbound')
             and not exists (select 1 from suppressions x where x.org_id = p.org_id and x.kind = 'phone' and x.value = l.phone)
         ), timed as (
           select n.*, n.text_steps -> (n.step - 1) as rule,
                  n.anchor + make_interval(days => greatest(1, coalesce((n.text_steps -> (n.step - 1) ->> 'delay_days')::int, 2))) as due_at
           from next n
         )
         select t.project_id, t.org_id, t.lead_id, t.step, coalesce(t.rule ->> 'channel', 'sms') as channel,
                coalesce(t.rule ->> 'instructions', '') as instructions, t.due_at, t.sending,
                not exists (select 1 from text_messages o join projects op on op.id = o.project_id
                            where op.org_id = t.org_id and o.to_number = t.phone and o.status in ('sent', 'delivered')) as first
         from timed t where t.due_at <= now() + interval '12 hours'
         order by t.due_at limit 500",
    )
    .bind(leads)
    .fetch_all(db)
    .await
}

/// Saves a written step, waiting for review unless its campaign is approved. False when the step was already there.
pub async fn insert_step(db: &PgPool, step: &DueStep, lead: &Lead, body: &str, template: Option<(&str, &Value)>) -> sqlx::Result<bool> {
    let (sid, vars) = template.map(|(s, v)| (s, v.clone())).unwrap_or(("", json!({})));
    let done = sqlx::query(
        "insert into text_messages (project_id, lead_id, segment_id, channel, kind, step, to_number, body, template_sid, template_vars, status, due_at)
         values ($1, $2, $3, $4, 'sequence', $5, $6, $7, $8, $9, $10, $11)
         on conflict (lead_id, step) where kind = 'sequence' do nothing",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(lead.segment_id)
    .bind(&step.channel)
    .bind(step.step)
    .bind(&lead.phone)
    .bind(body)
    .bind(sid)
    .bind(vars)
    .bind(if step.sending { "approved" } else { "draft" })
    .bind(step.due_at)
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// WhatsApp steps are the org's approved template, filled in here. SMS steps are written by the agent, 20 leads a run.
pub async fn schedule(state: &AppState) -> anyhow::Result<()> {
    let db = &state.db;
    let mut accounts: HashMap<Uuid, Option<Account>> = HashMap::new();
    let mut to_write: HashMap<(Uuid, Uuid), Vec<Uuid>> = HashMap::new();
    for step in due_steps(db, None).await? {
        if step.channel != "whatsapp" {
            to_write.entry((step.project_id, step.org_id)).or_default().push(step.lead_id);
            continue;
        }
        if let Entry::Vacant(e) = accounts.entry(step.org_id) {
            e.insert(Account::of_org(db, step.org_id).await?);
        }
        // Without a template it waits, and goes out once one is picked.
        let Some(account) = accounts[&step.org_id].as_ref().filter(|a| !a.whatsapp_template_sid.is_empty() && !a.whatsapp_from.is_empty()) else {
            continue;
        };
        let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(step.lead_id).fetch_one(db).await?;
        let sender: String = sqlx::query_scalar("select sender_name from project_settings where project_id = $1").bind(lead.project_id).fetch_one(db).await?;
        let values = template_values(&account.whatsapp_template_vars, &lead, &sender);
        let preview = fill_template(&account.whatsapp_template_body, &values);
        insert_step(db, &step, &lead, &preview, Some((&account.whatsapp_template_sid, &values))).await?;
    }
    for ((project_id, org_id), leads) in to_write {
        // A run in the last 6 hours already wrote these, or couldn't, so they aren't paid for again so soon.
        let fresh: Vec<Uuid> = sqlx::query_scalar(
            "select u.lead_id from unnest($1::uuid[]) as u(lead_id) where not exists (
               select 1 from agent_runs r where r.kind = $2 and r.project_id = $3
                 and (r.status in ('queued', 'running') or r.created_at > now() - interval '6 hours') and r.input -> 'lead_ids' ? u.lead_id::text)",
        )
        .bind(&leads)
        .bind(kind::WRITE_TEXTS)
        .bind(project_id)
        .fetch_all(db)
        .await?;
        for batch in fresh.chunks(20) {
            agent::enqueue(db, kind::WRITE_TEXTS, Some(project_id), Some(org_id), json!({ "lead_ids": batch })).await?;
        }
    }
    Ok(())
}

#[derive(FromRow)]
struct Outgoing {
    #[sqlx(flatten)]
    text: TextMessage,
    org_id: Uuid,
    paused: bool,
    timezone: String,
    send_days: Vec<i32>,
    send_start_hour: i32,
    send_end_hour: i32,
    daily_text_limit: i32,
    lead_country: String,
    /// Why it can't go out at all, like a number on the do-not-contact list, which cancels it.
    blocked: Option<String>,
    /// When the lead last wrote on WhatsApp.
    whatsapp_in: Option<Ts>,
}

/// The sender's every-minute pass: due steps get written, then approved texts go out. Replies go first and skip the send
/// window, the pause switch and the daily cap. Sequence texts keep to the project's send days and hours in the lead's time
/// zone when their number or country says it, else the project's.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    schedule(state).await?;
    send_due(state).await
}

async fn send_due(state: &AppState) -> anyhow::Result<()> {
    let db = &state.db;
    // One pass at a time across backends, so the daily cap counted below can't be spent twice. The lock goes with the
    // transaction, which nothing else uses, so an error anywhere below still releases it.
    let mut lock = db.begin().await?;
    if !sqlx::query_scalar::<_, bool>("select pg_try_advisory_xact_lock(hashtextextended('twilio-send', 0))").fetch_one(&mut *lock).await? {
        return Ok(());
    }
    let due = sqlx::query_as::<_, Outgoing>(
        "select t.*, p.org_id, s.paused, s.timezone, s.send_days, s.send_start_hour, s.send_end_hour, s.daily_text_limit,
                l.country_code as lead_country,
                case when exists (select 1 from suppressions x where x.org_id = p.org_id
                                  and ((x.kind = 'phone' and x.value = t.to_number) or x.value = l.email or x.value = split_part(l.email::text, '@', 2)))
                       then 'On the do-not-contact list'
                     when l.status in ('unsubscribed', 'do_not_contact') then 'Lead ' || replace(l.status, '_', ' ')
                     when t.kind = 'sequence' and (l.status not in ('new', 'queued', 'contacted') or exists (
                       select 1 from email_messages i where i.lead_id = l.id and i.direction = 'inbound')) then 'They replied'
                end as blocked,
                (select max(i.created_at) from email_messages i where i.lead_id = l.id and i.direction = 'inbound' and i.channel = 'whatsapp') as whatsapp_in
         from text_messages t
         join projects p on p.id = t.project_id
         join project_settings s on s.project_id = t.project_id
         join leads l on l.id = t.lead_id
         join twilio_accounts a on a.org_id = p.org_id
         where t.status = 'approved' and coalesce(t.due_at, now()) <= now() and (a.failed_at is null or a.failed_at < now() - interval '1 hour')
         order by t.kind = 'reply' desc, t.due_at nulls first, t.created_at limit 500",
    )
    .fetch_all(db)
    .await?;

    let mut accounts: HashMap<Uuid, Option<(Account, Client)>> = HashMap::new();
    let mut room: HashMap<Uuid, i64> = HashMap::new();
    let now = Utc::now();
    for row in due {
        let t = &row.text;
        if let Some(why) = &row.blocked {
            sqlx::query("update text_messages set status = 'cancelled', error = $2 where id = $1 and status = 'approved'").bind(t.id).bind(why).execute(db).await?;
            continue;
        }
        if t.kind == "sequence" {
            let tz = phone::time_zone(&t.to_number, &row.lead_country).unwrap_or(row.timezone.parse().unwrap_or(Tz::UTC));
            if row.paused || !crate::mail::in_window(tz, &row.send_days, row.send_start_hour, row.send_end_hour, now) {
                continue;
            }
        }
        if let Entry::Vacant(e) = room.entry(t.project_id) {
            let sent: i64 = sqlx::query_scalar("select count(*) from text_messages where project_id = $1 and sent_at > now() - interval '1 day'")
                .bind(t.project_id)
                .fetch_one(db)
                .await?;
            e.insert(i64::from(row.daily_text_limit) - sent);
        }
        if t.kind == "sequence" && room[&t.project_id] <= 0 {
            continue;
        }
        if let Entry::Vacant(e) = accounts.entry(row.org_id) {
            let account = Account::of_org(db, row.org_id).await?;
            let client = match account {
                Some(a) => match Client::for_account(state, &a) {
                    Ok(c) => Some((a, c)),
                    Err(e) => {
                        tracing::warn!(org_id = %row.org_id, error = %e, "couldn't decrypt the Twilio API key");
                        None
                    }
                },
                None => None,
            };
            e.insert(client);
        }
        // An account that failed this tick holds the rest of its texts too.
        let Some((account, client)) = accounts[&row.org_id].as_ref() else { continue };

        let window_open = row.whatsapp_in.is_some_and(|at| at > now - chrono::Duration::hours(WHATSAPP_WINDOW_HOURS));
        if t.channel == "whatsapp" && t.template_sid.is_empty() && !window_open {
            let why = "More than 24 hours passed since they last wrote on WhatsApp, so only an approved template can go to them";
            sqlx::query("update text_messages set status = 'failed', error = $2 where id = $1").bind(t.id).bind(why).execute(db).await?;
            continue;
        }
        // The checks in the select above again, now, since a STOP or a reply may have come in while earlier texts went out.
        // A text that fails them stays approved, and the next pass cancels it with the reason.
        let claimed = sqlx::query(
            "update text_messages t set status = 'sending' from leads l, projects p
             where t.id = $1 and t.status = 'approved' and l.id = t.lead_id and p.id = t.project_id
               and l.status not in ('unsubscribed', 'do_not_contact')
               and not exists (select 1 from suppressions x where x.org_id = p.org_id
                               and ((x.kind = 'phone' and x.value = t.to_number) or x.value = l.email or x.value = split_part(l.email::text, '@', 2)))
               and (t.kind <> 'sequence' or (l.status in ('new', 'queued', 'contacted')
                    and not exists (select 1 from email_messages i where i.lead_id = l.id and i.direction = 'inbound')))",
        )
        .bind(t.id)
        .execute(db)
        .await?;
        if claimed.rows_affected() == 0 {
            continue;
        }
        let content = if t.template_sid.is_empty() { Content::Text(&t.body) } else { Content::Template { sid: &t.template_sid, vars: &t.template_vars } };
        match client.send(account, &t.channel, &t.to_number, &content, &status_url(&state.cfg.public_url)).await {
            Ok(sid) => {
                sqlx::query("update text_messages set status = 'sent', sent_at = now(), twilio_sid = $2, error = '' where id = $1")
                    .bind(t.id)
                    .bind(sid)
                    .execute(db)
                    .await?;
                sqlx::query("update leads set last_contacted_at = now() where id = $1").bind(t.lead_id).execute(db).await?;
                sqlx::query("update twilio_accounts set error = null, failed_at = null where org_id = $1 and error is not null").bind(row.org_id).execute(db).await?;
                *room.get_mut(&t.project_id).expect("counted above") -= 1;
            }
            Err(Failure::Account(why)) => {
                sqlx::query("update text_messages set status = 'approved' where id = $1").bind(t.id).execute(db).await?;
                sqlx::query("update twilio_accounts set error = $2, failed_at = now() where org_id = $1").bind(row.org_id).bind(&why).execute(db).await?;
                tracing::warn!(org_id = %row.org_id, error = %why, "twilio held the org's texts");
                accounts.insert(row.org_id, None);
            }
            Err(e) => {
                sqlx::query("update text_messages set status = 'failed', error = $2 where id = $1").bind(t.id).bind(e.reason()).execute(db).await?;
            }
        }
    }
    lock.commit().await?;
    Ok(())
}

/// A text Twilio posted to /api/twilio/inbound, already checked to come from this org's account.
pub struct Inbound<'a> {
    pub sid: &'a str,
    pub from: &'a str,
    pub body: &'a str,
    pub media: bool,
}

/// Stores a lead's answer in their inbox thread, stops their sequences and has it classified like an email reply. A STOP
/// word puts the number on the do-not-contact list and unsubscribes the lead instead. A MessageSid already stored is left.
/// Returns the lead it matched.
pub async fn on_inbound(state: &AppState, org_id: Uuid, msg: &Inbound<'_>) -> anyhow::Result<Option<Uuid>> {
    let db = &state.db;
    let channel = if msg.from.starts_with("whatsapp:") { "whatsapp" } else { "sms" };
    let Some(number) = phone::normalize(msg.from, "") else { return Ok(None) };
    let stop = is_stop(msg.body);
    if stop {
        sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, $2, 'phone', 'unsubscribe') on conflict (org_id, value) do nothing")
            .bind(org_id)
            .bind(&number)
            .execute(db)
            .await?;
    }
    // The one they were last texted as, when several of the org's leads share the number.
    let Some(lead) = sqlx::query_as::<_, Lead>(
        "select l.* from leads l join projects p on p.id = l.project_id
         where p.org_id = $1 and l.phone = $2
         order by (select max(t.sent_at) from text_messages t where t.lead_id = l.id) desc nulls last, l.created_at desc limit 1",
    )
    .bind(org_id)
    .bind(&number)
    .fetch_optional(db)
    .await?
    else {
        return Ok(None);
    };
    // The side effects go first and are safe to repeat, and the message is stored last, so a Twilio retry after any failure
    // here finds no MessageSid yet and does everything again.
    let channel_why = if channel == "whatsapp" { "They replied on WhatsApp" } else { "They replied by text" };
    if stop {
        crate::mail::suppress_lead(state, &lead, Some(org_id), "unsubscribed", "unsubscribe").await?;
    } else {
        crate::routes::linkedin::stop_for_reply(db, &lead, channel_why).await?;
    }
    let body = if msg.body.trim().is_empty() && msg.media { "(sent a picture or file)".to_string() } else { msg.body.trim().chars().take(4000).collect() };
    let mut tx = db.begin().await?;
    let stored: Option<Uuid> = sqlx::query_scalar(
        "insert into email_messages (project_id, lead_id, segment_id, direction, kind, step, body, status, channel, twilio_sid, classification,
                                     classification_reason, handled_at)
         values ($1, $2, $3, 'inbound', 'inbound', 0, $4, 'received', $5, $6, $7, $8, case when $9 then now() end)
         on conflict (twilio_sid) do nothing returning id",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(lead.segment_id)
    .bind(&body)
    .bind(channel)
    .bind(msg.sid)
    .bind(stop.then_some("unsubscribe"))
    .bind(if stop { "They texted STOP" } else { "" })
    .bind(stop)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = stored.filter(|_| !stop) {
        agent::enqueue(&mut *tx, kind::CLASSIFY_REPLY, Some(lead.project_id), Some(org_id), json!({ "message_id": id })).await?;
        tracing::info!(lead = %lead.id, channel, "stored a text reply");
    }
    tx.commit().await?;
    Ok(Some(lead.id))
}

/// What Twilio's delivery error codes mean, in the words the Texts page shows.
fn delivery_error(code: &str) -> String {
    let said = match code {
        "30003" => "The phone is off or out of reach",
        "30004" => "The number blocks texts",
        "30005" => "The number doesn't exist",
        "30006" => "It's a landline or a number that can't get texts",
        "30007" => "The carrier filtered it as spam",
        "30008" => "The carrier couldn't say why",
        "21610" => "The number replied STOP to this sender before",
        "63003" => "The number isn't on WhatsApp",
        "63016" => "WhatsApp only takes an approved template outside the 24-hour window",
        "" => "Twilio couldn't deliver it",
        _ => return format!("Twilio couldn't deliver it (error {code})"),
    };
    if code.is_empty() { said.to_string() } else { format!("{said} (Twilio error {code})") }
}

/// A delivery update for a text Splee sent from this org's account. Only delivered, undelivered and failed change it.
pub async fn on_status(db: &PgPool, org_id: Uuid, sid: &str, status: &str, error_code: &str) -> sqlx::Result<()> {
    let (status, error) = match status {
        "delivered" => ("delivered", String::new()),
        "undelivered" | "failed" => ("failed", delivery_error(error_code)),
        _ => return Ok(()),
    };
    sqlx::query(
        "update text_messages t set status = $3, error = $4 from projects p
         where p.id = t.project_id and p.org_id = $1 and t.twilio_sid = $2 and t.status in ('sent', 'delivered')",
    )
    .bind(org_id)
    .bind(sid)
    .bind(status)
    .bind(error)
    .execute(db)
    .await?;
    Ok(())
}

/// Twilio's API as its docs describe it, for tests here and in the routes. The good API key is SKgood/secret, the auth token
/// AC…/token. +15005550009 isn't a mobile number, and the template HXintro has two variables.
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::{Form, Json, Router};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::{Value, json};

    // Made up, and split so GitHub's secret scanning doesn't take them for real Twilio credentials.
    pub const ACCOUNT: &str = concat!("AC", "0123456789abcdef", "0123456789abcdef");
    pub const KEY: &str = concat!("SK", "0123456789abcdef", "0123456789abcdef");
    pub const TEMPLATE: &str = concat!("HX", "0123456789abcdef", "0123456789abcdef");
    pub const APP: &str = concat!("AP", "0123456789abcdef", "0123456789abcdef");
    pub const RECORDING: &str = concat!("RE", "0123456789abcdef", "0123456789abcdef");

    pub type Sent = Arc<Mutex<Vec<HashMap<String, String>>>>;

    fn authed(headers: &HeaderMap) -> bool {
        let given = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Basic ")).unwrap_or_default();
        let pair = String::from_utf8(STANDARD.decode(given).unwrap_or_default()).unwrap_or_default();
        pair == format!("{KEY}:secret") || pair == format!("{ACCOUNT}:token")
    }

    fn refused() -> (StatusCode, Json<Value>) {
        (StatusCode::UNAUTHORIZED, Json(json!({ "code": 20003, "message": "Authenticate", "status": 401 })))
    }

    pub async fn start() -> (String, Sent) {
        async fn account(headers: HeaderMap, Path(file): Path<String>) -> (StatusCode, Json<Value>) {
            if !authed(&headers) || file != format!("{ACCOUNT}.json") {
                return refused();
            }
            (StatusCode::OK, Json(json!({ "sid": ACCOUNT, "friendly_name": "Fieldnote", "status": "active" })))
        }
        async fn message(State(sent): State<Sent>, headers: HeaderMap, Form(form): Form<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            if !authed(&headers) {
                return refused();
            }
            if form.get("To").is_some_and(|to| to.ends_with("+15005550009")) {
                return (StatusCode::BAD_REQUEST, Json(json!({ "code": 21614, "message": "'To' number is not a valid mobile number", "status": 400 })));
            }
            let mut sent = sent.lock().unwrap();
            sent.push(form);
            (StatusCode::CREATED, Json(json!({ "sid": format!("SM{:032}", sent.len()), "status": "queued" })))
        }
        async fn content(headers: HeaderMap, Path(sid): Path<String>) -> (StatusCode, Json<Value>) {
            if !authed(&headers) {
                return refused();
            }
            if sid != TEMPLATE {
                return (StatusCode::NOT_FOUND, Json(json!({ "code": 20404, "message": "The requested resource was not found", "status": 404 })));
            }
            (StatusCode::OK, Json(json!({
                "sid": TEMPLATE, "friendly_name": "intro", "language": "en", "variables": { "2": "Acme", "1": "Sam" },
                "types": { "twilio/text": { "body": "Hi {{1}}, it's Alex from Fieldnote. Can I send {{2}} a two-minute video?" } },
            })))
        }
        // A TwiML App whose Voice URL starts out pointing elsewhere. Updates land in `sent` like messages do.
        async fn application(headers: HeaderMap, Path((_, file)): Path<(String, String)>) -> (StatusCode, Json<Value>) {
            if !authed(&headers) {
                return refused();
            }
            if file != format!("{APP}.json") {
                return (StatusCode::NOT_FOUND, Json(json!({ "code": 20404, "message": "The requested resource was not found", "status": 404 })));
            }
            (StatusCode::OK, Json(json!({ "sid": APP, "friendly_name": "Splee calls", "voice_url": "https://old.example/voice", "voice_method": "GET" })))
        }
        async fn update_application(State(sent): State<Sent>, headers: HeaderMap, Form(form): Form<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
            if !authed(&headers) {
                return refused();
            }
            sent.lock().unwrap().push(form);
            (StatusCode::OK, Json(json!({ "sid": APP })))
        }
        async fn recording(headers: HeaderMap, Path((_, file)): Path<(String, String)>) -> (StatusCode, Vec<u8>) {
            if !authed(&headers) {
                return (StatusCode::UNAUTHORIZED, vec![]);
            }
            if file != format!("{RECORDING}.mp3") {
                return (StatusCode::NOT_FOUND, vec![]);
            }
            (StatusCode::OK, b"ID3 fake mp3".to_vec())
        }
        let sent = Sent::default();
        let app = Router::new()
            .route("/2010-04-01/Accounts/{file}", get(account))
            .route("/2010-04-01/Accounts/{sid}/Messages.json", post(message))
            .route("/2010-04-01/Accounts/{sid}/Applications/{file}", get(application).post(update_application))
            .route("/2010-04-01/Accounts/{sid}/Recordings/{file}", get(recording))
            .route("/v1/Content/{sid}", get(content))
            .with_state(sent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, sent)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn signatures_match_twilios_own_example() {
        // From Twilio's webhook security docs.
        let url = "https://mycompany.com/myapp.php?foo=1&bar=2";
        let params: Vec<(String, String)> = [("CallSid", "CA1234567890ABCDE"), ("Caller", "+12349013030"), ("Digits", "1234"), ("From", "+12349013030"), ("To", "+18005551212")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let token = "12345";
        let header = signature(token, url, &params);
        assert_eq!(header, "0/KCTR6DLpKmkAf8muzZqo1nDgQ=");
        assert!(verify(token, url, &params, &header));
        assert!(!verify("other", url, &params, &header));
        assert!(!verify(token, "https://mycompany.com/other.php?foo=1&bar=2", &params, &header));
        assert!(!verify(token, url, &params[1..], &header));
        assert!(!verify(token, url, &params, "not base64!"));
    }

    #[test]
    fn a_text_names_its_sender_fits_and_ends_with_stop_the_first_time() {
        let text = finish_sms("Hi Sam, saw your new clinic in Leeds. Worth a quick chat?", "Alex", "Fieldnote", "+447700900123", true);
        assert_eq!(text, "Hi Sam, saw your new clinic in Leeds. Worth a quick chat?\nAlex\nReply STOP to opt out.");
        // Named already, a later text, and a country without the rule.
        assert_eq!(finish_sms("Hi Sam, Alex here again.", "Alex", "Fieldnote", "+447700900123", false), "Hi Sam, Alex here again.");
        assert_eq!(finish_sms("Hi Sam, Alex here.", "Alex", "", "+491701234567", true), "Hi Sam, Alex here.");
        assert_eq!(finish_sms("Hi Sam.", "", "Fieldnote", "+491701234567", true), "Hi Sam.\nFieldnote");

        let long = format!("Hi Sam, Alex here. {} Last sentence that runs on and on.", "A middle sentence. ".repeat(20));
        let text = finish_sms(&long, "Alex", "Fieldnote", "+14155550134", true);
        assert!(text.chars().count() <= SMS_MAX_CHARS, "{}", text.len());
        assert!(text.ends_with("A middle sentence.\nReply STOP to opt out."), "{text}");
    }

    #[test]
    fn only_a_stop_word_alone_opts_out() {
        for body in ["STOP", " stop ", "Stop.", "STOPALL", "unsubscribe", "Cancel", "end", "QUIT!"] {
            assert!(is_stop(body), "{body}");
        }
        for body in ["Don't stop", "stop by Tuesday?", "Ended up booking", ""] {
            assert!(!is_stop(body), "{body}");
        }
    }

    #[test]
    fn templates_fill_from_the_lead_with_words_for_the_gaps() {
        let lead = Lead { first_name: "Sam".into(), company_name: String::new(), ..test_lead() };
        let values = template_values(&json!({ "1": "first_name", "2": "company_name", "3": "sender_name" }), &lead, "Alex");
        assert_eq!(values, json!({ "1": "Sam", "2": "your team", "3": "Alex" }));
        assert_eq!(fill_template("Hi {{1}}, {{3}} here about {{2}}.", &values), "Hi Sam, Alex here about your team.");
        let nameless = template_values(&json!({ "1": "first_name" }), &test_lead(), "");
        assert_eq!(nameless, json!({ "1": "there" }));
    }

    #[tokio::test]
    async fn credentials_and_templates_are_checked_with_twilio() {
        let (base, sent) = fake::start().await;
        assert_eq!(Client::new(&base, fake::ACCOUNT, fake::KEY, "secret").check().await, Ok("Fieldnote".into()));
        assert_eq!(Client::new(&base, fake::ACCOUNT, fake::ACCOUNT, "token").check().await, Ok("Fieldnote".into()));
        let wrong = Client::new(&base, fake::ACCOUNT, fake::KEY, "nope").check().await;
        assert!(matches!(&wrong, Err(Failure::Account(r)) if r.contains("Authenticate (Twilio error 20003)")), "{wrong:?}");

        let client = Client::new(&base, fake::ACCOUNT, fake::KEY, "secret");
        let (body, vars) = client.template(fake::TEMPLATE).await.unwrap();
        assert_eq!((body.as_str(), vars), ("Hi {{1}}, it's Alex from Fieldnote. Can I send {{2}} a two-minute video?", vec!["1".to_string(), "2".to_string()]));
        assert!(matches!(client.template("HX0000").await, Err(Failure::Message(_))));

        let account = Account {
            account_sid: fake::ACCOUNT.into(),
            api_key_sid: fake::KEY.into(),
            api_secret_enc: String::new(),
            sms_from: "MG0123456789abcdef0123456789abcdef".into(),
            whatsapp_from: "+14155550100".into(),
            whatsapp_template_sid: String::new(),
            whatsapp_template_body: String::new(),
            whatsapp_template_vars: json!({}),
        };
        client.send(&account, "sms", "+14155550134", &Content::Text("Hi"), "https://splee.test/api/twilio/status").await.unwrap();
        let vars = json!({ "1": "Sam" });
        client.send(&account, "whatsapp", "+14155550134", &Content::Template { sid: fake::TEMPLATE, vars: &vars }, "x").await.unwrap();
        let not_mobile = client.send(&account, "sms", "+15005550009", &Content::Text("Hi"), "x").await;
        assert!(matches!(not_mobile, Err(Failure::Message(r)) if r == "'To' number is not a valid mobile number (Twilio error 21614)"));
        let sent = sent.lock().unwrap();
        assert_eq!((sent[0]["MessagingServiceSid"].as_str(), sent[0]["Body"].as_str()), ("MG0123456789abcdef0123456789abcdef", "Hi"));
        assert_eq!(sent[0]["StatusCallback"], "https://splee.test/api/twilio/status");
        assert_eq!((sent[1]["From"].as_str(), sent[1]["To"].as_str()), ("whatsapp:+14155550100", "whatsapp:+14155550134"));
        assert_eq!((sent[1]["ContentSid"].as_str(), sent[1]["ContentVariables"].as_str(), sent[1].get("Body")), (fake::TEMPLATE, "{\"1\":\"Sam\"}", None));
    }

    pub(crate) fn test_lead() -> Lead {
        Lead {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            segment_id: None,
            company_id: None,
            full_name: String::new(),
            first_name: String::new(),
            last_name: String::new(),
            title: String::new(),
            email: None,
            email_status: "unknown".into(),
            linkedin_url: String::new(),
            x_url: String::new(),
            phone: String::new(),
            phone_source: String::new(),
            company_name: String::new(),
            company_domain: None,
            city: String::new(),
            country_code: String::new(),
            source: "import".into(),
            status: "new".into(),
            personalization: json!({}),
            notes: String::new(),
            remind_at: None,
            meeting_at: None,
            unsubscribe_token: String::new(),
            last_contacted_at: None,
            created_at: Utc::now(),
            variant_id: None,
            email_search: String::new(),
            email_source: String::new(),
            email_search_note: String::new(),
            email_search_lookups: 0,
            email_search_credits: None,
            email_searched_at: None,
            score: 0,
            score_fit: 0,
            score_intent: 0,
            score_reasons: sqlx::types::Json(vec![]),
            scored_at: None,
            owner_id: None,
        }
    }
}
