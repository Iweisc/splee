//! Client reports, for agencies and teams that run outreach for someone else: read-only public links to one project's
//! Analytics. A link's token is shown once and kept only as its sha256. The shared page (routes::reports, /report/{token}
//! in the frontend) shows the link's date range without anything internal: spend and mailbox addresses only when the link
//! says so, lead emails never. email.rs sends each link's weekly or monthly summary.

pub mod email;
#[cfg(test)]
mod tests;

use chrono::{Datelike, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use serde::Serialize;
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::crypto::{keyed_hash, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::routes::analytics::{self, AnalyticsQuery};
use crate::state::AppState;

pub const RANGES: [&str; 4] = ["last_7", "last_30", "last_90", "month_to_date"];
/// Meetings and hot replies a page lists, newest first.
const MAX_LISTED: usize = 20;
const EXCERPT_CHARS: usize = 160;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ReportLink {
    pub id: Uuid,
    pub org_id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub intro: String,
    pub brand: String,
    pub logo_url: String,
    pub range_preset: String,
    pub show_spend: bool,
    pub show_mailboxes: bool,
    pub show_leads: bool,
    pub email_schedule: String,
    #[serde(skip)]
    pub emailed_period: Option<NaiveDate>,
    pub expires_at: Option<Ts>,
    pub revoked_at: Option<Ts>,
    pub last_viewed_at: Option<Ts>,
    pub created_by: Option<Uuid>,
    pub created_at: Ts,
}

impl ReportLink {
    pub fn live(&self, now: Ts) -> bool {
        self.revoked_at.is_none() && self.expires_at.is_none_or(|at| at > now)
    }
}

/// The first and last day a preset covers on `today`, inclusive.
pub fn range(preset: &str, today: NaiveDate) -> (NaiveDate, NaiveDate) {
    let days = match preset {
        "last_7" => 7,
        "last_90" => 90,
        "month_to_date" => return (today.with_day(1).unwrap_or(today), today),
        _ => 30,
    };
    (today - Duration::days(days - 1), today)
}

pub const VIEW: &str = "report-view";
pub const UNSUBSCRIBE: &str = "report-unsubscribe";

/// A recipient's own token for `purpose`: their id and a keyed hash of it. The emails link to the page with it, since
/// Splee doesn't keep the link's own token, and to the unsubscribe that stops only their copy.
pub fn recipient_token(secret: &str, purpose: &str, id: Uuid) -> String {
    format!("{}.{}", id.simple(), keyed_hash(secret, purpose, &id.to_string()))
}

pub fn recipient_from(secret: &str, purpose: &str, token: &str) -> Option<Uuid> {
    let (id, sig) = token.split_once('.')?;
    let id: Uuid = id.parse().ok()?;
    crate::crypto::verify_keyed_hash(secret, purpose, &id.to_string(), sig).then_some(id)
}

/// The live link a page token opens: the link's own, or a recipient's from an email. Revoked, expired and unknown are 404.
pub async fn by_token(state: &AppState, token: &str) -> AppResult<ReportLink> {
    let link: Option<ReportLink> = match recipient_from(&state.cfg.app_secret, VIEW, token) {
        Some(recipient) => {
            sqlx::query_as("select l.* from report_links l join report_recipients r on r.link_id = l.id where r.id = $1")
                .bind(recipient)
                .fetch_optional(&state.db)
                .await?
        }
        None => sqlx::query_as("select * from report_links where token_hash = $1").bind(sha256_hex(token)).fetch_optional(&state.db).await?,
    };
    link.filter(|l| l.live(Utc::now())).ok_or(AppError::NotFound)
}

/// Whose report it is, as the page and the email show it.
pub struct Brand {
    pub name: String,
    /// The organization sharing it, which the email's footer names.
    pub org: String,
    pub logo_url: String,
    pub tz: Tz,
}

pub async fn brand(db: &PgPool, link: &ReportLink) -> sqlx::Result<Brand> {
    let (project, domain, org, timezone): (String, Option<String>, String, String) = sqlx::query_as(
        "select p.name, p.domain, o.name, coalesce(ps.timezone, 'UTC') from projects p join organizations o on o.id = p.org_id
         left join project_settings ps on ps.project_id = p.id where p.id = $1",
    )
    .bind(link.project_id)
    .fetch_one(db)
    .await?;
    let org_brand = link.brand == "org";
    // Orgs have no website, so their logo is only ever the one the link sets.
    let logo_url = if link.logo_url.is_empty() {
        domain.filter(|d| !d.is_empty() && !org_brand).map(|d| format!("https://www.google.com/s2/favicons?domain={d}&sz=128")).unwrap_or_default()
    } else {
        link.logo_url.clone()
    };
    let name = if org_brand { org.clone() } else { project };
    Ok(Brand { name, org, logo_url, tz: timezone.parse().unwrap_or(Tz::UTC) })
}

/// A lead as a client may see them: no email address, no last name.
#[derive(Serialize, FromRow)]
pub struct Person {
    pub first_name: String,
    pub company: String,
    pub at: Ts,
    /// The start of the call, for a meeting whose scheduling tool said.
    #[sqlx(default)]
    pub meeting_at: Option<Ts>,
    /// The start of a hot reply, without the quoted thread or anything that looks like an address.
    #[sqlx(default)]
    pub excerpt: String,
}

#[derive(Serialize)]
pub struct SharedReport {
    pub name: String,
    pub logo_url: String,
    pub title: String,
    pub intro: String,
    pub range_preset: String,
    pub show_spend: bool,
    pub show_mailboxes: bool,
    pub show_leads: bool,
    /// routes::analytics::Analytics, less what the link leaves out. See redact.
    pub analytics: Value,
    pub meetings: Option<Vec<Person>>,
    pub hot_replies: Option<Vec<Person>>,
}

const SPEND: [&str; 5] = ["claude_cost_micros", "email_cost_micros", "spend_micros", "cost_per_hot_micros", "cost_per_meeting_micros"];

/// Takes out what a client shouldn't see unless the link says so. Analytics has no lead addresses to begin with.
pub fn redact(mut analytics: Value, link: &ReportLink) -> Value {
    if !link.show_spend {
        for totals in ["totals", "previous"] {
            if let Some(t) = analytics[totals].as_object_mut() {
                SPEND.iter().for_each(|k| drop(t.remove(*k)));
            }
        }
        for campaign in analytics["campaigns"].as_array_mut().into_iter().flatten().filter_map(Value::as_object_mut) {
            SPEND.iter().for_each(|k| drop(campaign.remove(*k)));
        }
    }
    if !link.show_mailboxes
        && let Some(a) = analytics.as_object_mut()
    {
        a.remove("mailboxes");
    }
    for campaign in analytics["campaigns"].as_array_mut().into_iter().flatten() {
        if let Some(name) = campaign["name"].as_str() {
            campaign["name"] = Value::from(no_addresses(name));
        }
    }
    analytics
}

/// `text` without its words that look like an email address. Names come from imports and forms, where an address can end
/// up in any field, and a client never sees a lead's.
pub fn no_addresses(text: &str) -> String {
    text.split_whitespace().filter(|w| !w.contains('@')).collect::<Vec<_>>().join(" ")
}

impl Person {
    fn scrubbed(self) -> Self {
        Person { first_name: no_addresses(&self.first_name), company: no_addresses(&self.company), ..self }
    }
}

/// The page as `link` shows it at `now`.
pub async fn shared(db: &PgPool, link: &ReportLink, now: Ts) -> AppResult<SharedReport> {
    let brand = brand(db, link).await?;
    let (from, to) = range(&link.range_preset, now.with_timezone(&brand.tz).date_naive());
    let numbers = analytics::analytics(db, link.project_id, &AnalyticsQuery { from: Some(from), to: Some(to), ..Default::default() }).await?;
    let analytics = redact(serde_json::to_value(numbers).map_err(anyhow::Error::from)?, link);
    let (meetings, hot_replies) = if link.show_leads {
        (Some(meetings(db, link.project_id, from, to, brand.tz).await?), Some(hot_replies(db, link.project_id, from, to, brand.tz).await?))
    } else {
        (None, None)
    };
    Ok(SharedReport {
        name: brand.name,
        logo_url: brand.logo_url,
        title: link.title.clone(),
        intro: link.intro.clone(),
        range_preset: link.range_preset.clone(),
        show_spend: link.show_spend,
        show_mailboxes: link.show_mailboxes,
        show_leads: link.show_leads,
        analytics,
        meetings,
        hot_replies,
    })
}

/// $2 and $3 are the first and last day, $4 the time zone they're in.
const FIRST_NAME: &str = "coalesce(nullif(l.first_name, ''), split_part(l.full_name, ' ', 1)) as first_name, l.company_name as company";

/// Leads booked in the range, counted the way Analytics counts meetings.
pub async fn meetings(db: &PgPool, project_id: Uuid, from: NaiveDate, to: NaiveDate, tz: Tz) -> sqlx::Result<Vec<Person>> {
    sqlx::query_as(&format!(
        "select {FIRST_NAME}, l.status_changed_at as at, l.meeting_at from leads l
         where l.project_id = $1 and l.status = 'meeting_booked'
           and l.status_changed_at >= $2::date::timestamp at time zone $4 and l.status_changed_at < ($3::date + 1)::timestamp at time zone $4
         order by l.status_changed_at desc limit {MAX_LISTED}"
    ))
    .bind(project_id)
    .bind(from)
    .bind(to)
    .bind(tz.name())
    .fetch_all(db)
    .await
    .map(|people: Vec<Person>| people.into_iter().map(Person::scrubbed).collect())
}

/// Each lead's latest reply the classifier called hot in the range.
async fn hot_replies(db: &PgPool, project_id: Uuid, from: NaiveDate, to: NaiveDate, tz: Tz) -> sqlx::Result<Vec<Person>> {
    let rows: Vec<(String, String, Ts, String)> = sqlx::query_as(&format!(
        "select * from (
           select distinct on (l.id) {FIRST_NAME}, e.created_at as at, e.body from email_messages e join leads l on l.id = e.lead_id
           where e.project_id = $1 and e.direction = 'inbound' and e.classification = 'hot'
             and e.created_at >= $2::date::timestamp at time zone $4 and e.created_at < ($3::date + 1)::timestamp at time zone $4
           order by l.id, e.created_at desc
         ) r order by at desc limit {MAX_LISTED}"
    ))
    .bind(project_id)
    .bind(from)
    .bind(to)
    .bind(tz.name())
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(first_name, company, at, body)| Person { first_name, company, at, meeting_at: None, excerpt: excerpt(&body) }.scrubbed()).collect())
}

/// The first EXCERPT_CHARS of a reply on one line. Words with an @ go, so a signature can't hand the lead's address out.
pub fn excerpt(body: &str) -> String {
    let text = crate::mail::inbound::strip_quoted(body);
    let words: Vec<&str> = text.split_whitespace().filter(|w| !w.contains('@')).collect();
    let mut out = String::new();
    for word in words {
        if out.chars().count() + word.chars().count() + 1 > EXCERPT_CHARS {
            out.push('…');
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}
