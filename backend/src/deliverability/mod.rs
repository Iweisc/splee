//! The Deliverability page: whether each sending domain and mailbox is landing in trouble, from blocklists, Google
//! Postmaster Tools and Splee's own bounce, opt-out and reply rates. Every hour the worker checks the blocklists and
//! Postmaster of each org it last checked over 6 hours ago, then holds each at-risk mailbox from sending (status paused,
//! why in last_error) and tells the team in chat. Resuming it on the Mailboxes page lets it send through the problems it
//! was held for. A new one holds it again.

pub mod blocklists;
pub mod health;
pub mod postmaster;
pub mod thresholds;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use chrono::{Days, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use self::blocklists::{Kind, LISTS, Resolve};
use self::health::{Listing, PostmasterDay, Rates, Reason};
use self::thresholds::Level;
use crate::hooks::{self, ChatAlert};
use crate::models::Ts;
use crate::state::AppState;

/// Blocklists change slowly and ask the sender to go easy on queries, and Postmaster Tools updates once a day.
const CHECK_EVERY_HOURS: i32 = 6;
/// A sending address seen in a Received header longer ago than this may have changed, so the SMTP host's is used instead.
const RECEIVED_IP_DAYS: i32 = 30;
/// The chart's days, each the rate over the week ending that day, which a day with few sends can't swing.
const CHART_DAYS: u64 = 30;
const ROLLING_DAYS: u64 = 7;

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(3600), || tick(&state), "deliverability").await });
}

/// The domains an org sends from: its bought sending domains and its mailboxes' domains.
pub async fn org_domains(db: &PgPool, org_id: Uuid) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "select domain from sending_domains where org_id = $1 and status = 'registered'
         union select lower(split_part(email::text, '@', 2)) from mailboxes where org_id = $1
         order by 1",
    )
    .bind(org_id)
    .fetch_all(db)
    .await
}

/// Each org with something to check, whether its blocklists and Postmaster are due, checked, and judged. One org's
/// failure is logged and the rest go on.
async fn tick(state: &AppState) -> anyhow::Result<()> {
    let orgs: Vec<(Uuid, bool, bool)> = sqlx::query_as(
        "select o.org_id,
                coalesce((select max(c.checked_at) from blocklist_checks c where c.org_id = o.org_id) < now() - make_interval(hours => $1), true),
                exists (select 1 from postmaster_connections p where p.org_id = o.org_id and p.refresh_token_enc <> ''
                          and (p.synced_at is null or p.synced_at < now() - make_interval(hours => $1)))
           from (select org_id from mailboxes union select org_id from sending_domains where status = 'registered') o",
    )
    .bind(CHECK_EVERY_HOURS)
    .fetch_all(&state.db)
    .await?;
    let resolver = crate::mail::dns::resolver()?;
    for (org_id, blocklists_due, postmaster_due) in orgs {
        if blocklists_due && let Err(e) = check_blocklists(&state.db, &resolver, org_id).await {
            tracing::warn!(error = ?e, %org_id, "blocklist check failed");
        }
        if postmaster_due && let Err(e) = postmaster::sync(state, org_id).await {
            tracing::warn!(error = %e, %org_id, "Postmaster Tools sync failed");
        }
        if let Err(e) = evaluate(state, org_id).await {
            tracing::error!(error = ?e, %org_id, "judging mailbox health failed");
        }
    }
    Ok(())
}

/// Learns each mailbox's sending addresses, then asks every list about the org's domains and addresses and keeps the
/// answers. Rows for a domain or address the org no longer sends from are dropped.
pub async fn check_blocklists(db: &PgPool, resolver: &impl Resolve, org_id: Uuid) -> anyhow::Result<()> {
    let mailboxes: Vec<(Uuid, String, String, bool)> = sqlx::query_as(
        "select id, smtp_host, received_ip, coalesce(received_ip_at > now() - make_interval(days => $2), false) from mailboxes where org_id = $1",
    )
    .bind(org_id)
    .bind(RECEIVED_IP_DAYS)
    .fetch_all(db)
    .await?;
    let mut ips: Vec<String> = vec![];
    for (id, smtp_host, received_ip, recent) in mailboxes {
        let found: Vec<String> = if recent && !received_ip.is_empty() {
            vec![received_ip]
        } else {
            blocklists::public_ipv4(resolver, &smtp_host).await.iter().map(ToString::to_string).collect()
        };
        sqlx::query("update mailboxes set sending_ips = $2 where id = $1").bind(id).bind(&found).execute(db).await?;
        ips.extend(found);
    }
    ips.sort();
    ips.dedup();
    let domains = org_domains(db, org_id).await?;
    let targets: Vec<(&str, Kind)> = domains.iter().map(|d| (d.as_str(), Kind::Domain)).chain(ips.iter().map(|ip| (ip.as_str(), Kind::Ip))).collect();
    let asks = targets.iter().flat_map(|&(target, kind)| LISTS.iter().filter(move |l| l.kind == kind).map(move |l| (target, kind, l)));
    let answers = futures::future::join_all(asks.map(|(target, kind, list)| async move { (target, kind, list, blocklists::check(resolver, list, target).await) })).await;
    for (target, kind, list, outcome) in answers {
        sqlx::query(
            "insert into blocklist_checks (org_id, target, kind, list, status, detail) values ($1, $2, $3, $4, $5, $6)
             on conflict (org_id, target, list) do update set kind = excluded.kind, status = excluded.status, detail = excluded.detail, checked_at = now()",
        )
        .bind(org_id)
        .bind(target)
        .bind(kind.name())
        .bind(list.id)
        .bind(outcome.status())
        .bind(outcome.detail())
        .execute(db)
        .await?;
    }
    let kept: Vec<&str> = targets.iter().map(|(t, _)| *t).collect();
    sqlx::query("delete from blocklist_checks where org_id = $1 and target <> all($2)").bind(org_id).bind(&kept).execute(db).await?;
    Ok(())
}

/// Keeps the sending server's address another Splee mailbox saw on a warmup email from `mailbox_id`.
pub async fn note_received_ip(db: &PgPool, mailbox_id: Uuid, raw: &[u8]) -> sqlx::Result<()> {
    let Some(ip) = blocklists::received_ip(raw) else { return Ok(()) };
    sqlx::query("update mailboxes set received_ip = $2, received_ip_at = now() where id = $1").bind(mailbox_id).bind(ip.to_string()).execute(db).await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct MailboxRow {
    id: Uuid,
    email: String,
    domain: String,
    status: String,
    last_error: String,
    health_held_at: Option<Ts>,
    health_ack: Vec<String>,
    sending_ips: Vec<String>,
    from_received: bool,
    warmup_spam: bool,
}

#[derive(Clone, sqlx::FromRow)]
struct CheckRow {
    target: String,
    kind: String,
    list: String,
    status: String,
    detail: String,
    checked_at: Ts,
}

#[derive(Serialize)]
pub struct ListingView {
    pub list: &'static str,
    pub label: &'static str,
    pub kind: String,
    pub target: String,
    /// clear, listed or unknown, when Splee couldn't ask.
    pub status: String,
    pub detail: String,
    pub delist_url: String,
    pub checked_at: Ts,
}

#[derive(Serialize)]
pub struct MailboxReport {
    pub id: Uuid,
    pub email: String,
    pub domain: String,
    pub status: String,
    /// Splee holds it, and last_error says why.
    pub held: bool,
    pub last_error: String,
    pub verdict: Level,
    /// Its own problems and its domain's.
    pub reasons: Vec<Reason>,
    pub week: Rates,
    pub month: Rates,
    pub ips: Vec<String>,
    /// "received" when the addresses come from a Received header, else "smtp_host".
    pub ip_source: &'static str,
    #[serde(skip)]
    ack: Vec<String>,
}

#[derive(Serialize)]
pub struct DomainReport {
    pub domain: String,
    /// The worst of its own problems and its mailboxes'.
    pub verdict: Level,
    pub reasons: Vec<Reason>,
    /// Its own lists and its mailboxes' sending addresses'.
    pub listings: Vec<ListingView>,
    /// Its latest day in Postmaster Tools.
    pub postmaster: Option<PostmasterDay>,
    pub mailboxes: usize,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Point {
    pub date: NaiveDate,
    pub bounce_rate: Option<f64>,
    pub opt_out_rate: Option<f64>,
    /// Gmail's, from Postmaster Tools. Across domains, the highest that day.
    pub spam_rate: Option<f64>,
}

#[derive(Serialize)]
pub struct Series {
    /// Empty for all domains together.
    pub domain: String,
    pub days: Vec<Point>,
}

#[derive(Serialize)]
pub struct Report {
    pub checked_at: Option<Ts>,
    pub domains: Vec<DomainReport>,
    pub mailboxes: Vec<MailboxReport>,
    pub series: Vec<Series>,
}

/// Leads each of the org's mailboxes emailed in the last `days` days, and how many of those bounced, opted out (a reply
/// asking to stop or an unsubscribe click) or replied. By lead, like the Mailboxes page, so a lead's follow-ups don't
/// count as more people.
async fn rates(db: &PgPool, org_id: Uuid, days: i32) -> sqlx::Result<HashMap<Uuid, Rates>> {
    let rows: Vec<(Uuid, i64, i64, i64, i64)> = sqlx::query_as(
        "select m.id,
           (select count(distinct e.lead_id) from email_messages e
             where e.mailbox_id = m.id and e.status = 'sent' and e.sent_at > now() - make_interval(days => $2)),
           (select count(distinct e.lead_id) from email_messages e
             where e.mailbox_id = m.id and e.direction = 'inbound' and e.classification = 'bounce' and e.created_at > now() - make_interval(days => $2)),
           (select count(*) from (
              select e.lead_id from email_messages e
               where e.mailbox_id = m.id and e.direction = 'inbound' and e.classification = 'unsubscribe' and e.created_at > now() - make_interval(days => $2)
              union
              select e.lead_id from email_messages e join leads l on l.id = e.lead_id
               where e.mailbox_id = m.id and e.status = 'sent' and l.status = 'unsubscribed' and l.status_changed_at > now() - make_interval(days => $2)) o),
           (select count(distinct e.lead_id) from email_messages e
             where e.mailbox_id = m.id and e.direction = 'inbound' and e.classification is distinct from 'bounce' and e.created_at > now() - make_interval(days => $2))
         from mailboxes m where m.org_id = $1",
    )
    .bind(org_id)
    .bind(days)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(id, emailed, bounced, opted_out, replied)| (id, Rates { emailed, bounced, opted_out, replied })).collect())
}

/// Each domain's leads emailed, bounced and opted out per UTC day, from `since` on.
async fn daily(db: &PgPool, org_id: Uuid, since: NaiveDate) -> sqlx::Result<Vec<(String, NaiveDate, Rates)>> {
    let rows: Vec<(String, NaiveDate, i64, i64, i64)> = sqlx::query_as(
        "with mb as (select id, lower(split_part(email::text, '@', 2)) as domain from mailboxes where org_id = $1),
         events as (
           select mb.domain, (e.sent_at at time zone 'UTC')::date as day, e.lead_id, 'sent' as what
             from email_messages e join mb on mb.id = e.mailbox_id where e.status = 'sent' and e.sent_at >= $2::date
           union all
           select mb.domain, (e.created_at at time zone 'UTC')::date, e.lead_id, case when e.classification = 'bounce' then 'bounce' else 'opt_out' end
             from email_messages e join mb on mb.id = e.mailbox_id
            where e.direction = 'inbound' and e.classification in ('bounce', 'unsubscribe') and e.created_at >= $2::date
           union all
           select distinct mb.domain, (l.status_changed_at at time zone 'UTC')::date, l.id, 'opt_out'
             from leads l join email_messages e on e.lead_id = l.id and e.status = 'sent' join mb on mb.id = e.mailbox_id
            where l.status = 'unsubscribed' and l.status_changed_at >= $2::date)
         select domain, day, count(distinct lead_id) filter (where what = 'sent'), count(distinct lead_id) filter (where what = 'bounce'),
                count(distinct lead_id) filter (where what = 'opt_out')
           from events group by domain, day",
    )
    .bind(org_id)
    .bind(since)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(domain, day, emailed, bounced, opted_out)| (domain, day, Rates { emailed, bounced, opted_out, replied: 0 })).collect())
}

/// The chart's days up to `today`, each with the bounce and opt-out rates of the week ending that day and that day's spam rate.
pub fn rolling(counts: &BTreeMap<NaiveDate, Rates>, spam: &BTreeMap<NaiveDate, f64>, today: NaiveDate) -> Vec<Point> {
    (0..CHART_DAYS)
        .rev()
        .map(|ago| {
            let date = today - Days::new(ago);
            let week = counts.range(date - Days::new(ROLLING_DAYS - 1)..=date).fold(Rates::default(), |sum, (_, r)| sum.add(*r));
            Point {
                date,
                bounce_rate: health::rate(week.bounced, week.emailed),
                opt_out_rate: health::rate(week.opted_out, week.emailed),
                spam_rate: spam.get(&date).copied(),
            }
        })
        .collect()
}

fn listing_view(c: &CheckRow) -> Option<ListingView> {
    let list = blocklists::list(&c.list)?;
    Some(ListingView {
        list: list.id,
        label: list.label,
        kind: c.kind.clone(),
        target: c.target.clone(),
        status: c.status.clone(),
        detail: c.detail.clone(),
        delist_url: blocklists::delist_url(list, &c.target),
        checked_at: c.checked_at,
    })
}

fn listed<'a>(checks: &'a [CheckRow], targets: &[&str]) -> Vec<Listing<'a>> {
    checks
        .iter()
        .filter(|c| c.status == "listed" && targets.contains(&c.target.as_str()))
        .filter_map(|c| Some(Listing { list: blocklists::list(&c.list)?, target: &c.target, detail: &c.detail }))
        .collect()
}

/// Everything the page shows, judged as of `today`.
pub async fn report(db: &PgPool, org_id: Uuid, today: NaiveDate) -> sqlx::Result<Report> {
    let mailboxes: Vec<MailboxRow> = sqlx::query_as(&format!(
        "select m.id, m.email::text as email, lower(split_part(m.email::text, '@', 2)) as domain, m.status, m.last_error, m.health_held_at,
                m.health_ack, m.sending_ips, (m.received_ip <> '' and m.received_ip = any(m.sending_ips)) as from_received,
                coalesce(m.warmup and {}, false) as warmup_spam
           from mailboxes m where m.org_id = $1 order by m.created_at, m.email",
        crate::mail::warmup::LANDING_IN_SPAM
    ))
    .bind(org_id)
    .fetch_all(db)
    .await?;
    let (week, month) = (rates(db, org_id, 7).await?, rates(db, org_id, 30).await?);
    let checks: Vec<CheckRow> =
        sqlx::query_as("select target, kind, list, status, detail, checked_at from blocklist_checks where org_id = $1 order by target, list")
            .bind(org_id)
            .fetch_all(db)
            .await?;
    let latest: Vec<PostmasterDay> = sqlx::query_as(
        "select distinct on (domain) domain, day, reputation, spam_rate, spf_rate, dkim_rate, dmarc_rate from postmaster_days
          where org_id = $1 order by domain, day desc",
    )
    .bind(org_id)
    .fetch_all(db)
    .await?;
    let postmaster_of = |domain: &str| latest.iter().find(|d| d.domain == domain);
    let domain_reasons = |domain: &str| health::domain_reasons(&listed(&checks, &[domain]), postmaster_of(domain), today);

    let mailboxes: Vec<MailboxReport> = mailboxes
        .into_iter()
        .map(|m| {
            let (w, mo) = (week.get(&m.id).copied().unwrap_or_default(), month.get(&m.id).copied().unwrap_or_default());
            let ips: Vec<&str> = m.sending_ips.iter().map(String::as_str).collect();
            let mut reasons = domain_reasons(&m.domain);
            reasons.extend(health::mailbox_reasons(&w, &mo, &listed(&checks, &ips), m.warmup_spam));
            MailboxReport {
                verdict: health::verdict(&reasons),
                reasons,
                held: m.health_held_at.is_some() && m.status == "paused",
                week: w,
                month: mo,
                ip_source: if m.from_received { "received" } else { "smtp_host" },
                ips: m.sending_ips,
                ack: m.health_ack,
                id: m.id,
                email: m.email,
                domain: m.domain,
                status: m.status,
                last_error: m.last_error,
            }
        })
        .collect();

    let domains: Vec<DomainReport> = org_domains(db, org_id)
        .await?
        .into_iter()
        .map(|domain| {
            let own: Vec<&MailboxReport> = mailboxes.iter().filter(|m| m.domain == domain).collect();
            let reasons = domain_reasons(&domain);
            let ips: HashSet<&str> = own.iter().flat_map(|m| m.ips.iter().map(String::as_str)).collect();
            let listings = checks.iter().filter(|c| c.target == domain || ips.contains(c.target.as_str())).filter_map(listing_view).collect();
            let verdict = own.iter().map(|m| m.verdict).chain([health::verdict(&reasons)]).max().unwrap_or(Level::Good);
            DomainReport { verdict, reasons, listings, postmaster: postmaster_of(&domain).cloned(), mailboxes: own.len(), domain }
        })
        .collect();

    let since = today - Days::new(CHART_DAYS + ROLLING_DAYS);
    let mut counts: BTreeMap<String, BTreeMap<NaiveDate, Rates>> = BTreeMap::new();
    for (domain, day, r) in daily(db, org_id, since).await? {
        let all = counts.entry(String::new()).or_default().entry(day).or_default();
        *all = all.add(r);
        counts.entry(domain).or_default().insert(day, r);
    }
    let spam_days: Vec<(String, NaiveDate, f64)> =
        sqlx::query_as("select domain, day, spam_rate from postmaster_days where org_id = $1 and day >= $2 and spam_rate is not null")
            .bind(org_id)
            .bind(since)
            .fetch_all(db)
            .await?;
    let mut spam: BTreeMap<String, BTreeMap<NaiveDate, f64>> = BTreeMap::new();
    for (domain, day, rate) in spam_days {
        let all = spam.entry(String::new()).or_default().entry(day).or_insert(rate);
        *all = all.max(rate);
        spam.entry(domain).or_default().insert(day, rate);
    }
    let names = std::iter::once(String::new()).chain(domains.iter().map(|d| d.domain.clone()));
    let empty = BTreeMap::new();
    let series = names
        .map(|domain| {
            let days = rolling(counts.get(&domain).unwrap_or(&empty), spam.get(&domain).unwrap_or(&BTreeMap::new()), today);
            Series { domain, days }
        })
        .collect();

    Ok(Report { checked_at: checks.iter().map(|c| c.checked_at).max(), domains, mailboxes, series })
}

/// Judges the org's mailboxes and holds each active one at risk for a problem it wasn't resumed through, with an alert.
pub async fn evaluate(state: &AppState, org_id: Uuid) -> anyhow::Result<Report> {
    let first = report(&state.db, org_id, Utc::now().date_naive()).await?;
    let mut held_any = false;
    for m in &first.mailboxes {
        let codes = health::at_risk_codes(&m.reasons);
        // A problem that cleared is forgotten, so it holds the mailbox again if it comes back.
        let ack: Vec<String> = m.ack.iter().filter(|c| codes.contains(c)).cloned().collect();
        if ack != m.ack {
            sqlx::query("update mailboxes set health_ack = $2 where id = $1").bind(m.id).bind(&ack).execute(&state.db).await?;
        }
        if m.status != "active" || codes.iter().all(|c| ack.contains(c)) {
            continue;
        }
        let why: Vec<&str> = m.reasons.iter().filter(|r| r.level == Level::AtRisk).map(|r| r.text.as_str()).collect();
        let why = why.join(". ");
        let message = format!("Held by Splee because this mailbox is at risk: {why}. Fix it, then resume the mailbox to send again.");
        let held = sqlx::query("update mailboxes set status = 'paused', last_error = $2, health_held_at = now() where id = $1 and status = 'active'")
            .bind(m.id)
            .bind(&message)
            .execute(&state.db)
            .await?;
        if held.rows_affected() == 1 {
            held_any = true;
            tracing::warn!(mailbox = %m.email, %why, "held an at-risk mailbox");
            announce_hold(state, org_id, &m.email, &why).await;
        }
    }
    // Read again, so the page shows the holds this made.
    if held_any { Ok(report(&state.db, org_id, Utc::now().date_naive()).await?) } else { Ok(first) }
}

/// Tells each of the org's projects' chats that a mailbox stopped sending.
async fn announce_hold(state: &AppState, org_id: Uuid, email: &str, why: &str) {
    let projects: Vec<Uuid> = match sqlx::query_scalar("select id from projects where org_id = $1").bind(org_id).fetch_all(&state.db).await {
        Ok(p) => p,
        Err(e) => return tracing::warn!(error = %e, "couldn't load the projects to alert"),
    };
    let summary = format!("Splee stopped sending from {email}: {why}.");
    let link = format!("{}/app/deliverability", state.cfg.public_url.trim_end_matches('/'));
    let alert = ChatAlert { label: "Mailbox held", summary: &summary, quote: None, link: &link, link_text: "See what to do" };
    for project in projects {
        hooks::post_chat(&state.db, project, &alert).await;
    }
}

/// The at-risk problems a mailbox has now, which resuming it acknowledges.
pub async fn at_risk_now(db: &PgPool, org_id: Uuid, mailbox_id: Uuid) -> sqlx::Result<Vec<String>> {
    let report = report(db, org_id, Utc::now().date_naive()).await?;
    Ok(report.mailboxes.iter().find(|m| m.id == mailbox_id).map(|m| health::at_risk_codes(&m.reasons)).unwrap_or_default())
}

#[cfg(test)]
mod tests;
