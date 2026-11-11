//! Lead scoring: a 0-100 score per lead and company that puts the likeliest buyers first, from the signals Splee already
//! keeps. Fit (0-50) is how well the company matches the targeting, and intent (0-50) is what it did lately, fading with
//! time. No Claude tokens: the worker reads each project's facts in a few set-based queries, scores them here and writes
//! them back in batches. It scores a project at most every THROTTLE_MINUTES, when something it reads changed, and every
//! REFRESH_MINUTES anyway so intent fades and "yesterday" stays true. refresh_lead scores one lead on demand.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::fingerprints;
use crate::models::Ts;

pub const FIT_MAX: f64 = 50.0;
pub const INTENT_MAX: f64 = 50.0;
const THROTTLE_MINUTES: i32 = 10;
const REFRESH_MINUTES: i32 = 60;
/// Rows per update statement, so one write never holds 50,000 rows' locks and arrays at once.
const WRITE_BATCH: usize = 5000;
const MAX_REASONS: usize = 4;

/// One input to the score. Fit weights are shares of FIT_MAX among the fit signals the project has set up, so a project
/// that only targets countries still scores fit out of 50. Intent weights are points, halved every `half_life` days,
/// counted for `window` days, and capped at INTENT_MAX together.
pub struct Signal {
    /// Its name in project_settings.score_signals.
    pub key: &'static str,
    pub label: &'static str,
    pub weight: f64,
    pub half_life: f64,
    pub window: i32,
}

const fn fit(key: &'static str, label: &'static str, weight: f64) -> Signal {
    Signal { key, label, weight, half_life: 0.0, window: 0 }
}

/// In a country the project or the lead's campaign targets. Exclusions alone count too.
pub const COUNTRY: Signal = fit("country", "Country", 10.0);
/// Headcount in a targeted size band.
pub const SIZE: Signal = fit("size", "Company size", 10.0);
/// Name, description or company database category mentions one of the campaign's industries or keywords.
pub const INDUSTRY: Signal = fit("industry", "Industry", 15.0);
/// Its site uses a tool picked as a good sign. Only counted once the site has been scanned.
pub const TECH: Signal = fit("tech", "Tools they use", 10.0);
/// Its site shows none of the competitor tools. A company that uses one loses these points and says why.
pub const NO_COMPETITOR: Signal = fit("competitor", "No competitor tool", 10.0);
/// The lead's title holds one of the campaign's roles, or is as senior as one (a VP for a VP of Sales).
pub const PERSONA: Signal = fit("persona", "Title", 15.0);
/// Open posts for the watched roles, first posted in the last 30 days. A fresh post is the clearest budget signal.
pub const HIRING: Signal = Signal { key: "hiring", label: "Hiring", weight: 20.0, half_life: 10.0, window: 30 };
/// Any page view from the company's network. Fades fast: a visit from last week says little.
pub const VISIT: Signal = Signal { key: "visit", label: "Site visits", weight: 10.0, half_life: 3.0, window: 14 };
/// A view of one of the Visitors tab's high-intent pages, like /pricing.
pub const INTENT_PAGE: Signal = Signal { key: "intent_page", label: "High-intent pages", weight: 15.0, half_life: 3.0, window: 14 };
/// The lead replied and the reply was classified as a question.
pub const QUESTION: Signal = Signal { key: "question", label: "Replied with a question", weight: 20.0, half_life: 7.0, window: 30 };
/// The lead has an open deal. Fades slowly, since a deal that sits still is going cold.
pub const DEAL: Signal = Signal { key: "deal", label: "Open deal", weight: 25.0, half_life: 30.0, window: 365 };

pub const SIGNALS: [&Signal; 11] = [&COUNTRY, &SIZE, &INDUSTRY, &TECH, &NO_COMPETITOR, &PERSONA, &HIRING, &VISIT, &INTENT_PAGE, &QUESTION, &DEAL];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reason {
    pub text: String,
    /// "fit", "intent", or "against" for what holds the score down.
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Score {
    pub score: i16,
    pub fit: i16,
    pub intent: i16,
    pub reasons: Vec<Reason>,
}

/// What a campaign targets. Leads without a campaign get every campaign's lists together.
#[derive(Debug, Default, Clone)]
pub struct Campaign {
    pub countries: Vec<String>,
    pub sizes: Vec<String>,
    pub industries: Vec<String>,
    pub roles: Vec<String>,
}

/// Everything a score depends on besides the company and lead themselves.
#[derive(Debug, Clone)]
pub struct Targeting {
    pub countries: Vec<String>,
    pub exclude_countries: Vec<String>,
    pub sizes: Vec<String>,
    pub campaigns: HashMap<Uuid, Campaign>,
    pub any_campaign: Campaign,
    pub uses: Vec<String>,
    pub competitors: Vec<String>,
    pub off: HashSet<String>,
    pub tz: Tz,
    pub now: Ts,
}

#[derive(Debug, Default, Clone)]
pub struct CompanyFacts {
    pub segment_id: Option<Uuid>,
    pub country_code: String,
    pub employees: String,
    /// Name, description and company database category, lowercase, for the industry keywords.
    pub text: String,
    /// None until the site has been scanned.
    pub techs: Option<Vec<String>>,
    /// Open posts for watched roles in the hiring window, newest first.
    pub hiring: Vec<(String, Ts)>,
    pub views: i64,
    pub last_view: Option<Ts>,
    pub intent_page: Option<(String, Ts)>,
}

#[derive(Debug, Default, Clone)]
pub struct LeadFacts {
    pub segment_id: Option<Uuid>,
    pub title: String,
    pub question_at: Option<Ts>,
    /// The open deal's stage and when it opened.
    pub deal: Option<(String, Ts)>,
}

impl Targeting {
    fn on(&self, s: &Signal) -> bool {
        !self.off.contains(s.key)
    }

    fn campaign(&self, segment_id: Option<Uuid>) -> &Campaign {
        segment_id.and_then(|id| self.campaigns.get(&id)).unwrap_or(&self.any_campaign)
    }

    /// "today", "yesterday" or "3 days ago", by the project's calendar.
    fn when(&self, at: Ts) -> String {
        let days = (self.now.with_timezone(&self.tz).date_naive() - at.with_timezone(&self.tz).date_naive()).num_days();
        match days {
            ..=0 => "today".into(),
            1 => "yesterday".into(),
            n => format!("{n} days ago"),
        }
    }

    /// The signal's points for something that happened `at`, or None once it's outside the window.
    fn fade(&self, s: &Signal, at: Ts) -> Option<f64> {
        let days = ((self.now - at).num_seconds() as f64 / 86_400.0).max(0.0);
        (self.on(s) && days <= f64::from(s.window)).then(|| s.weight * 0.5f64.powf(days / s.half_life))
    }
}

/// Scores a company, through `lead` when it's one of its leads. Without a lead, the title and the lead's own signals
/// (replies and deals) don't count.
pub fn score(t: &Targeting, company: &CompanyFacts, lead: Option<&LeadFacts>) -> Score {
    let campaign = t.campaign(lead.and_then(|l| l.segment_id).or(company.segment_id));
    let mut possible = 0.0;
    let mut fits: Vec<(f64, String)> = vec![];
    let mut against: Option<String> = None;
    let mut add_fit = |s: &Signal, set_up: bool, hit: Option<String>| {
        if t.on(s) && set_up {
            possible += s.weight;
            if let Some(text) = hit {
                fits.push((s.weight, text));
            }
        }
    };

    let cc = company.country_code.trim().to_uppercase();
    let includes: Vec<&Vec<String>> = [&t.countries, &campaign.countries].into_iter().filter(|l| !l.is_empty()).collect();
    let country_hit = !cc.is_empty()
        && !t.exclude_countries.iter().any(|c| c.eq_ignore_ascii_case(&cc))
        && includes.iter().all(|l| l.iter().any(|c| c.trim().eq_ignore_ascii_case(&cc)));
    add_fit(&COUNTRY, !includes.is_empty() || !t.exclude_countries.is_empty(), country_hit.then(|| format!("Based in {cc}")));

    let size = company.employees.trim();
    let bands: Vec<&Vec<String>> = [&t.sizes, &campaign.sizes].into_iter().filter(|l| !l.is_empty()).collect();
    let size_hit = !size.is_empty() && bands.iter().all(|l| l.iter().any(|b| b.trim() == size));
    add_fit(&SIZE, !bands.is_empty(), size_hit.then(|| format!("{size} employees")));

    let text = words(&company.text);
    let industry = campaign.industries.iter().find(|k| mentions(&text, k));
    add_fit(&INDUSTRY, !campaign.industries.is_empty(), industry.map(|k| format!("Matches your industry: {}", k.trim())));

    if let Some(techs) = &company.techs {
        let has = |picks: &[String]| picks.iter().filter(|p| techs.contains(p)).cloned().collect::<Vec<_>>();
        add_fit(&TECH, !t.uses.is_empty(), fingerprints::fact_line(&has(&t.uses)));
        let rivals = has(&t.competitors);
        if t.on(&NO_COMPETITOR) && !rivals.is_empty() {
            against = Some(format!("Uses a competitor: {}", rivals.join(", ")));
        }
        add_fit(&NO_COMPETITOR, !t.competitors.is_empty(), rivals.is_empty().then(|| "Uses none of your competitors".to_string()));
    }

    if let Some(lead) = lead {
        add_fit(&PERSONA, !campaign.roles.is_empty(), persona(&lead.title, &campaign.roles).map(|role| format!("Title matches your buyer: {role}")));
    }

    let scale = if possible > 0.0 { FIT_MAX / possible } else { 0.0 };
    let mut reasons: Vec<(f64, String, &str)> = fits.into_iter().map(|(points, text)| (points * scale, text, "fit")).collect();
    let fit = reasons.iter().map(|r| r.0).sum::<f64>();

    let mut intents: Vec<(f64, String)> = vec![];
    if let Some(points) = company.hiring.first().and_then(|(_, at)| t.fade(&HIRING, *at)) {
        intents.push((points, hiring_text(&company.hiring)));
    }
    if let Some(at) = company.last_view
        && let Some(points) = t.fade(&VISIT, at)
    {
        let when = t.when(at);
        let text = match company.views {
            1 => format!("Visited your site {when}"),
            n => format!("Viewed {n} pages on your site, last {when}"),
        };
        intents.push((points, text));
    }
    if let Some((path, at)) = &company.intent_page
        && let Some(points) = t.fade(&INTENT_PAGE, *at)
    {
        intents.push((points, format!("Read {path} {}", t.when(*at))));
    }
    if let Some(lead) = lead {
        if let Some(at) = lead.question_at
            && let Some(points) = t.fade(&QUESTION, at)
        {
            intents.push((points, format!("Replied with a question {}", t.when(at))));
        }
        if let Some((stage, at)) = &lead.deal
            && let Some(points) = t.fade(&DEAL, *at)
        {
            intents.push((points, format!("Open deal in {stage}")));
        }
    }
    let intent = intents.iter().map(|r| r.0).sum::<f64>().min(INTENT_MAX);
    reasons.extend(intents.into_iter().map(|(points, text)| (points, text, "intent")));

    reasons.sort_by(|a, b| b.0.total_cmp(&a.0));
    let room = MAX_REASONS - usize::from(against.is_some());
    let mut listed: Vec<Reason> = reasons.into_iter().take(room).map(|(_, text, kind)| Reason { text, kind: kind.into() }).collect();
    listed.extend(against.map(|text| Reason { text, kind: "against".into() }));
    let (fit, intent) = (fit.round() as i16, intent.round() as i16);
    Score { score: fit + intent, fit, intent, reasons: listed }
}

/// Lowercase words between single spaces, with a space at each end, so a keyword only matches whole words.
fn words(text: &str) -> String {
    let words: Vec<String> = text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect();
    format!(" {} ", words.join(" "))
}

/// Whether `text` from `words` holds the keyword as whole words, singular or plural ("dental clinics" finds a dental clinic,
/// and "IT" doesn't find "with").
fn mentions(text: &str, keyword: &str) -> bool {
    let k = words(keyword);
    let k = k.trim();
    let has = |phrase: &str| text.contains(&format!(" {phrase} "));
    !k.is_empty() && (has(k) || has(&format!("{k}s")) || k.strip_suffix('s').is_some_and(has))
}

/// 4 for owners and the C-suite, 3 for VPs, 2 for heads and directors, 1 for managers and leads, else 0.
fn seniority(title: &str) -> u8 {
    let title = words(title);
    let has = |list: &[&str]| list.iter().any(|w| title.contains(&format!(" {w} ")));
    // Before the C-suite, so a Vice President isn't read as a president.
    if has(&["vp", "svp", "evp", "vice"]) {
        3
    } else if has(&["owner", "founder", "cofounder", "ceo", "cto", "coo", "cfo", "cmo", "cro", "chief", "president", "partner", "principal"]) {
        4
    } else if has(&["head", "director"]) {
        2
    } else if has(&["manager", "lead"]) {
        1
    } else {
        0
    }
}

/// The campaign role the title matches: one it contains, else the first role at the same seniority.
fn persona<'a>(title: &str, roles: &'a [String]) -> Option<&'a str> {
    let t = words(title);
    if t.trim().is_empty() {
        return None;
    }
    let level = seniority(title);
    roles
        .iter()
        .find(|r| mentions(&t, r))
        .or_else(|| roles.iter().find(|r| level > 0 && seniority(r) == level))
        .map(|r| r.trim())
}

/// "Hiring 2 SDRs", "Hiring an Account Executive and 1 more role", for the role with the most posts.
fn hiring_text(posts: &[(String, Ts)]) -> String {
    let mut counts: Vec<(&str, usize)> = vec![];
    for (role, _) in posts {
        match counts.iter_mut().find(|(r, _)| r.eq_ignore_ascii_case(role)) {
            Some((_, n)) => *n += 1,
            None => counts.push((role.as_str(), 1)),
        }
    }
    // Stable, so a tie goes to the newest post's role.
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    let (role, n) = counts[0];
    let head = if n == 1 { format!("Hiring {} {role}", article(role)) } else { format!("Hiring {n} {}", plural(role)) };
    match posts.len() - n {
        0 => head,
        1 => format!("{head} and 1 more role"),
        more => format!("{head} and {more} more roles"),
    }
}

fn is_acronym(word: &str) -> bool {
    word.len() <= 5 && word.chars().all(|c| c.is_ascii_uppercase())
}

fn article(role: &str) -> &'static str {
    let first = role.chars().next().unwrap_or('x');
    // An acronym is read letter by letter, and "an SDR" is said "an ess-dee-ar".
    let vowel = if is_acronym(role.split(' ').next().unwrap_or_default()) { "AEFHILMNORSX".contains(first) } else { "AEIOUaeiou".contains(first) };
    if vowel { "an" } else { "a" }
}

fn plural(role: &str) -> String {
    if let Some((head, tail)) = role.split_once(" of ") {
        return format!("{} of {tail}", plural(head));
    }
    if role.ends_with('s') && !is_acronym(role.rsplit(' ').next().unwrap_or_default()) {
        return role.to_string();
    }
    match role.strip_suffix('y') {
        Some(stem) if !stem.ends_with(['a', 'e', 'o', 'u']) => format!("{stem}ies"),
        _ => format!("{role}s"),
    }
}

pub fn spawn_worker(db: PgPool) {
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(60), || tick(&db), "lead scores").await });
}

/// Scores each due project once, skipping one another backend is scoring.
pub async fn tick(db: &PgPool) -> anyhow::Result<()> {
    for project_id in due(db).await? {
        let mut lock = db.begin().await?;
        let owned: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended('score:' || $1::text, 0))")
            .bind(project_id)
            .fetch_one(&mut *lock)
            .await?;
        // The list predates the lock, so another backend may have scored this project in between.
        let still_due: bool = sqlx::query_scalar(
            "select coalesce(scored_at is null or scored_at < now() - make_interval(mins => $2), false) from project_settings where project_id = $1",
        )
        .bind(project_id)
        .bind(THROTTLE_MINUTES)
        .fetch_optional(&mut *lock)
        .await?
        .unwrap_or(false);
        if owned && still_due {
            recompute(db, project_id).await?;
        }
        lock.commit().await?;
    }
    Ok(())
}

/// Projects not scored in THROTTLE_MINUTES whose inputs changed since, or that weren't scored in REFRESH_MINUTES. Each
/// check is an index lookup, and projects with no leads or companies are left out. Demo projects too.
pub async fn due(db: &PgPool) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "select p.id from projects p join project_settings s on s.project_id = p.id
         where p.org_id is not null
           and (exists (select 1 from leads l where l.project_id = p.id) or exists (select 1 from companies c where c.project_id = p.id))
           and (s.scored_at is null or (s.scored_at < now() - make_interval(mins => $1) and (
                s.scored_at < now() - make_interval(mins => $2)
                or s.score_inputs_at > s.scored_at
                or exists (select 1 from leads l where l.project_id = p.id and l.created_at > s.scored_at)
                or exists (select 1 from companies c where c.project_id = p.id and c.created_at > s.scored_at)
                or exists (select 1 from visitor_hits h where h.project_id = p.id and h.created_at > s.scored_at and h.domain is not null)
                or exists (select 1 from job_openings j where j.project_id = p.id and (j.first_seen > s.scored_at or j.closed_at > s.scored_at))
                or exists (select 1 from email_messages e where e.project_id = p.id and e.status = 'received' and e.created_at > s.scored_at)
                or exists (select 1 from deals d where d.project_id = p.id
                           and (d.created_at > s.scored_at or d.stage_changed_at > s.scored_at or d.closed_at > s.scored_at)))))
         order by s.scored_at nulls first",
    )
    .bind(THROTTLE_MINUTES)
    .bind(REFRESH_MINUTES)
    .fetch_all(db)
    .await
}

/// Marks the project's settings as changed, so the next tick scores it again even within the hour.
pub async fn touch(db: &PgPool, project_id: Uuid) -> sqlx::Result<()> {
    sqlx::query("update project_settings set score_inputs_at = now() where project_id = $1").bind(project_id).execute(db).await?;
    Ok(())
}

fn list(v: &Value, key: &str) -> Vec<String> {
    v[key].as_array().into_iter().flatten().filter_map(|x| x.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

fn extend_unique(into: &mut Vec<String>, from: &[String]) {
    for item in from {
        if !into.iter().any(|x| x.eq_ignore_ascii_case(item)) {
            into.push(item.clone());
        }
    }
}

pub async fn targeting(db: &PgPool, project_id: Uuid) -> sqlx::Result<Targeting> {
    // A demo project has no settings row, and scores against nothing but its signals.
    let (settings, signals, timezone, now): (Value, Value, String, Ts) = sqlx::query_as(
        "select coalesce(s.targeting, '{}'), coalesce(s.score_signals, '{}'), coalesce(s.timezone, 'UTC'), now()
         from projects p left join project_settings s on s.project_id = p.id where p.id = $1",
    )
    .bind(project_id)
    .fetch_one(db)
    .await?;
    let segments: Vec<(Uuid, Value)> = sqlx::query_as("select id, targeting from segments where project_id = $1 order by position, created_at")
        .bind(project_id)
        .fetch_all(db)
        .await?;
    let mut any_campaign = Campaign::default();
    let mut campaigns = HashMap::new();
    // A lead in no campaign fits if any campaign would take it, so a campaign open to every country or size opens it up.
    let (mut any_country, mut any_size) = (false, false);
    for (id, v) in segments {
        let mut industries = list(&v, "industries");
        extend_unique(&mut industries, &list(&v, "keywords"));
        let c = Campaign { countries: list(&v, "countries"), sizes: list(&v, "company_sizes"), industries, roles: list(&v, "roles") };
        any_country |= c.countries.is_empty();
        any_size |= c.sizes.is_empty();
        extend_unique(&mut any_campaign.countries, &c.countries);
        extend_unique(&mut any_campaign.sizes, &c.sizes);
        extend_unique(&mut any_campaign.industries, &c.industries);
        extend_unique(&mut any_campaign.roles, &c.roles);
        campaigns.insert(id, c);
    }
    if any_country {
        any_campaign.countries.clear();
    }
    if any_size {
        any_campaign.sizes.clear();
    }
    let mut uses = vec![];
    let mut competitors = vec![];
    for pick in crate::tech::picks(db, project_id).await? {
        if pick.kind == "competitor" { competitors.push(pick.tech) } else { uses.push(pick.tech) }
    }
    let off = signals.as_object().into_iter().flatten().filter(|(_, on)| on.as_bool() == Some(false)).map(|(k, _)| k.clone()).collect();
    Ok(Targeting {
        countries: list(&settings, "countries"),
        exclude_countries: list(&settings, "exclude_countries"),
        sizes: list(&settings, "company_sizes"),
        campaigns,
        any_campaign,
        uses,
        competitors,
        off,
        tz: timezone.parse().unwrap_or(chrono_tz::UTC),
        now,
    })
}

#[derive(FromRow)]
struct CompanyRow {
    id: Uuid,
    domain: String,
    segment_id: Option<Uuid>,
    country_code: String,
    employees: String,
    text: String,
}

#[derive(FromRow)]
struct LeadRow {
    id: Uuid,
    company_id: Option<Uuid>,
    domain: Option<String>,
    segment_id: Option<Uuid>,
    title: String,
    country_code: String,
    company_name: String,
    question_at: Option<Ts>,
    deal_stage: Option<String>,
    deal_at: Option<Ts>,
}

#[derive(FromRow)]
struct DomainRow {
    domain: String,
    techs: Option<Vec<String>>,
    roles: Option<Vec<String>>,
    posted: Option<Vec<Ts>>,
    views: Option<i64>,
    last_view: Option<Ts>,
    intent_path: Option<String>,
    intent_at: Option<Ts>,
}

/// Domain signals of the project's companies and leads, or of `only` these domains. One row per domain with any of them.
async fn domain_signals(db: &PgPool, project_id: Uuid, only: Option<&[String]>) -> sqlx::Result<HashMap<String, DomainRow>> {
    let rows: Vec<DomainRow> = sqlx::query_as(
        "with domains as (
           select lower(domain::text) as domain from companies where project_id = $1 and $2::text[] is null
           union select lower(company_domain::text) from leads where project_id = $1 and company_domain <> '' and $2::text[] is null
           union select lower(d) from unnest($2::text[]) as d
         ), hiring as (
           select lower(domain::text) as domain, array_agg(role order by coalesce(posted_at, first_seen) desc) as roles,
                  array_agg(coalesce(posted_at, first_seen) order by coalesce(posted_at, first_seen) desc) as posted
           from job_openings where project_id = $1 and closed_at is null and coalesce(posted_at, first_seen) > now() - make_interval(days => $3)
             and ($2::text[] is null or lower(domain::text) = any($2))
           group by 1
         ), views as (
           select lower(domain::text) as domain, count(*) as views, max(created_at) as last_view from visitor_hits
           where project_id = $1 and domain is not null and created_at > now() - make_interval(days => $4)
             and ($2::text[] is null or lower(domain::text) = any($2))
           group by 1
         ), intent as (
           select distinct on (lower(h.domain::text)) lower(h.domain::text) as domain, h.path, h.created_at
           from visitor_hits h join visitor_sites s on s.project_id = h.project_id, unnest(s.intent_paths) as i(path)
           where h.project_id = $1 and h.domain is not null and h.created_at > now() - make_interval(days => $5)
             and (h.path = i.path or starts_with(h.path, i.path || '/')) and ($2::text[] is null or lower(h.domain::text) = any($2))
           order by lower(h.domain::text), h.created_at desc
         )
         select d.domain, t.techs, h.roles, h.posted, v.views, v.last_view, i.path as intent_path, i.created_at as intent_at
         from domains d
         left join tech_scans t on t.domain = d.domain::citext and t.scanned_at is not null
         left join hiring h on h.domain = d.domain
         left join views v on v.domain = d.domain
         left join intent i on i.domain = d.domain
         where t.domain is not null or h.domain is not null or v.domain is not null or i.domain is not null",
    )
    .bind(project_id)
    .bind(only)
    .bind(HIRING.window)
    .bind(VISIT.window)
    .bind(INTENT_PAGE.window)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|r| (r.domain.clone(), r)).collect())
}

fn with_signals(mut facts: CompanyFacts, signals: Option<&DomainRow>) -> CompanyFacts {
    if let Some(s) = signals {
        facts.techs = s.techs.clone();
        facts.hiring = s.roles.iter().flatten().cloned().zip(s.posted.iter().flatten().copied()).collect();
        facts.views = s.views.unwrap_or(0);
        facts.last_view = s.last_view;
        facts.intent_page = s.intent_path.clone().zip(s.intent_at);
    }
    facts
}

const LEADS: &str = "select l.id, l.company_id, lower(coalesce(nullif(l.company_domain::text, ''), c.domain::text)) as domain, l.segment_id,
       l.title, l.country_code, l.company_name, q.at as question_at, d.stage as deal_stage, d.at as deal_at
     from leads l
     left join companies c on c.id = l.company_id
     left join (select lead_id, max(created_at) as at from email_messages
                where project_id = $1 and status = 'received' and classification = 'question' and created_at > now() - make_interval(days => $3)
                  and ($2::uuid is null or lead_id = $2)
                group by lead_id) q on q.lead_id = l.id
     left join (select d.lead_id, s.name as stage, d.created_at as at from deals d join deal_stages s on s.id = d.stage_id
                where d.project_id = $1 and d.status = 'open' and ($2::uuid is null or d.lead_id = $2)) d on d.lead_id = l.id
     where l.project_id = $1 and ($2::uuid is null or l.id = $2)";

const COMPANIES: &str = "select c.id, lower(c.domain::text) as domain, c.segment_id, c.country_code, c.employees,
       lower(concat_ws(' ', c.name, c.description, replace(d.category, '_', ' '))) as text
     from companies c left join directory_companies d on d.domain = lower(c.domain::text)
     where c.project_id = $1 and ($2::uuid is null or c.id = $2)";

/// Scores the lead from what its company row says, or from the lead itself when it has none.
fn lead_score(t: &Targeting, l: &LeadRow, companies: &HashMap<Uuid, CompanyFacts>, signals: &HashMap<String, DomainRow>) -> Score {
    let mut facts = match l.company_id.and_then(|id| companies.get(&id)) {
        Some(c) => c.clone(),
        None => with_signals(CompanyFacts::default(), l.domain.as_ref().and_then(|d| signals.get(d))),
    };
    if facts.country_code.is_empty() {
        facts.country_code = l.country_code.clone();
    }
    if !l.company_name.is_empty() {
        facts.text = format!("{} {}", facts.text, l.company_name.to_lowercase());
    }
    let lead = LeadFacts { segment_id: l.segment_id, title: l.title.clone(), question_at: l.question_at, deal: l.deal_stage.clone().zip(l.deal_at) };
    score(t, &facts, Some(&lead))
}

/// The project's companies, or `only` that one, with their domain signals, as (id, lowercase domain, facts).
async fn companies(
    db: &PgPool,
    project_id: Uuid,
    only: Option<Uuid>,
    signals: &HashMap<String, DomainRow>,
) -> sqlx::Result<Vec<(Uuid, String, CompanyFacts)>> {
    let rows: Vec<CompanyRow> = sqlx::query_as(COMPANIES).bind(project_id).bind(only).fetch_all(db).await?;
    Ok(rows
        .into_iter()
        .map(|c| {
            let facts =
                CompanyFacts { segment_id: c.segment_id, country_code: c.country_code, employees: c.employees, text: c.text, ..Default::default() };
            let facts = with_signals(facts, signals.get(&c.domain));
            (c.id, c.domain, facts)
        })
        .collect())
}

/// Scores every lead and company in the project, and marks it scored as of when it started. Returns the leads scored.
pub async fn recompute(db: &PgPool, project_id: Uuid) -> anyhow::Result<usize> {
    let t = targeting(db, project_id).await?;
    let signals = domain_signals(db, project_id, None).await?;
    let rows = companies(db, project_id, None, &signals).await?;
    let domains: Vec<(Uuid, String)> = rows.iter().map(|(id, domain, _)| (*id, domain.clone())).collect();
    let companies: HashMap<Uuid, CompanyFacts> = rows.into_iter().map(|(id, _, facts)| (id, facts)).collect();
    let leads: Vec<LeadRow> = sqlx::query_as(LEADS).bind(project_id).bind(None::<Uuid>).bind(QUESTION.window).fetch_all(db).await?;
    let scored: Vec<(Uuid, Score)> = leads.iter().map(|l| (l.id, lead_score(&t, l, &companies, &signals))).collect();
    write(db, "leads", &scored).await?;

    let with_leads: HashSet<Uuid> = leads.iter().filter_map(|l| l.company_id).collect();
    let lead_domains: HashSet<&str> = leads.iter().filter_map(|l| l.domain.as_deref()).collect();
    let own: Vec<(Uuid, Score)> = domains
        .iter()
        .filter(|(id, domain)| !with_leads.contains(id) && !lead_domains.contains(domain.as_str()))
        .filter_map(|(id, _)| Some((*id, score(&t, companies.get(id)?, None))))
        .collect();
    write(db, "companies", &own).await?;
    best_leads(db, project_id, None).await?;
    sqlx::query("update project_settings set scored_at = $2 where project_id = $1").bind(project_id).bind(t.now).execute(db).await?;
    Ok(scored.len())
}

/// Scores one lead now, and its company from its best lead. None when the lead is gone.
pub async fn refresh_lead(db: &PgPool, lead_id: Uuid) -> anyhow::Result<Option<Score>> {
    let Some(project_id) = sqlx::query_scalar::<_, Uuid>("select project_id from leads where id = $1").bind(lead_id).fetch_optional(db).await? else {
        return Ok(None);
    };
    let t = targeting(db, project_id).await?;
    let Some(lead) = sqlx::query_as::<_, LeadRow>(LEADS).bind(project_id).bind(lead_id).bind(QUESTION.window).fetch_optional(db).await? else {
        return Ok(None);
    };
    let signals = match &lead.domain {
        Some(domain) => domain_signals(db, project_id, Some(std::slice::from_ref(domain))).await?,
        None => HashMap::new(),
    };
    let companies: HashMap<Uuid, CompanyFacts> = match lead.company_id {
        Some(id) => self::companies(db, project_id, Some(id), &signals).await?.into_iter().map(|(id, _, facts)| (id, facts)).collect(),
        None => HashMap::new(),
    };
    let score = lead_score(&t, &lead, &companies, &signals);
    write(db, "leads", &[(lead_id, score.clone())]).await?;
    best_leads(db, project_id, Some(lead_id)).await?;
    Ok(Some(score))
}

/// Scores companies that aren't the project's yet, like company database listings, from their own facts plus the
/// project's signals for their domains (site visits, mostly). Keyed by the domain, which must be lowercase.
pub async fn rank(db: &PgPool, project_id: Uuid, companies: Vec<(String, CompanyFacts)>) -> sqlx::Result<Vec<(String, Score)>> {
    let t = targeting(db, project_id).await?;
    let domains: Vec<String> = companies.iter().map(|(d, _)| d.clone()).collect();
    let signals = domain_signals(db, project_id, Some(&domains)).await?;
    Ok(companies
        .into_iter()
        .map(|(domain, mut facts)| {
            let techs = facts.techs.take();
            let mut facts = with_signals(facts, signals.get(&domain));
            facts.techs = facts.techs.or(techs);
            let s = score(&t, &facts, None);
            (domain, s)
        })
        .collect())
}

/// Writes scores in batches, touching only rows whose score changed, so an hourly pass over a quiet project writes nothing.
async fn write(db: &PgPool, table: &str, scores: &[(Uuid, Score)]) -> sqlx::Result<()> {
    let sql = format!(
        "update {table} t set score = u.score, score_fit = u.fit, score_intent = u.intent, score_reasons = u.reasons::jsonb, scored_at = now()
         from unnest($1::uuid[], $2::int2[], $3::int2[], $4::int2[], $5::text[]) as u(id, score, fit, intent, reasons)
         where t.id = u.id and (t.scored_at is null or (t.score, t.score_fit, t.score_intent, t.score_reasons)
                                 is distinct from (u.score, u.fit, u.intent, u.reasons::jsonb))"
    );
    for chunk in scores.chunks(WRITE_BATCH) {
        let reasons: Vec<String> = chunk.iter().map(|(_, s)| serde_json::to_string(&s.reasons).expect("reasons serialize")).collect();
        sqlx::query(&sql)
            .bind(chunk.iter().map(|(id, _)| *id).collect::<Vec<_>>())
            .bind(chunk.iter().map(|(_, s)| s.score).collect::<Vec<_>>())
            .bind(chunk.iter().map(|(_, s)| s.fit).collect::<Vec<_>>())
            .bind(chunk.iter().map(|(_, s)| s.intent).collect::<Vec<_>>())
            .bind(reasons)
            .execute(db)
            .await?;
    }
    Ok(())
}

/// Gives each company with leads its best lead's score, the most intent first on a tie. With `lead`, only that lead's company.
async fn best_leads(db: &PgPool, project_id: Uuid, lead: Option<Uuid>) -> sqlx::Result<()> {
    sqlx::query(
        "with mine as (
           select c.id, c.domain from companies c where c.project_id = $1
             and ($2::uuid is null or c.id = (select company_id from leads where id = $2)
                  or c.domain = (select company_domain from leads where id = $2 and company_id is null))
         ), linked as (
           select m.id, l.score, l.score_fit, l.score_intent, l.score_reasons, l.created_at from mine m join leads l on l.company_id = m.id
           union all
           select m.id, l.score, l.score_fit, l.score_intent, l.score_reasons, l.created_at
           from mine m join leads l on l.project_id = $1 and l.company_id is null and l.company_domain = m.domain
         ), best as (
           select distinct on (id) * from linked order by id, score desc, score_intent desc, created_at
         )
         update companies c set score = b.score, score_fit = b.score_fit, score_intent = b.score_intent, score_reasons = b.score_reasons, scored_at = now()
         from best b
         where c.id = b.id and (c.scored_at is null or (c.score, c.score_fit, c.score_intent, c.score_reasons)
                                 is distinct from (b.score, b.score_fit, b.score_intent, b.score_reasons))",
    )
    .bind(project_id)
    .bind(lead)
    .execute(db)
    .await?;
    Ok(())
}

/// The reasons the email writer may open with: the two strongest that speak for the lead, which the user sees on the lead too.
pub fn opener_reasons(reasons: &[Reason]) -> Vec<String> {
    reasons.iter().filter(|r| r.kind != "against").take(2).map(|r| r.text.clone()).collect()
}

#[cfg(test)]
mod tests {
    use chrono::{Duration as Days, TimeZone, Utc};
    use serde_json::json;

    use super::*;

    fn now() -> Ts {
        Utc.with_ymd_and_hms(2026, 11, 11, 15, 0, 0).unwrap()
    }

    fn ago(days: i64) -> Ts {
        now() - Days::days(days)
    }

    fn targeting() -> Targeting {
        Targeting {
            countries: vec!["GB".into()],
            exclude_countries: vec![],
            sizes: vec![],
            campaigns: HashMap::new(),
            any_campaign: Campaign {
                countries: vec![],
                sizes: vec!["11-50".into()],
                industries: vec!["dental clinics".into()],
                roles: vec!["Head of Sales".into()],
            },
            uses: vec!["HubSpot".into()],
            competitors: vec!["Outreach".into()],
            off: HashSet::new(),
            tz: chrono_tz::UTC,
            now: now(),
        }
    }

    fn company() -> CompanyFacts {
        CompanyFacts {
            country_code: "gb".into(),
            employees: "11-50".into(),
            text: "smile dental clinic in leeds".into(),
            techs: Some(vec!["HubSpot".into()]),
            ..Default::default()
        }
    }

    fn texts(s: &Score) -> Vec<&str> {
        s.reasons.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn fit_is_the_share_of_set_up_signals_a_company_meets() {
        let t = targeting();
        let lead = LeadFacts { title: "Head of Sales, UK".into(), ..Default::default() };
        let full = score(&t, &company(), Some(&lead));
        assert_eq!((full.fit, full.intent, full.score), (50, 0, 50));
        assert_eq!(texts(&full)[..2], ["Matches your industry: dental clinics", "Title matches your buyer: Head of Sales"]);

        // Wrong country and size: 20 of the 70 possible points are gone.
        let mut away = company();
        away.country_code = "FR".into();
        away.employees = "501-1000".into();
        assert_eq!(score(&t, &away, Some(&lead)).fit, (50.0 * 50.0 / 70.0_f64).round() as i16);

        // Without a lead the title doesn't count, so the company still meets everything that does.
        assert_eq!(score(&t, &company(), None).fit, 50);

        // A competitor costs its points and says so, last.
        let mut rival = company();
        rival.techs = Some(vec!["HubSpot".into(), "Outreach".into()]);
        let s = score(&t, &rival, Some(&lead));
        assert_eq!(s.fit, (50.0 * 60.0 / 70.0_f64).round() as i16);
        assert_eq!(s.reasons.last().unwrap(), &Reason { text: "Uses a competitor: Outreach".into(), kind: "against".into() });
        assert_eq!(s.reasons.len(), MAX_REASONS);

        // An unscanned site doesn't count against it: the tech signals are left out of what's possible.
        let mut unscanned = company();
        unscanned.techs = None;
        assert_eq!(score(&t, &unscanned, Some(&lead)).fit, 50);

        // Nothing set up means no fit at all, not a perfect one.
        let bare = Targeting { countries: vec![], uses: vec![], competitors: vec![], any_campaign: Campaign::default(), ..targeting() };
        assert_eq!(score(&bare, &company(), Some(&lead)).fit, 0);
    }

    #[test]
    fn keywords_match_whole_words_singular_or_plural() {
        let text = words("Smile Dental-Clinic (Leeds), IT support");
        assert!(mentions(&text, "dental clinics") && mentions(&text, "Clinic") && mentions(&text, "it"));
        assert!(!mentions(&words("Dentistry with care"), "it") && !mentions(&text, "dent") && !mentions(&text, " "));
    }

    #[test]
    fn titles_match_a_role_or_its_seniority() {
        let roles = ["VP Sales".to_string(), "Owner".to_string()];
        assert_eq!(persona("Senior VP Sales EMEA", &roles), Some("VP Sales"));
        assert_eq!(persona("Vice President, Revenue", &roles), Some("VP Sales"));
        assert_eq!(persona("Co-founder & CEO", &roles), Some("Owner"));
        assert_eq!(persona("Sales Manager", &roles), None);
        assert_eq!(persona("", &roles), None);
    }

    #[test]
    fn each_intent_signal_adds_points_that_fade_with_time() {
        let t = Targeting { countries: vec![], uses: vec![], competitors: vec![], any_campaign: Campaign::default(), ..targeting() };
        let base = CompanyFacts::default();
        let intent = |c: &CompanyFacts, l: &LeadFacts| score(&t, c, Some(l)).intent;
        let lead = LeadFacts::default();

        let hiring = |days| CompanyFacts { hiring: vec![("SDR".into(), ago(days))], ..base.clone() };
        assert_eq!(intent(&hiring(0), &lead), 20);
        assert_eq!(intent(&hiring(10), &lead), 10, "half after its 10-day half-life");
        assert_eq!(intent(&hiring(31), &lead), 0, "gone after 30 days");

        let visit = CompanyFacts { views: 1, last_view: Some(ago(3)), ..base.clone() };
        assert_eq!(intent(&visit, &lead), 5);
        let page = CompanyFacts { intent_page: Some(("/pricing".into(), ago(0))), ..base.clone() };
        assert_eq!(intent(&page, &lead), 15);
        assert_eq!(intent(&CompanyFacts { intent_page: Some(("/pricing".into(), ago(15))), ..base.clone() }, &lead), 0);

        let question = LeadFacts { question_at: Some(ago(7)), ..Default::default() };
        assert_eq!(intent(&base, &question), 10);
        let deal = LeadFacts { deal: Some(("Proposal".into(), ago(0))), ..Default::default() };
        assert_eq!(intent(&base, &deal), 25);
        // A company on its own has no replies or deals.
        assert_eq!(score(&t, &base, None).intent, 0);

        // Together they stop at 50.
        let all = CompanyFacts { views: 4, last_view: Some(ago(0)), ..page.clone() };
        let busy = LeadFacts { question_at: Some(ago(0)), deal: Some(("Proposal".into(), ago(0))), ..Default::default() };
        let s = score(&t, &CompanyFacts { hiring: vec![("SDR".into(), ago(0))], ..all }, Some(&busy));
        assert_eq!((s.intent, s.score), (50, 50));
    }

    #[test]
    fn a_signal_turned_off_counts_for_nothing() {
        let mut t = targeting();
        let lead = LeadFacts { title: "Intern".into(), deal: Some(("Proposal".into(), ago(0))), ..Default::default() };
        let mut c = company();
        c.techs = Some(vec!["Outreach".into()]);
        let on = score(&t, &c, Some(&lead));
        t.off = ["persona", "deal", "competitor", "tech"].map(String::from).into();
        let off = score(&t, &c, Some(&lead));
        // The title and both tools no longer count toward what's possible, so the rest is a full fit.
        assert!(on.fit < 50 && on.intent == 25, "{on:?}");
        assert_eq!((off.fit, off.intent), (50, 0));
        assert!(!texts(&off).iter().any(|r| r.contains("deal") || r.contains("competitor")), "{off:?}");
    }

    #[test]
    fn reasons_read_plainly_and_strongest_first() {
        let t = Targeting { tz: "Europe/London".parse().unwrap(), ..targeting() };
        let c = CompanyFacts {
            hiring: vec![("SDR".into(), ago(1)), ("SDR".into(), ago(2)), ("Account Executive".into(), ago(3))],
            views: 6,
            last_view: Some(ago(1)),
            intent_page: Some(("/pricing".into(), ago(1))),
            techs: Some(vec!["Outreach".into()]),
            ..company()
        };
        let lead = LeadFacts { question_at: Some(ago(2)), ..Default::default() };
        let s = score(&t, &c, Some(&lead));
        assert_eq!(
            texts(&s),
            ["Hiring 2 SDRs and 1 more role", "Replied with a question 2 days ago", "Read /pricing yesterday", "Uses a competitor: Outreach"]
        );
        assert_eq!(opener_reasons(&s.reasons), ["Hiring 2 SDRs and 1 more role", "Replied with a question 2 days ago"]);

        assert_eq!(hiring_text(&[("Account Executive".into(), ago(0))]), "Hiring an Account Executive");
        assert_eq!(hiring_text(&[("SDR".into(), ago(0))]), "Hiring an SDR");
        assert_eq!(hiring_text(&[("Head of Sales".into(), ago(0)), ("head of sales".into(), ago(1)), ("BDR".into(), ago(1)), ("AE".into(), ago(1))]),
            "Hiring 2 Heads of Sales and 2 more roles");
        assert_eq!(plural("Sales Development Representative"), "Sales Development Representatives");
        assert_eq!(plural("Business Development Reps"), "Business Development Reps");
        let visit = CompanyFacts { views: 1, last_view: Some(ago(0)), ..Default::default() };
        assert_eq!(texts(&score(&t, &visit, None)), ["Visited your site today"]);
    }

    async fn project(db: &PgPool) -> Uuid {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let p: Uuid = sqlx::query_scalar("insert into projects (org_id) values ($1) returning id").bind(org).fetch_one(db).await.unwrap();
        sqlx::query("insert into project_settings (project_id, targeting) values ($1, $2)")
            .bind(p)
            .bind(json!({"countries": ["GB"], "hiring_roles": ["SDR"]}))
            .execute(db)
            .await
            .unwrap();
        p
    }

    async fn lead_score_of(db: &PgPool, lead: Uuid) -> (i16, i16, i16, Value) {
        sqlx::query_as("select score, score_fit, score_intent, score_reasons from leads where id = $1").bind(lead).fetch_one(db).await.unwrap()
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_lead_in_no_campaign_takes_any_campaigns_countries_and_sizes(db: PgPool) {
        let p = project(&db).await;
        let add = |targeting: &'static str| {
            sqlx::query("insert into segments (project_id, name, targeting) values ($1, 'C', $2::jsonb)").bind(p).bind(targeting).execute(&db)
        };
        add(r#"{"countries": ["GB"], "company_sizes": ["11-50"]}"#).await.unwrap();
        add(r#"{"countries": ["IE"], "company_sizes": ["11-50"]}"#).await.unwrap();
        let t = super::targeting(&db, p).await.unwrap();
        assert_eq!((t.any_campaign.countries, t.any_campaign.sizes), (vec!["GB".to_string(), "IE".into()], vec!["11-50".to_string()]));

        add(r#"{"countries": ["FR"]}"#).await.unwrap();
        let t = super::targeting(&db, p).await.unwrap();
        assert_eq!(t.any_campaign.countries.len(), 3);
        assert!(t.any_campaign.sizes.is_empty(), "a campaign open to every size opens it up");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_project_is_scored_from_its_signals_in_one_pass(db: PgPool) {
        let p = project(&db).await;
        let segment: Uuid = sqlx::query_scalar(
            "insert into segments (project_id, name, targeting) values ($1, 'Clinics', '{\"roles\": [\"Owner\"], \"industries\": [\"dental\"]}') returning id",
        )
        .bind(p)
        .fetch_one(&db)
        .await
        .unwrap();
        let acme: Uuid = sqlx::query_scalar(
            "insert into companies (project_id, segment_id, domain, name, description, country_code) values ($1, $2, 'acme.co.uk', 'Acme Dental', '', 'GB') returning id",
        )
        .bind(p)
        .bind(segment)
        .fetch_one(&db)
        .await
        .unwrap();
        let quiet: Uuid = sqlx::query_scalar("insert into companies (project_id, domain, name, country_code) values ($1, 'quiet.fr', 'Quiet', 'FR') returning id")
            .bind(p)
            .fetch_one(&db)
            .await
            .unwrap();
        let mut leads = vec![];
        for (name, title) in [("Ana", "Owner"), ("Bo", "Receptionist")] {
            let id: Uuid = sqlx::query_scalar(
                "insert into leads (project_id, segment_id, company_id, full_name, title, email, company_domain)
                 values ($1, $2, $3, $4, $5, $4 || '@acme.co.uk', 'acme.co.uk') returning id",
            )
            .bind(p)
            .bind(segment)
            .bind(acme)
            .bind(name)
            .bind(title)
            .fetch_one(&db)
            .await
            .unwrap();
            leads.push(id);
        }
        sqlx::query(
            "insert into job_openings (project_id, domain, provider, title, role, url, first_seen)
             values ($1, 'acme.co.uk', 'greenhouse', 'SDR', 'SDR', 'https://x/1', now())",
        )
            .bind(p)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into visitor_sites (project_id, intent_paths) values ($1, '{/pricing}')").bind(p).execute(&db).await.unwrap();
        sqlx::query("insert into visitor_hits (project_id, ip_hash, domain, path) values ($1, 'h', 'acme.co.uk', '/pricing/teams'), ($1, 'h', 'quiet.fr', '/blog')")
            .bind(p)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, status, classification, created_at)
             values ($1, $2, 'inbound', 'inbound', 'received', 'question', now() - interval '1 day')",
        )
            .bind(p)
            .bind(leads[0])
            .execute(&db)
            .await
            .unwrap();

        assert_eq!(recompute(&db, p).await.unwrap(), 2);
        let (score, fit, intent, reasons) = lead_score_of(&db, leads[0]).await;
        // Country, industry and title met: a full fit. Hiring 20, a page view 10, /pricing 15, yesterday's question 18: capped at 50.
        assert_eq!((score, fit, intent), (100, 50, 50));
        assert_eq!(reasons[0], json!({"text": "Hiring an SDR", "kind": "intent"}));
        let (score, fit, ..) = lead_score_of(&db, leads[1]).await;
        // The receptionist misses the title (25 of 40 points) and has no question of their own: 20 + 10 + 15 of intent.
        assert_eq!((score, fit), (76, 31));

        // Acme takes its best lead's score. Quiet has no leads, so it gets its own: no country match, one visit.
        let companies: Vec<(Uuid, i16, i16)> =
            sqlx::query_as("select id, score, score_intent from companies where project_id = $1 order by score desc").bind(p).fetch_all(&db).await.unwrap();
        assert_eq!(companies, [(acme, 100, 50), (quiet, 10, 10)]);
        let scored_at: Option<Ts> = sqlx::query_scalar("select scored_at from project_settings where project_id = $1").bind(p).fetch_one(&db).await.unwrap();
        assert!(scored_at.is_some());

        // An unchanged pass writes nothing.
        let before: Vec<Ts> = sqlx::query_scalar("select scored_at from leads where project_id = $1 order by id").bind(p).fetch_all(&db).await.unwrap();
        recompute(&db, p).await.unwrap();
        let after: Vec<Ts> = sqlx::query_scalar("select scored_at from leads where project_id = $1 order by id").bind(p).fetch_all(&db).await.unwrap();
        assert_eq!(before, after);

        // Turning the hiring signal off takes it out of both the score and the reasons.
        sqlx::query("update project_settings set score_signals = '{\"hiring\": false, \"question\": false}' where project_id = $1").bind(p).execute(&db).await.unwrap();
        recompute(&db, p).await.unwrap();
        let (score, _, intent, reasons) = lead_score_of(&db, leads[0]).await;
        assert_eq!((score, intent), (75, 25));
        assert!(!reasons.to_string().contains("Hiring"), "{reasons}");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn projects_are_scored_at_most_every_10_minutes_and_when_something_changed(db: PgPool) {
        let p = project(&db).await;
        // A project without leads or companies is never due.
        project(&db).await;
        let demo: Uuid = sqlx::query_scalar("insert into projects default values returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(demo).execute(&db).await.unwrap();
        for project in [p, demo] {
            sqlx::query("insert into leads (project_id, full_name, email, created_at) values ($1, 'Ana', 'ana@acme.com', now() - interval '2 hours')")
                .bind(project)
                .execute(&db)
                .await
                .unwrap();
        }
        let set_scored = |ago: &'static str| sqlx::query("update project_settings set scored_at = now() - $2::interval where project_id = $1").bind(p).bind(ago).execute(&db);

        // Never scored: due. Projects without leads or companies, and demo projects, never are.
        assert_eq!(due(&db).await.unwrap(), [p]);
        tick(&db).await.unwrap();
        assert!(due(&db).await.unwrap().is_empty());
        let scored: Option<Ts> = sqlx::query_scalar("select scored_at from leads where project_id = $1").bind(p).fetch_one(&db).await.unwrap();
        assert!(scored.is_some());

        // A new hit within 10 minutes waits for the throttle, then makes it due.
        set_scored("5 minutes").await.unwrap();
        sqlx::query("insert into visitor_hits (project_id, ip_hash, domain, path) values ($1, 'h', 'acme.com', '/')").bind(p).execute(&db).await.unwrap();
        assert!(due(&db).await.unwrap().is_empty());
        set_scored("11 minutes").await.unwrap();
        sqlx::query("update visitor_hits set created_at = now() - interval '1 minute'").execute(&db).await.unwrap();
        assert_eq!(due(&db).await.unwrap(), [p]);

        // Nothing new after 11 minutes: not due until the hour is up, so intent still fades.
        sqlx::query("update visitor_hits set created_at = now() - interval '20 minutes'").execute(&db).await.unwrap();
        assert!(due(&db).await.unwrap().is_empty());
        set_scored("61 minutes").await.unwrap();
        assert_eq!(due(&db).await.unwrap(), [p]);

        // A settings change counts as an input.
        set_scored("11 minutes").await.unwrap();
        touch(&db, p).await.unwrap();
        assert_eq!(due(&db).await.unwrap(), [p]);

        // A backend already scoring the project makes this one skip it.
        let mut other = db.begin().await.unwrap();
        sqlx::query("select pg_advisory_xact_lock(hashtextextended('score:' || $1::text, 0))").bind(p).execute(&mut *other).await.unwrap();
        tick(&db).await.unwrap();
        assert_eq!(due(&db).await.unwrap(), [p]);
        other.rollback().await.unwrap();
        tick(&db).await.unwrap();
        assert!(due(&db).await.unwrap().is_empty());
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn one_lead_is_scored_on_demand_with_its_company(db: PgPool) {
        let p = project(&db).await;
        let company: Uuid = sqlx::query_scalar("insert into companies (project_id, domain, country_code) values ($1, 'acme.com', 'GB') returning id")
            .bind(p)
            .fetch_one(&db)
            .await
            .unwrap();
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "insert into leads (project_id, company_id, full_name, email, company_domain)
             values ($1, $2, 'Ana', 'ana@acme.com', 'acme.com'), ($1, $2, 'Bo', 'bo@acme.com', 'acme.com') returning id",
        )
        .bind(p)
        .bind(company)
        .fetch_all(&db)
        .await
        .unwrap();
        let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, position) select org_id, 'Proposal', 0 from projects where id = $1 returning id")
            .bind(p)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into deals (org_id, project_id, lead_id, stage_id) select org_id, $1, $2, $3 from projects where id = $1")
            .bind(p)
            .bind(ids[1])
            .bind(stage)
            .execute(&db)
            .await
            .unwrap();

        let s = refresh_lead(&db, ids[1]).await.unwrap().unwrap();
        assert_eq!((s.fit, s.intent), (50, 25));
        assert!(s.reasons.iter().any(|r| r.text == "Open deal in Proposal"), "{s:?}");
        // Only that lead: the other one and the project's pass are untouched.
        assert_eq!(lead_score_of(&db, ids[0]).await.0, 0);
        let (score, reasons): (i16, Value) = sqlx::query_as("select score, score_reasons from companies where id = $1").bind(company).fetch_one(&db).await.unwrap();
        assert_eq!(score, 75);
        assert!(reasons.to_string().contains("Open deal"));
        let scored_at: Option<Ts> = sqlx::query_scalar("select scored_at from project_settings where project_id = $1").bind(p).fetch_one(&db).await.unwrap();
        assert!(scored_at.is_none());
        assert!(refresh_lead(&db, Uuid::new_v4()).await.unwrap().is_none());
    }
}
