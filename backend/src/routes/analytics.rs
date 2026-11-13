//! The Analytics page: what's working across campaigns, mailboxes, sequence steps, send times and channels in a date
//! range, next to the period before it. The v1 API and the MCP tool get_analytics serve the same numbers.
//!
//! Everything counts on the day it happened, in the project's time zone: an email when it was sent, a reply when the lead's
//! first reply arrived, a meeting when the lead was booked. A reply, bounce, meeting, unsubscribe or won deal is credited
//! to the last first email or follow-up the lead got before it, which decides its campaign, mailbox, step and send hour.
//!
//! Open and click rates are by email, out of the emails sent in the range that tracked them, counting opens and clicks
//! whenever they came. Opens are an estimate: some people block the pixel and Apple Mail opens every email itself.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{Datelike, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::bouncing;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/analytics", get(project_analytics))
}

const DEFAULT_DAYS: i64 = 30;
const MAX_DAYS: i64 = 365;
/// Longer ranges are charted by week, so a year stays readable.
const DAILY_UP_TO: i64 = 90;

#[derive(Deserialize, Default)]
pub struct AnalyticsQuery {
    /// First and last day of the range, inclusive, in the project's time zone. The last 30 days by default.
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub segment_id: Option<Uuid>,
    pub mailbox_id: Option<Uuid>,
}

#[derive(Serialize)]
pub struct Analytics {
    pub(crate) from: NaiveDate,
    pub(crate) to: NaiveDate,
    pub(crate) previous_from: NaiveDate,
    pub(crate) previous_to: NaiveDate,
    timezone: String,
    /// The organization's currency, which revenue is counted in.
    currency: String,
    /// Whether the project has a payment provider connected or payments kept from one, so the page shows revenue.
    revenue_tracked: bool,
    /// Payments in the range in other currencies, which revenue leaves out, and those currencies.
    ignored_payments: i64,
    ignored_currencies: Vec<String>,
    /// "day", or "week" for ranges over 90 days. Weeks start on Monday.
    interval: &'static str,
    pub(crate) totals: Totals,
    pub(crate) previous: Totals,
    series: Vec<Point>,
    pub(crate) campaigns: Vec<CampaignRow>,
    /// The A/B test angles of the campaigns, for projects that test any.
    angles: Vec<AngleRow>,
    mailboxes: Vec<MailboxRow>,
    steps: Vec<StepRow>,
    /// Weekday (1 for Monday to 7 for Sunday) and hour of the emails sent, in the project's time zone. Empty cells are left out.
    heatmap: Vec<SlotRow>,
    channels: Vec<ChannelRow>,
    /// By the language of the emails leads got, most leads first. A reply counts for the language of the last email before it.
    languages: Vec<LanguageRow>,
}

/// Rates are by lead, out of the leads emailed in the range, like the overview's. Null when nobody was emailed.
#[derive(Serialize, Default)]
pub struct Totals {
    /// First emails and follow-ups. Replies typed in the inbox only count in email cost.
    pub(crate) sent: i64,
    contacted: i64,
    delivered: i64,
    pub(crate) replies: i64,
    pub(crate) reply_rate: Option<f64>,
    /// Leads who replied and are now Hot or Meeting booked.
    pub(crate) hot: i64,
    hot_rate: Option<f64>,
    pub(crate) meetings: i64,
    pub(crate) deals_won: i64,
    /// One entry per currency the won deals are priced in, the organization's own first.
    won_value: Vec<Money>,
    unsubscribes: i64,
    bounced: i64,
    bounce_rate: Option<f64>,
    #[serde(flatten)]
    engagement: Engagement,
    /// What Claude Code reported for the agent runs started in the range. A mailbox filter leaves it out, since runs
    /// don't belong to a mailbox.
    claude_cost_micros: i64,
    email_cost_micros: i64,
    pub(crate) spend_micros: i64,
    cost_per_hot_micros: Option<i64>,
    cost_per_meeting_micros: Option<i64>,
    /// Payments less refunds kept in the range, in cents of the organization's currency. A mailbox filter leaves them at 0,
    /// since payments don't belong to a mailbox.
    pub(crate) revenue_cents: i64,
    refunded_cents: i64,
    payments: i64,
    paying_leads: i64,
    /// Revenue over meetings booked, null without meetings.
    revenue_per_meeting_cents: Option<i64>,
    /// Revenue over spend, null when nothing was spent. Spend is in US dollars, so it's also null when the organization
    /// counts revenue in another currency.
    pub(crate) return_on_spend: Option<f64>,
}

/// Emails opened and clicked, and the rates out of those that tracked it. A rate is null when none did, so the page leaves
/// out opens for campaigns that don't track them.
#[derive(Serialize, Default, Clone, Copy)]
pub struct Engagement {
    opened: i64,
    open_rate: Option<f64>,
    clicked: i64,
    click_rate: Option<f64>,
}

impl Engagement {
    fn from_counts(n: &Counts) -> Self {
        Self { opened: n.opened, open_rate: rate(n.opened, n.open_tracked), clicked: n.clicked, click_rate: rate(n.clicked, n.click_tracked) }
    }
}

#[derive(Serialize)]
struct Money {
    currency: String,
    value_cents: i64,
}

#[derive(Serialize)]
struct Point {
    /// The day, or the Monday that starts the week.
    date: String,
    sent: i64,
    replies: i64,
    hot: i64,
    meetings: i64,
}

#[derive(Serialize)]
pub(crate) struct CampaignRow {
    /// Null for emails and leads outside any campaign.
    pub(crate) segment_id: Option<Uuid>,
    pub(crate) name: String,
    pub(crate) sent: i64,
    contacted: i64,
    pub(crate) replies: i64,
    reply_rate: Option<f64>,
    pub(crate) hot: i64,
    pub(crate) meetings: i64,
    deals_won: i64,
    bounced: i64,
    bounce_rate: Option<f64>,
    #[serde(flatten)]
    engagement: Engagement,
    spend_micros: i64,
    cost_per_hot_micros: Option<i64>,
    /// From the payments of the campaign's leads.
    revenue_cents: i64,
    return_on_spend: Option<f64>,
}

#[derive(Serialize)]
struct MailboxRow {
    mailbox_id: Option<Uuid>,
    email: String,
    sent: i64,
    contacted: i64,
    replies: i64,
    reply_rate: Option<f64>,
    hot: i64,
    bounced: i64,
    bounce_rate: Option<f64>,
    /// Over 5% of at least 20 leads bounced, where providers start sending the mailbox's mail to spam.
    bouncing: bool,
    #[serde(flatten)]
    engagement: Engagement,
}

/// One angle of a campaign's A/B test. The reply rate is by lead, like the campaign's.
#[derive(Serialize)]
struct AngleRow {
    variant_id: Uuid,
    segment_id: Uuid,
    name: String,
    campaign: String,
    sent: i64,
    contacted: i64,
    replies: i64,
    reply_rate: Option<f64>,
    #[serde(flatten)]
    engagement: Engagement,
}

/// Step 0 is the first email, step n follow-up n. The rate is out of the step's emails, like the Follow-ups page's.
#[derive(Serialize)]
struct StepRow {
    step: i32,
    sent: i64,
    replies: i64,
    reply_rate: Option<f64>,
    #[serde(flatten)]
    engagement: Engagement,
}

#[derive(Serialize)]
struct SlotRow {
    weekday: i32,
    hour: i32,
    sent: i64,
    replies: i64,
}

/// sent is emails, LinkedIn steps, texts, calls made or posts. Null leads or replies are ones Splee doesn't record for the
/// channel: LinkedIn replies only stop the sequence, posts have no lead, and a call is answered rather than replied to. Only
/// calls have connect_rate, the share where someone picked up and talked, and meetings, the ones booked on the call.
#[derive(Serialize)]
struct ChannelRow {
    channel: &'static str,
    sent: i64,
    leads: Option<i64>,
    replies: Option<i64>,
    reply_rate: Option<f64>,
    connect_rate: Option<f64>,
    meetings: Option<i64>,
}

/// The rate is by lead, out of the leads emailed in this language, like the totals'.
#[derive(Serialize)]
struct LanguageRow {
    language: &'static str,
    name: &'static str,
    sent: i64,
    contacted: i64,
    replies: i64,
    reply_rate: Option<f64>,
}

#[derive(FromRow, Default)]
struct Counts {
    sent: i64,
    contacted: i64,
    replies: i64,
    email_replies: i64,
    hot: i64,
    meetings: i64,
    bounced: i64,
    unsubscribes: i64,
    won: i64,
    won_cents: i64,
    email_cost_micros: i64,
    open_tracked: i64,
    opened: i64,
    click_tracked: i64,
    clicked: i64,
}

#[derive(FromRow)]
struct Row {
    breakdown: String,
    /// The range itself, or the period before it.
    cur: bool,
    bucket: Option<String>,
    segment_id: Option<Uuid>,
    variant_id: Option<Uuid>,
    mailbox_id: Option<Uuid>,
    step: Option<i32>,
    weekday: Option<i32>,
    hour: Option<i32>,
    currency: Option<String>,
    language: Option<String>,
    #[sqlx(flatten)]
    n: Counts,
}

/// Both periods in one pass, grouped every way the page breaks them down. $2 starts the previous period, $3 the range,
/// and $4 is the range's last day. A lead's first reply and first bounce are found over all time, so only the first ever
/// counts, in whichever period it arrived.
const FACTS: &str = "with w as (
   select $2::date::timestamp at time zone $5 as lo, $3::date::timestamp at time zone $5 as mid,
     ($4::date + 1)::timestamp at time zone $5 as hi
 ), firsts as (
   select lead_id,
     min(created_at) filter (where classification is distinct from 'bounce') as replied_at,
     (array_agg(channel order by created_at) filter (where classification is distinct from 'bounce'))[1] as reply_channel,
     min(created_at) filter (where classification = 'bounce') as bounced_at
   from email_messages where project_id = $1 and direction = 'inbound' group by lead_id
 ), outcomes as (
   select lead_id, 'reply' as what, replied_at as at, reply_channel as channel, null::text as currency, 0::bigint as cents
   from firsts where replied_at is not null
   union all select lead_id, 'bounce', bounced_at, null, null, 0 from firsts where bounced_at is not null
   union all select id, 'meeting', status_changed_at, null, null, 0 from leads where project_id = $1 and status = 'meeting_booked'
   union all select id, 'unsubscribe', status_changed_at, null, null, 0 from leads where project_id = $1 and status = 'unsubscribed'
   union all select lead_id, 'won', closed_at, null, currency, value_cents from deals where project_id = $1 and status = 'won'
 ), facts as (
   select 'sent' as what, e.sent_at as at, e.lead_id, e.segment_id, e.mailbox_id, e.kind in ('initial', 'follow_up') as outreach,
     e.step, e.sent_at, e.cost_micros, false as hot, null as channel, null as currency, 0::bigint as cents, e.variant_id,
     e.track_opens, e.track_clicks, coalesce(nullif(e.language, ''), 'en') as language,
     exists (select 1 from tracking_hits h where h.email_id = e.id and h.kind = 'open' and h.filter_reason is null) as opened,
     exists (select 1 from tracking_hits h where h.email_id = e.id and h.kind = 'click' and h.filter_reason is null) as clicked
   from email_messages e, w
   where e.project_id = $1 and e.status = 'sent' and e.direction = 'outbound' and e.sent_at >= w.lo and e.sent_at < w.hi
   union all
   select o.what, o.at, o.lead_id, coalesce(c.segment_id, l.segment_id), c.mailbox_id, c.step is not null, c.step, c.sent_at, 0,
     l.status in ('hot', 'meeting_booked'), o.channel, o.currency, o.cents, c.variant_id, false, false, c.language, false, false
   from outcomes o cross join w join leads l on l.id = o.lead_id
   left join lateral (
     select e.segment_id, e.mailbox_id, e.step, e.sent_at, e.variant_id, coalesce(nullif(e.language, ''), 'en') as language from email_messages e
     where e.lead_id = o.lead_id and e.direction = 'outbound' and e.status = 'sent' and e.kind in ('initial', 'follow_up')
       and e.sent_at <= o.at
     order by e.sent_at desc limit 1
   ) c on true
   where o.at >= w.lo and o.at < w.hi
 ), scoped as (
   select f.*, f.at >= w.mid as cur, to_char(date_trunc($8, f.at at time zone $5), 'YYYY-MM-DD') as bucket,
     case when f.outreach then f.step end as out_step,
     extract(isodow from case when f.outreach then f.sent_at at time zone $5 end)::int as weekday,
     extract(hour from case when f.outreach then f.sent_at at time zone $5 end)::int as hour
   from facts f, w
   where ($6::uuid is null or f.segment_id = $6) and ($7::uuid is null or f.mailbox_id = $7)
 )
 select case when grouping(bucket) = 0 then 'series' when grouping(segment_id) = 0 then 'campaign'
          when grouping(mailbox_id) = 0 then 'mailbox' when grouping(out_step) = 0 then 'step'
          when grouping(weekday) = 0 then 'slot' when grouping(currency) = 0 then 'currency' when grouping(variant_id) = 0 then 'angle'
          when grouping(language) = 0 then 'language' else 'total' end as breakdown,
   cur, bucket, segment_id, variant_id, mailbox_id, out_step as step, weekday, hour, currency, language,
   count(*) filter (where what = 'sent' and outreach) as sent,
   count(distinct lead_id) filter (where what = 'sent' and outreach) as contacted,
   count(*) filter (where what = 'reply') as replies,
   count(*) filter (where what = 'reply' and channel = 'email') as email_replies,
   count(*) filter (where what = 'reply' and hot) as hot,
   count(*) filter (where what = 'meeting') as meetings,
   count(*) filter (where what = 'bounce') as bounced,
   count(*) filter (where what = 'unsubscribe') as unsubscribes,
   count(*) filter (where what = 'won') as won,
   coalesce(sum(cents) filter (where what = 'won'), 0)::bigint as won_cents,
   coalesce(sum(cost_micros), 0)::bigint as email_cost_micros,
   count(*) filter (where what = 'sent' and outreach and track_opens) as open_tracked,
   count(*) filter (where what = 'sent' and outreach and track_opens and opened) as opened,
   count(*) filter (where what = 'sent' and outreach and track_clicks) as click_tracked,
   count(*) filter (where what = 'sent' and outreach and track_clicks and clicked) as clicked
 from scoped
 group by grouping sets ((cur), (cur, bucket), (cur, segment_id), (cur, mailbox_id), (cur, out_step), (cur, weekday, hour), (cur, currency),
                         (cur, variant_id), (cur, language))";

fn rate(part: i64, whole: i64) -> Option<f64> {
    (whole > 0).then(|| part as f64 / whole as f64)
}

fn per(spend: i64, n: i64) -> Option<i64> {
    (n > 0).then(|| spend / n)
}

/// Cents over micro-dollars, which only compare when the cents are US ones.
fn return_on(revenue_cents: i64, spend_micros: i64, usd: bool) -> Option<f64> {
    (usd && spend_micros > 0).then(|| revenue_cents as f64 * 10_000.0 / spend_micros as f64)
}

#[derive(FromRow)]
struct PaymentSums {
    cur: bool,
    /// Whether the row is a campaign's, with segment_id null for leads outside any campaign, or the period's total.
    by_campaign: bool,
    segment_id: Option<Uuid>,
    currency: String,
    net: i64,
    refunded: i64,
    payments: i64,
    leads: i64,
}

/// Payments kept in both periods, per currency, in total and per campaign of the lead who paid. Revenue comes from here
/// rather than won deals, since one deal can be paid many times.
async fn payment_sums(db: &PgPool, id: Uuid, q: &AnalyticsQuery, previous_from: NaiveDate, from: NaiveDate, to: NaiveDate, tz: &str) -> sqlx::Result<Vec<PaymentSums>> {
    if q.mailbox_id.is_some() {
        return Ok(vec![]);
    }
    sqlx::query_as(
        "select p.paid_at >= $3::date::timestamp at time zone $5 as cur, grouping(l.segment_id) = 0 as by_campaign, l.segment_id, p.currency,
           sum(p.amount_cents)::bigint as net, coalesce(-sum(p.amount_cents) filter (where p.kind = 'refund'), 0)::bigint as refunded,
           count(*) filter (where p.kind = 'payment') as payments, count(distinct p.lead_id) filter (where p.kind = 'payment') as leads
         from payments p left join leads l on l.id = p.lead_id
         where p.project_id = $1 and p.paid_at >= $2::date::timestamp at time zone $5 and p.paid_at < ($4::date + 1)::timestamp at time zone $5
           and ($6::uuid is null or l.segment_id = $6)
         group by grouping sets ((1, p.currency), (1, l.segment_id, p.currency))",
    )
    .bind(id)
    .bind(previous_from)
    .bind(from)
    .bind(to)
    .bind(tz)
    .bind(q.segment_id)
    .fetch_all(db)
    .await
}

struct Project {
    org_id: Option<Uuid>,
    timezone: String,
    currency: String,
}

/// The project's numbers for `q`. The caller checks the project belongs to the organization.
pub async fn analytics(db: &PgPool, id: Uuid, q: &AnalyticsQuery) -> AppResult<Analytics> {
    let (org_id, timezone, currency): (Option<Uuid>, String, String) = sqlx::query_as(
        "select p.org_id, coalesce(ps.timezone, 'UTC'), coalesce(o.currency, 'USD') from projects p
         left join project_settings ps on ps.project_id = p.id left join organizations o on o.id = p.org_id where p.id = $1",
    )
    .bind(id)
    .fetch_one(db)
    .await?;
    // Postgres would refuse a zone it doesn't know, and the Schedule page only saves known ones, so this is for old rows.
    let tz: Tz = timezone.parse().unwrap_or(Tz::UTC);
    let project = Project { org_id, timezone: tz.name().to_string(), currency };
    let today = Utc::now().with_timezone(&tz).date_naive();
    let to = q.to.unwrap_or(today);
    let from = q.from.unwrap_or(to - Duration::days(DEFAULT_DAYS - 1));
    if from > to {
        return Err(AppError::BadRequest("The range has to start on or before the day it ends".into()));
    }
    let days = (to - from).num_days() + 1;
    if days > MAX_DAYS {
        return Err(AppError::BadRequest(format!("Pick a range of at most {MAX_DAYS} days")));
    }
    let weekly = days > DAILY_UP_TO;
    let (previous_from, previous_to) = (from - Duration::days(days), from - Duration::days(1));

    let rows = sqlx::query_as::<_, Row>(FACTS)
        .bind(id)
        .bind(previous_from)
        .bind(from)
        .bind(to)
        .bind(&project.timezone)
        .bind(q.segment_id)
        .bind(q.mailbox_id)
        .bind(if weekly { "week" } else { "day" })
        .fetch_all(db)
        .await?;
    let claude = claude_costs(db, id, previous_from, from, to, &project.timezone).await?;
    let paid = payment_sums(db, id, q, previous_from, from, to, &project.timezone).await?;
    let revenue_tracked: bool = sqlx::query_scalar(
        "select exists (select 1 from payment_connections where project_id = $1) or exists (select 1 from payments where project_id = $1)",
    )
    .bind(id)
    .fetch_one(db)
    .await?;
    // Agent runs belong to a campaign or the whole project, never a mailbox.
    let claude_for = |cur: bool, segment: Option<Uuid>| -> i64 {
        if q.mailbox_id.is_some() {
            return 0;
        }
        claude.iter().filter(|c| c.cur == cur && (segment.is_none() || c.segment_id == segment)).map(|c| c.micros).sum()
    };

    let mut totals = [Totals::default(), Totals::default()];
    let mut won_value: [Vec<Money>; 2] = [vec![], vec![]];
    let (mut series, mut campaigns, mut mailboxes, mut steps, mut heatmap, mut angles) = (vec![], vec![], vec![], vec![], vec![], vec![]);
    let mut languages = vec![];
    let mut email_replies = 0;
    for row in rows {
        let n = row.n;
        match row.breakdown.as_str() {
            "total" => {
                if row.cur {
                    email_replies = n.email_replies;
                }
                totals[usize::from(row.cur)] = Totals::from_counts(&n, claude_for(row.cur, q.segment_id));
            }
            "currency" => {
                if let Some(currency) = row.currency {
                    won_value[usize::from(row.cur)].push(Money { currency, value_cents: n.won_cents });
                }
            }
            _ if !row.cur => {}
            "series" => series.push((row.bucket.unwrap_or_default(), n)),
            "campaign" => campaigns.push((row.segment_id, n)),
            "angle" => angles.extend(row.variant_id.map(|v| (v, n))),
            "mailbox" => mailboxes.push((row.mailbox_id, n)),
            "step" => {
                if let Some(step) = row.step.filter(|_| n.sent > 0 || n.replies > 0) {
                    let engagement = Engagement::from_counts(&n);
                    steps.push(StepRow { step, sent: n.sent, replies: n.replies, reply_rate: rate(n.replies, n.sent), engagement });
                }
            }
            "language" => {
                if let Some(lang) = row.language.as_deref().and_then(crate::language::find).filter(|_| n.sent > 0 || n.replies > 0) {
                    let (sent, contacted, replies) = (n.sent, n.contacted, n.replies);
                    languages.push(LanguageRow { language: lang.code, name: lang.name, sent, contacted, replies, reply_rate: rate(replies, contacted) });
                }
            }
            "slot" => {
                if let (Some(weekday), Some(hour)) = (row.weekday, row.hour)
                    && (n.sent > 0 || n.replies > 0)
                {
                    heatmap.push(SlotRow { weekday, hour, sent: n.sent, replies: n.replies });
                }
            }
            _ => {}
        }
    }
    for (t, mut won) in totals.iter_mut().zip(won_value) {
        won.sort_by(|a, b| (a.currency != project.currency).cmp(&(b.currency != project.currency)).then(a.currency.cmp(&b.currency)));
        t.won_value = won;
    }
    let (mut ignored_payments, mut ignored_currencies) = (0, vec![]);
    for p in paid.iter().filter(|p| !p.by_campaign) {
        if p.currency != project.currency {
            if p.cur {
                ignored_payments += p.payments;
                ignored_currencies.push(p.currency.clone());
            }
            continue;
        }
        let t = &mut totals[usize::from(p.cur)];
        (t.revenue_cents, t.refunded_cents, t.payments, t.paying_leads) = (p.net, p.refunded, p.payments, p.leads);
    }
    ignored_currencies.sort();
    let usd = project.currency == "USD";
    for t in &mut totals {
        t.revenue_per_meeting_cents = per(t.revenue_cents, t.meetings);
        t.return_on_spend = return_on(t.revenue_cents, t.spend_micros, usd);
    }
    let campaign_revenue: Vec<(Option<Uuid>, i64)> =
        paid.iter().filter(|p| p.by_campaign && p.cur && p.currency == project.currency).map(|p| (p.segment_id, p.net)).collect();
    let [previous, totals] = totals;
    let email = ChannelRow {
        channel: "email",
        sent: totals.sent,
        leads: Some(totals.contacted),
        replies: Some(email_replies),
        reply_rate: rate(email_replies, totals.contacted),
        connect_rate: None,
        meetings: None,
    };
    steps.sort_by_key(|s| s.step);
    heatmap.sort_by_key(|s| (s.weekday, s.hour));
    languages.sort_by_key(|l| (std::cmp::Reverse(l.contacted), l.language));

    Ok(Analytics {
        series: fill(series, from, to, weekly),
        campaigns: campaign_rows(db, id, campaigns, &campaign_revenue, usd, |segment| claude_for(true, segment)).await?,
        angles: angle_rows(db, id, angles).await?,
        mailboxes: mailbox_rows(db, project.org_id, mailboxes).await?,
        channels: channels(db, id, q, from, to, &project.timezone, email).await?,
        from,
        to,
        previous_from,
        previous_to,
        timezone: project.timezone,
        currency: project.currency,
        revenue_tracked,
        ignored_payments,
        ignored_currencies,
        interval: if weekly { "week" } else { "day" },
        totals,
        previous,
        steps,
        heatmap,
        languages,
    })
}

impl Totals {
    fn from_counts(n: &Counts, claude_cost_micros: i64) -> Self {
        let spend_micros = claude_cost_micros + n.email_cost_micros;
        Self {
            sent: n.sent,
            contacted: n.contacted,
            delivered: (n.sent - n.bounced).max(0),
            replies: n.replies,
            reply_rate: rate(n.replies, n.contacted),
            hot: n.hot,
            hot_rate: rate(n.hot, n.contacted),
            meetings: n.meetings,
            deals_won: n.won,
            won_value: vec![],
            unsubscribes: n.unsubscribes,
            bounced: n.bounced,
            bounce_rate: rate(n.bounced, n.contacted),
            engagement: Engagement::from_counts(n),
            claude_cost_micros,
            email_cost_micros: n.email_cost_micros,
            spend_micros,
            cost_per_hot_micros: per(spend_micros, n.hot),
            cost_per_meeting_micros: per(spend_micros, n.meetings),
            revenue_cents: 0,
            refunded_cents: 0,
            payments: 0,
            paying_leads: 0,
            revenue_per_meeting_cents: None,
            return_on_spend: None,
        }
    }
}

/// Every day or week of the range, empty ones included.
fn fill(rows: Vec<(String, Counts)>, from: NaiveDate, to: NaiveDate, weekly: bool) -> Vec<Point> {
    let mut by_date: std::collections::HashMap<String, Counts> = rows.into_iter().collect();
    let (mut date, step) = if weekly {
        (from - Duration::days(from.weekday().num_days_from_monday().into()), Duration::days(7))
    } else {
        (from, Duration::days(1))
    };
    let mut points = vec![];
    while date <= to {
        let key = date.format("%Y-%m-%d").to_string();
        let n = by_date.remove(&key).unwrap_or_default();
        points.push(Point { date: key, sent: n.sent, replies: n.replies, hot: n.hot, meetings: n.meetings });
        date += step;
    }
    points
}

struct ClaudeCost {
    cur: bool,
    segment_id: Option<Uuid>,
    micros: i64,
}

/// Agent run cost per campaign and period. Runs that research the whole project, like explore, have no campaign.
async fn claude_costs(db: &PgPool, id: Uuid, previous_from: NaiveDate, from: NaiveDate, to: NaiveDate, tz: &str) -> sqlx::Result<Vec<ClaudeCost>> {
    let rows: Vec<(bool, Option<String>, i64)> = sqlx::query_as(
        "select r.created_at >= $3::date::timestamp at time zone $5, r.input->>'segment_id', (sum(r.cost_usd) * 1000000)::bigint
         from agent_runs r
         where r.project_id = $1 and r.created_at >= $2::date::timestamp at time zone $5 and r.created_at < ($4::date + 1)::timestamp at time zone $5
         group by 1, 2",
    )
    .bind(id)
    .bind(previous_from)
    .bind(from)
    .bind(to)
    .bind(tz)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(cur, segment, micros)| ClaudeCost { cur, segment_id: segment.and_then(|s| s.parse().ok()), micros }).collect())
}

async fn campaign_rows(
    db: &PgPool,
    id: Uuid,
    mut rows: Vec<(Option<Uuid>, Counts)>,
    revenue: &[(Option<Uuid>, i64)],
    usd: bool,
    claude: impl Fn(Option<Uuid>) -> i64,
) -> sqlx::Result<Vec<CampaignRow>> {
    let names: Vec<(Uuid, String)> = sqlx::query_as("select id, name from segments where project_id = $1").bind(id).fetch_all(db).await?;
    // A campaign whose leads paid in the range gets a row even when nothing else about it happened then.
    for (segment, _) in revenue {
        if !rows.iter().any(|(s, _)| s == segment) {
            rows.push((*segment, Counts::default()));
        }
    }
    Ok(rows
        .into_iter()
        .map(|(segment_id, n)| {
            let revenue_cents = revenue.iter().find(|(s, _)| *s == segment_id).map_or(0, |(_, cents)| *cents);
            // The project-wide runs, like explore, aren't any one campaign's, so the no-campaign row leaves them out.
            let spend_micros = n.email_cost_micros + segment_id.map_or(0, |s| claude(Some(s)));
            let name = segment_id.and_then(|s| names.iter().find(|(id, _)| *id == s)).map_or("No campaign", |(_, name)| name.as_str());
            CampaignRow {
                segment_id,
                name: name.to_string(),
                sent: n.sent,
                contacted: n.contacted,
                replies: n.replies,
                reply_rate: rate(n.replies, n.contacted),
                hot: n.hot,
                meetings: n.meetings,
                deals_won: n.won,
                bounced: n.bounced,
                bounce_rate: rate(n.bounced, n.contacted),
                engagement: Engagement::from_counts(&n),
                spend_micros,
                cost_per_hot_micros: per(spend_micros, n.hot),
                revenue_cents,
                return_on_spend: return_on(revenue_cents, spend_micros, usd),
            }
        })
        .collect())
}

async fn mailbox_rows(db: &PgPool, org_id: Option<Uuid>, rows: Vec<(Option<Uuid>, Counts)>) -> sqlx::Result<Vec<MailboxRow>> {
    let emails: Vec<(Uuid, String)> =
        sqlx::query_as("select id, email::text from mailboxes where org_id = $1").bind(org_id).fetch_all(db).await?;
    Ok(rows
        .into_iter()
        // Meetings and deals of leads never emailed have no mailbox, which says nothing about sending.
        .filter(|(mailbox_id, n)| mailbox_id.is_some() || n.sent > 0)
        .map(|(mailbox_id, n)| MailboxRow {
            mailbox_id,
            email: mailbox_id.and_then(|m| emails.iter().find(|(id, _)| *id == m)).map_or("Removed mailbox", |(_, e)| e.as_str()).to_string(),
            sent: n.sent,
            contacted: n.contacted,
            replies: n.replies,
            reply_rate: rate(n.replies, n.contacted),
            hot: n.hot,
            bounced: n.bounced,
            bounce_rate: rate(n.bounced, n.contacted),
            bouncing: bouncing(n.contacted, n.bounced),
            engagement: Engagement::from_counts(&n),
        })
        .collect())
}

async fn angle_rows(db: &PgPool, id: Uuid, rows: Vec<(Uuid, Counts)>) -> sqlx::Result<Vec<AngleRow>> {
    let names: Vec<(Uuid, Uuid, String, String)> = sqlx::query_as(
        "select v.id, s.id, v.name, s.name from segment_variants v join segments s on s.id = v.segment_id where s.project_id = $1
         order by s.position, s.created_at, v.position, v.created_at",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    Ok(names
        .into_iter()
        .filter_map(|(variant_id, segment_id, name, campaign)| {
            let (_, n) = rows.iter().find(|(v, _)| *v == variant_id)?;
            Some(AngleRow {
                variant_id,
                segment_id,
                name,
                campaign,
                sent: n.sent,
                contacted: n.contacted,
                replies: n.replies,
                reply_rate: rate(n.replies, n.contacted),
                engagement: Engagement::from_counts(n),
            })
        })
        .collect())
}

#[derive(FromRow)]
struct ChannelCounts {
    linkedin: i64,
    linkedin_leads: i64,
    sms: i64,
    sms_leads: i64,
    sms_replies: i64,
    whatsapp: i64,
    whatsapp_leads: i64,
    whatsapp_replies: i64,
    calls: i64,
    call_leads: i64,
    connected: i64,
    call_meetings: i64,
    posts: i64,
}

/// `email` from the totals, then LinkedIn steps sent, texts and social posts. A mailbox filter keeps only email, and a
/// campaign filter drops social posts, which belong to the whole project.
async fn channels(db: &PgPool, id: Uuid, q: &AnalyticsQuery, from: NaiveDate, to: NaiveDate, tz: &str, email: ChannelRow) -> sqlx::Result<Vec<ChannelRow>> {
    if q.mailbox_id.is_some() {
        return Ok(vec![email]);
    }
    let c = sqlx::query_as::<_, ChannelCounts>(
        "with w as (select $2::date::timestamp at time zone $4 as lo, ($3::date + 1)::timestamp at time zone $4 as hi),
         linkedin as (
           select count(*) as linkedin, count(distinct m.lead_id) as linkedin_leads from linkedin_messages m join leads l on l.id = m.lead_id, w
           where m.project_id = $1 and m.status = 'done' and m.done_at >= w.lo and m.done_at < w.hi and ($5::uuid is null or l.segment_id = $5)
         ), texts as (
           select count(*) filter (where t.channel = 'sms') as sms, count(distinct t.lead_id) filter (where t.channel = 'sms') as sms_leads,
             count(*) filter (where t.channel = 'whatsapp') as whatsapp,
             count(distinct t.lead_id) filter (where t.channel = 'whatsapp') as whatsapp_leads
           from text_messages t, w
           where t.project_id = $1 and t.status in ('sent', 'delivered') and t.sent_at >= w.lo and t.sent_at < w.hi
             and ($5::uuid is null or t.segment_id = $5)
         ), first_replies as (
           -- Like the email row, a lead counts once, for the channel and range of their first reply.
           select r.lead_id, min(r.created_at) as at, (array_agg(r.channel order by r.created_at))[1] as channel
           from email_messages r
           where r.project_id = $1 and r.direction = 'inbound' and r.classification is distinct from 'bounce'
           group by r.lead_id
         ), text_replies as (
           select count(*) filter (where f.channel = 'sms') as sms_replies, count(*) filter (where f.channel = 'whatsapp') as whatsapp_replies
           from first_replies f join leads l on l.id = f.lead_id, w
           where f.at >= w.lo and f.at < w.hi and ($5::uuid is null or l.segment_id = $5)
         ), calls as (
           -- A call counts once it's logged, on the day it was made.
           select count(*) as calls, count(distinct c.lead_id) as call_leads,
             count(*) filter (where c.outcome in ('connected', 'not_interested', 'meeting_booked')) as connected,
             count(*) filter (where c.outcome = 'meeting_booked') as call_meetings
           from calls c, w
           where c.project_id = $1 and c.status = 'done' and coalesce(c.called_at, c.closed_at) >= w.lo and coalesce(c.called_at, c.closed_at) < w.hi
             and ($5::uuid is null or c.segment_id = $5)
         )
         select linkedin.*, texts.*, text_replies.*, calls.*,
           (select count(*) from social_posts p, w where p.project_id = $1 and p.status = 'posted' and p.posted_at >= w.lo and p.posted_at < w.hi) as posts
         from linkedin, texts, text_replies, calls",
    )
    .bind(id)
    .bind(from)
    .bind(to)
    .bind(tz)
    .bind(q.segment_id)
    .fetch_one(db)
    .await?;
    let row = |channel, sent, leads| ChannelRow { channel, sent, leads, replies: None, reply_rate: None, connect_rate: None, meetings: None };
    let texts = |channel, sent, leads, replies| ChannelRow { replies: Some(replies), reply_rate: rate(replies, leads), ..row(channel, sent, Some(leads)) };
    let mut rows = vec![
        email,
        row("linkedin", c.linkedin, Some(c.linkedin_leads)),
        texts("sms", c.sms, c.sms_leads, c.sms_replies),
        texts("whatsapp", c.whatsapp, c.whatsapp_leads, c.whatsapp_replies),
        ChannelRow { connect_rate: rate(c.connected, c.calls), meetings: Some(c.call_meetings), ..row("call", c.calls, Some(c.call_leads)) },
    ];
    if q.segment_id.is_none() {
        rows.push(row("social", c.posts, None));
    }
    Ok(rows)
}

async fn project_analytics(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<AnalyticsQuery>,
) -> AppResult<Json<Analytics>> {
    auth.project(&state.db, id).await?;
    Ok(Json(analytics(&state.db, id, &q).await?))
}

#[cfg(test)]
mod tests {
    use axum::Json;
    use axum::extract::{Path, Query, State};
    use chrono::NaiveDate;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::{Analytics, AnalyticsQuery, analytics};
    use crate::auth::{ApiKeyAuth, Auth};
    use crate::error::AppError;
    use crate::models::Ts;

    struct Seeded {
        org: Uuid,
        project: Uuid,
        alpha: Uuid,
        second_mailbox: Uuid,
    }

    /// October 2026 in New York, which is UTC-4 then. Every number the tests check can be worked out from this by hand.
    async fn seed(db: &PgPool) -> Seeded {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(db).await.unwrap();
        sqlx::query("insert into project_settings (project_id, timezone) values ($1, 'America/New_York')").bind(project).execute(db).await.unwrap();
        let segment = |name: &'static str| sqlx::query_scalar::<_, Uuid>("insert into segments (project_id, name) values ($1, $2) returning id").bind(project).bind(name);
        let (alpha, beta) = (segment("Alpha").fetch_one(db).await.unwrap(), segment("Beta").fetch_one(db).await.unwrap());
        let mailbox = |email: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, $2, 'smtp.test', '', '') returning id")
                .bind(org)
                .bind(email)
        };
        let (m1, m2) = (mailbox("a@acme.test").fetch_one(db).await.unwrap(), mailbox("b@acme.test").fetch_one(db).await.unwrap());
        let lead = |segment: Uuid, status: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, segment_id, status) values ($1, $2, $3) returning id").bind(project).bind(segment).bind(status)
        };
        let l1 = lead(alpha, "hot").fetch_one(db).await.unwrap();
        let l2 = lead(alpha, "meeting_booked").fetch_one(db).await.unwrap();
        let l3 = lead(beta, "bounced").fetch_one(db).await.unwrap();
        let l4 = lead(beta, "unsubscribed").fetch_one(db).await.unwrap();
        let l5 = lead(alpha, "replied").fetch_one(db).await.unwrap();
        let l6 = lead(beta, "contacted").fetch_one(db).await.unwrap();
        for (lead, at) in [(l2, "2026-10-12 15:00Z"), (l4, "2026-10-08 12:00Z")] {
            sqlx::query("update leads set status_changed_at = $2::timestamptz where id = $1").bind(lead).bind(at).execute(db).await.unwrap();
        }

        // (lead, mailbox, kind, step, classification, channel, at). Outbound kinds are sent for 1,000 micros, a manual reply for 300.
        let emails = [
            // Monday 09:00 in New York. The first gets a question back, the second a hot reply to its follow-up on Thursday.
            (l1, Some(m1), "initial", 0, None, "email", "2026-10-05 13:00Z"),
            (l1, None, "inbound", 0, Some("question"), "email", "2026-10-06 14:00Z"),
            (l1, Some(m1), "manual_reply", 0, None, "email", "2026-10-06 15:00Z"),
            (l2, Some(m1), "initial", 0, None, "email", "2026-10-05 13:00Z"),
            (l2, Some(m1), "follow_up", 1, None, "email", "2026-10-08 13:00Z"),
            (l2, None, "inbound", 0, Some("hot"), "email", "2026-10-09 14:00Z"),
            // Tuesday 03:30 UTC is still Monday 23:30 in New York.
            (l3, Some(m2), "initial", 0, None, "email", "2026-10-06 03:30Z"),
            (l3, None, "inbound", 0, Some("bounce"), "email", "2026-10-06 04:00Z"),
            (l4, Some(m2), "initial", 0, None, "email", "2026-10-07 13:00Z"),
            // The previous period. L6's email went out on September 30 in New York, though it was October 1 in UTC.
            (l5, Some(m1), "initial", 0, None, "email", "2026-09-20 13:00Z"),
            (l5, None, "inbound", 0, Some("question"), "email", "2026-09-22 13:00Z"),
            (l6, Some(m2), "initial", 0, None, "email", "2026-10-01 03:00Z"),
            // L5 answers a text in October too, which isn't their first reply, so it counts nowhere.
            (l5, None, "inbound", 0, Some("question"), "sms", "2026-10-11 13:00Z"),
            // A draft never counts.
            (l6, Some(m2), "follow_up", 1, None, "email", "2026-10-04 13:00Z"),
        ];
        for (i, (lead, mailbox, kind, step, classification, channel, at)) in emails.into_iter().enumerate() {
            let inbound = kind == "inbound";
            let status = if inbound { "received" } else if i == 13 { "draft" } else { "sent" };
            let segment = if [l1, l2, l5].contains(&lead) { alpha } else { beta };
            sqlx::query(
                "insert into email_messages (project_id, lead_id, segment_id, mailbox_id, direction, kind, step, status, classification, channel,
                   sent_at, created_at, cost_micros)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, case when $8 = 'sent' then $11::timestamptz end, $11::timestamptz, $12)",
            )
            .bind(project)
            .bind(lead)
            .bind(segment)
            .bind(mailbox)
            .bind(if inbound { "inbound" } else { "outbound" })
            .bind(kind)
            .bind(step)
            .bind(status)
            .bind(classification)
            .bind(channel)
            .bind(at)
            .bind(match kind {
                "inbound" => 0i64,
                "manual_reply" => 300,
                _ => 1000,
            })
            .execute(db)
            .await
            .unwrap();
        }

        let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Won', 'won', 0) returning id")
            .bind(org)
            .fetch_one(db)
            .await
            .unwrap();
        for (lead, cents, currency, at) in [(l2, 120_000i64, "USD", "2026-10-20 12:00Z"), (l1, 5_000, "EUR", "2026-10-22 12:00Z")] {
            sqlx::query(
                "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, closed_at) values ($1, $2, $3, $4, 'won', $5, $6, $7::timestamptz)",
            )
            .bind(org)
            .bind(project)
            .bind(lead)
            .bind(stage)
            .bind(cents)
            .bind(currency)
            .bind(at)
            .execute(db)
            .await
            .unwrap();
        }
        // Alpha's writer run and a project-wide explore in October, and a run in September.
        for (kind, segment, usd, at) in
            [("write_emails", Some(alpha), 0.5, "2026-10-03 12:00Z"), ("explore", None, 1.0, "2026-10-04 12:00Z"), ("explore", None, 2.0, "2026-09-10 12:00Z")]
        {
            let input = segment.map_or(serde_json::json!({}), |s: Uuid| serde_json::json!({ "segment_id": s }));
            sqlx::query("insert into agent_runs (kind, project_id, org_id, input, status, cost_usd, created_at) values ($1, $2, $3, $4, 'succeeded', $5, $6::timestamptz)")
                .bind(kind)
                .bind(project)
                .bind(org)
                .bind(input)
                .bind(usd)
                .bind(at)
                .execute(db)
                .await
                .unwrap();
        }
        sqlx::query("insert into linkedin_messages (lead_id, project_id, step, kind, body, status, done_at) values ($1, $2, 0, 'connect', 'Hi', 'done', '2026-10-10 12:00Z')")
            .bind(l1)
            .bind(project)
            .execute(db)
            .await
            .unwrap();
        sqlx::query(
            "insert into text_messages (project_id, lead_id, segment_id, channel, kind, to_number, status, sent_at) values ($1, $2, $3, 'sms', 'sequence', '+15550100', 'delivered', '2026-10-10 13:00Z')",
        )
        .bind(project)
        .bind(l5)
        .bind(alpha)
        .execute(db)
        .await
        .unwrap();
        // Calls made in October: two answered, one of them booked, and a no answer. One still open and one from September don't count.
        for (lead, segment, status, outcome, at) in [
            (l1, Some(alpha), "done", Some("connected"), "2026-10-10 14:00Z"),
            (l2, Some(alpha), "done", Some("meeting_booked"), "2026-10-11 14:00Z"),
            (l1, Some(alpha), "done", Some("no_answer"), "2026-10-12 14:00Z"),
            (l6, Some(beta), "open", None, "2026-10-12 14:00Z"),
            (l6, Some(beta), "done", Some("connected"), "2026-09-12 14:00Z"),
        ] {
            sqlx::query("insert into calls (project_id, lead_id, segment_id, kind, status, outcome, called_at, closed_at) values ($1, $2, $3, 'manual', $4, $5, $6::timestamptz, $6::timestamptz)")
                .bind(project)
                .bind(lead)
                .bind(segment)
                .bind(status)
                .bind(outcome)
                .bind(at)
                .execute(db)
                .await
                .unwrap();
        }
        sqlx::query("insert into social_posts (project_id, platform, body, status, posted_at) values ($1, 'x', 'Hello', 'posted', '2026-10-15 12:00Z')")
            .bind(project)
            .execute(db)
            .await
            .unwrap();
        Seeded { org, project, alpha, second_mailbox: m2 }
    }

    fn day(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn october() -> AnalyticsQuery {
        AnalyticsQuery { from: Some(day("2026-10-01")), to: Some(day("2026-10-31")), ..Default::default() }
    }

    async fn json(db: &PgPool, project: Uuid, q: AnalyticsQuery) -> serde_json::Value {
        serde_json::to_value(analytics(db, project, &q).await.unwrap()).unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn reply_rate_by_the_language_the_emails_were_written_in(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(&db).await.unwrap();
        // Two German leads with a first email and a follow-up each, one of whom replied after the follow-up, a French lead who
        // replied, and one written to before emails had a language, which counts as English.
        for (language, emails, replied) in [("de", 2, true), ("de", 2, false), ("fr", 1, true), ("", 1, false)] {
            sqlx::query(
                "with l as (insert into leads (project_id, status) values ($1, 'contacted') returning id),
                      e as (insert into email_messages (project_id, lead_id, direction, kind, step, status, sent_at, language)
                            select $1, l.id, 'outbound', case when g = 0 then 'initial' else 'follow_up' end, g, 'sent',
                                   now() - interval '3 days' + g * interval '1 day', $2
                            from l, generate_series(0, $3 - 1) g returning lead_id)
                 insert into email_messages (project_id, lead_id, direction, kind, status, created_at)
                 select distinct $1, lead_id, 'inbound', 'inbound', 'received', now() - interval '1 hour' from e where $4",
            )
            .bind(project)
            .bind(language)
            .bind(emails)
            .bind(replied)
            .execute(&db)
            .await
            .unwrap();
        }
        let a = json(&db, project, AnalyticsQuery::default()).await;
        let row = |l: &serde_json::Value| {
            (l["language"].as_str().unwrap().to_string(), l["name"].as_str().unwrap().to_string(), l["sent"].as_i64(), l["contacted"].as_i64(), l["replies"].as_i64(), l["reply_rate"].as_f64())
        };
        let rows: Vec<_> = a["languages"].as_array().unwrap().iter().map(row).collect();
        let want = |code: &str, name: &str, sent, contacted, replies, rate| (code.to_string(), name.to_string(), Some(sent), Some(contacted), Some(replies), Some(rate));
        assert_eq!(rows, vec![want("de", "German", 4, 2, 1, 0.5), want("en", "English", 1, 1, 0, 0.0), want("fr", "French", 1, 1, 1, 1.0)]);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn totals_steps_and_the_period_before(db: PgPool) {
        let s = seed(&db).await;
        let a = json(&db, s.project, october()).await;
        let t = &a["totals"];
        let pick = |v: &serde_json::Value, keys: &[&str]| keys.iter().map(|k| v[*k].clone()).collect::<Vec<_>>();
        assert_eq!(
            pick(t, &["sent", "contacted", "delivered", "bounced", "replies", "hot", "meetings", "deals_won", "unsubscribes"]),
            [5, 4, 4, 1, 2, 2, 1, 2, 1].map(serde_json::Value::from)
        );
        assert_eq!(pick(t, &["reply_rate", "hot_rate", "bounce_rate"]), [0.5, 0.5, 0.25].map(serde_json::Value::from));
        // The organization's currency first.
        assert_eq!(t["won_value"], serde_json::json!([{ "currency": "USD", "value_cents": 120000 }, { "currency": "EUR", "value_cents": 5000 }]));
        assert_eq!(
            pick(t, &["claude_cost_micros", "email_cost_micros", "spend_micros", "cost_per_hot_micros", "cost_per_meeting_micros"]),
            [1_500_000, 5_300, 1_505_300, 752_650, 1_505_300].map(serde_json::Value::from)
        );
        assert_eq!((a["previous_from"].as_str(), a["previous_to"].as_str()), (Some("2026-08-31"), Some("2026-09-30")));
        let p = &a["previous"];
        assert_eq!(pick(p, &["sent", "contacted", "replies", "claude_cost_micros", "email_cost_micros"]), [2, 2, 1, 2_000_000, 2_000].map(serde_json::Value::from));

        // The hot reply came after the follow-up, so it counts there.
        let steps: Vec<_> = a["steps"].as_array().unwrap().iter().map(|s| pick(s, &["step", "sent", "replies", "reply_rate"])).collect();
        let expected = serde_json::json!([[0, 4, 1, 0.25], [1, 1, 1, 1.0]]);
        assert_eq!(serde_json::json!(steps), expected);
        // Nothing here tracked opens or clicks, so there are no rates to show.
        assert_eq!(pick(&a["steps"][0], &["opened", "open_rate", "clicked", "click_rate"]), [0.into(), serde_json::Value::Null, 0.into(), serde_json::Value::Null]);
        let campaigns: Vec<_> = a["campaigns"].as_array().unwrap().iter().map(|c| pick(c, &["name", "sent", "replies", "bounced", "spend_micros"])).collect();
        assert!(campaigns.contains(&["Alpha".into(), 3.into(), 2.into(), 0.into(), 503_300.into()].to_vec()));
        assert!(campaigns.contains(&["Beta".into(), 2.into(), 0.into(), 1.into(), 2_000.into()].to_vec()));
        let mailboxes: Vec<_> = a["mailboxes"].as_array().unwrap().iter().map(|m| pick(m, &["email", "sent", "contacted", "replies", "bounce_rate"])).collect();
        assert!(mailboxes.contains(&["a@acme.test".into(), 3.into(), 2.into(), 2.into(), 0.0.into()].to_vec()));
        assert!(mailboxes.contains(&["b@acme.test".into(), 2.into(), 2.into(), 0.into(), 0.5.into()].to_vec()));
        assert_eq!(a["mailboxes"].as_array().unwrap().len(), 2);

        let channels: Vec<_> = a["channels"].as_array().unwrap().iter().map(|c| pick(c, &["channel", "sent", "leads", "replies"])).collect();
        assert_eq!(
            channels,
            [("email", 5, Some(4), Some(2)), ("linkedin", 1, Some(1), None), ("sms", 1, Some(1), Some(0)), ("whatsapp", 0, Some(0), Some(0)), ("call", 3, Some(2), None),
             ("social", 1, None, None)]
                .map(|(c, sent, leads, replies)| vec![serde_json::Value::from(c), sent.into(), leads.into(), replies.into()])
        );
        let calls = a["channels"].as_array().unwrap().iter().find(|c| c["channel"] == "call").unwrap();
        assert_eq!((calls["connect_rate"].as_f64(), calls["meetings"].as_i64()), (Some(2.0 / 3.0), Some(1)));
        assert!(a["channels"][0]["connect_rate"].is_null() && a["channels"][0]["meetings"].is_null(), "only calls have them");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn days_hours_and_weeks_use_the_project_time_zone(db: PgPool) {
        let s = seed(&db).await;
        let a = json(&db, s.project, october()).await;
        // Monday 09:00 twice, one answered. Monday 23:30, which was Tuesday in UTC. Wednesday and Thursday 09:00.
        let cells: Vec<_> = a["heatmap"].as_array().unwrap().iter().map(|c| [&c["weekday"], &c["hour"], &c["sent"], &c["replies"]].map(|v| v.as_i64().unwrap())).collect();
        assert_eq!(cells, [[1, 9, 2, 1], [1, 23, 1, 0], [3, 9, 1, 0], [4, 9, 1, 1]]);

        let series = a["series"].as_array().unwrap();
        assert_eq!((a["interval"].as_str(), series.len()), (Some("day"), 31));
        let on = |date: &str| {
            let p = series.iter().find(|p| p["date"] == date).unwrap();
            [&p["sent"], &p["replies"], &p["hot"], &p["meetings"]].map(|v| v.as_i64().unwrap())
        };
        assert_eq!(on("2026-10-05"), [3, 0, 0, 0]);
        assert_eq!(on("2026-10-06"), [0, 1, 1, 0]);
        assert_eq!(on("2026-10-09"), [0, 1, 1, 0]);
        assert_eq!(on("2026-10-12"), [0, 0, 0, 1]);
        assert_eq!(on("2026-10-01"), [0, 0, 0, 0]);

        // Past 90 days the series goes by week, from the Monday on or before the first day.
        let long = json(&db, s.project, AnalyticsQuery { from: Some(day("2026-07-01")), to: Some(day("2026-10-31")), ..Default::default() }).await;
        let weeks = long["series"].as_array().unwrap();
        assert_eq!((long["interval"].as_str(), weeks[0]["date"].as_str(), weeks.len()), (Some("week"), Some("2026-06-29"), 18));
        let week = weeks.iter().find(|p| p["date"] == "2026-10-05").unwrap();
        assert_eq!((week["sent"].as_i64(), week["replies"].as_i64()), (Some(5), Some(2)));
        // L6's September 30 email is in the week of the 28th.
        assert_eq!(weeks.iter().find(|p| p["date"] == "2026-09-28").unwrap()["sent"], 1);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn filters_by_campaign_and_mailbox(db: PgPool) {
        let s = seed(&db).await;
        let alpha = json(&db, s.project, AnalyticsQuery { segment_id: Some(s.alpha), ..october() }).await;
        let t = &alpha["totals"];
        assert_eq!([&t["sent"], &t["replies"], &t["bounced"], &t["claude_cost_micros"], &t["email_cost_micros"]].map(|v| v.as_i64().unwrap()), [3, 2, 0, 500_000, 3_300]);
        assert_eq!(alpha["campaigns"].as_array().unwrap().len(), 1);
        assert!(alpha["channels"].as_array().unwrap().iter().all(|c| c["channel"] != "social"));

        // Runs don't belong to a mailbox, and the other channels don't go through one.
        let second = json(&db, s.project, AnalyticsQuery { mailbox_id: Some(s.second_mailbox), ..october() }).await;
        let t = &second["totals"];
        assert_eq!(
            [&t["sent"], &t["contacted"], &t["replies"], &t["bounced"], &t["unsubscribes"], &t["claude_cost_micros"], &t["email_cost_micros"]].map(|v| v.as_i64().unwrap()),
            [2, 2, 0, 1, 1, 0, 2_000]
        );
        assert_eq!(second["channels"].as_array().unwrap().len(), 1);
        let step = &second["steps"][0];
        assert_eq!(second["steps"].as_array().unwrap().len(), 1);
        assert_eq!(serde_json::json!([step["step"], step["sent"], step["replies"], step["reply_rate"]]), serde_json::json!([0, 2, 0, 0.0]));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn ranges_are_checked_and_stay_in_the_organization(db: PgPool) {
        let s = seed(&db).await;
        let state = crate::autopilot::tests::state(db.clone());
        let range = |from: &str, to: &str| AnalyticsQuery { from: Some(day(from)), to: Some(day(to)), ..Default::default() };
        assert!(matches!(analytics(&db, s.project, &range("2026-10-02", "2026-10-01")).await, Err(AppError::BadRequest(_))));
        assert!(matches!(analytics(&db, s.project, &range("2025-10-31", "2026-10-31")).await, Err(AppError::BadRequest(_))));
        let year = analytics(&db, s.project, &range("2025-11-01", "2026-10-31")).await.unwrap();
        assert_eq!(serde_json::to_value(&year).unwrap()["totals"]["sent"], 7);

        // The last 30 days by default, ending today in the project's time zone.
        let Json(recent): Json<Analytics> = super::project_analytics(
            State(state.clone()),
            Auth { user_id: Uuid::new_v4(), org_id: s.org, role: "owner".into(), token_hash: String::new() },
            Path(s.project),
            Query(AnalyticsQuery::default()),
        )
        .await
        .unwrap();
        let recent = serde_json::to_value(recent).unwrap();
        assert_eq!(recent["series"].as_array().unwrap().len(), 30);

        // Another organization's project looks like one that doesn't exist.
        let other: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Other') returning id").fetch_one(&db).await.unwrap();
        let session = Auth { user_id: Uuid::new_v4(), org_id: other, role: "owner".into(), token_hash: String::new() };
        let refused = super::project_analytics(State(state.clone()), session, Path(s.project), Query(october())).await;
        assert!(matches!(refused, Err(AppError::NotFound)));
        let refused = super::super::api_v1::project_analytics(State(state), ApiKeyAuth { org_id: other }, Path(s.project), Query(october())).await;
        assert!(matches!(refused, Err(AppError::NotFound)));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn revenue_comes_from_payments_in_the_organizations_currency(db: PgPool) {
        let s = seed(&db).await;
        let untracked = json(&db, s.project, october()).await;
        assert_eq!((untracked["revenue_tracked"].as_bool(), untracked["totals"]["revenue_cents"].as_i64()), (Some(false), Some(0)));

        let lead = |status: &'static str| sqlx::query_scalar::<_, Uuid>("select id from leads where project_id = $1 and status = $2").bind(s.project).bind(status);
        let (alpha_lead, beta_lead, eur_lead, earlier) =
            (lead("meeting_booked").fetch_one(&db).await.unwrap(), lead("bounced").fetch_one(&db).await.unwrap(), lead("hot").fetch_one(&db).await.unwrap(), lead("replied").fetch_one(&db).await.unwrap());
        // October 31 at 23:00 in New York is November in UTC, and still counts.
        let payments = [
            (alpha_lead, "ch_1", "payment", 100_000i64, "USD", "2026-10-20 12:00Z"),
            (alpha_lead, "refund:ch_1:20000", "refund", -20_000, "USD", "2026-11-01 03:00Z"),
            (beta_lead, "ch_2", "payment", 50_000, "USD", "2026-10-10 12:00Z"),
            (eur_lead, "ch_3", "payment", 9_000, "EUR", "2026-10-12 12:00Z"),
            (earlier, "ch_4", "payment", 30_000, "USD", "2026-09-15 12:00Z"),
            (alpha_lead, "ch_5", "payment", 70_000, "USD", "2026-11-02 12:00Z"),
        ];
        for (lead, external_id, kind, cents, currency, at) in payments {
            sqlx::query(
                "insert into payments (org_id, project_id, lead_id, provider, external_id, kind, amount_cents, currency, paid_at)
                 values ($1, $2, $3, 'stripe', $4, $5, $6, $7, $8::timestamptz)",
            )
            .bind(s.org)
            .bind(s.project)
            .bind(lead)
            .bind(external_id)
            .bind(kind)
            .bind(cents)
            .bind(currency)
            .bind(at)
            .execute(&db)
            .await
            .unwrap();
        }

        let a = json(&db, s.project, october()).await;
        let t = &a["totals"];
        assert_eq!(a["revenue_tracked"], true);
        assert_eq!(
            [&t["revenue_cents"], &t["refunded_cents"], &t["payments"], &t["paying_leads"]].map(|v| v.as_i64().unwrap()),
            [130_000, 20_000, 2, 2]
        );
        // $1,300 over $1.5053 of spend.
        assert!((t["return_on_spend"].as_f64().unwrap() - 1300.0 / 1.5053).abs() < 1e-9);
        assert_eq!((a["ignored_payments"].as_i64(), a["ignored_currencies"].clone()), (Some(1), serde_json::json!(["EUR"])));
        assert_eq!(a["previous"]["revenue_cents"], 30_000);
        let campaign = |name: &str| a["campaigns"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap().clone();
        assert_eq!((campaign("Alpha")["revenue_cents"].as_i64(), campaign("Beta")["revenue_cents"].as_i64()), (Some(80_000), Some(50_000)));
        assert!((campaign("Alpha")["return_on_spend"].as_f64().unwrap() - 800.0 / 0.5033).abs() < 1e-9);

        // A campaign filter keeps its leads' payments, the euro one included. Payments don't belong to a mailbox, so a mailbox
        // filter has none.
        let alpha = json(&db, s.project, AnalyticsQuery { segment_id: Some(s.alpha), ..october() }).await;
        assert_eq!((alpha["totals"]["revenue_cents"].as_i64(), alpha["ignored_payments"].as_i64()), (Some(80_000), Some(1)));
        let mailbox = json(&db, s.project, AnalyticsQuery { mailbox_id: Some(s.second_mailbox), ..october() }).await;
        assert_eq!(mailbox["totals"]["revenue_cents"], 0);

        // A campaign with only a payment in the range still gets its row.
        let november = AnalyticsQuery { from: Some(day("2026-11-02")), to: Some(day("2026-11-02")), ..Default::default() };
        let n = json(&db, s.project, november).await;
        let rows: Vec<_> = n["campaigns"].as_array().unwrap().iter().map(|c| (c["name"].clone(), c["sent"].clone(), c["revenue_cents"].clone())).collect();
        assert_eq!(rows, [("Alpha".into(), 0.into(), 70_000.into())]);
        assert!(n["totals"]["return_on_spend"].is_null(), "nothing was spent");

        // Another organization's payments never count here.
        let other: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Other') returning id").fetch_one(&db).await.unwrap();
        let theirs: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Other') returning id").bind(other).fetch_one(&db).await.unwrap();
        assert_eq!(json(&db, theirs, october()).await["totals"]["revenue_cents"], 0);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_api_has_revenue_per_meeting_and_no_return_on_spend_outside_usd(db: PgPool) {
        let s = seed(&db).await;
        let lead = |status: &'static str| sqlx::query_scalar::<_, Uuid>("select id from leads where project_id = $1 and status = $2").bind(s.project).bind(status);
        let (booked, earlier) = (lead("meeting_booked").fetch_one(&db).await.unwrap(), lead("replied").fetch_one(&db).await.unwrap());
        // The earlier lead booked in September, which makes one meeting in each period.
        sqlx::query("update leads set status = 'meeting_booked' where id = $1").bind(earlier).execute(&db).await.unwrap();
        sqlx::query("update leads set status_changed_at = '2026-09-20 12:00Z' where id = $1").bind(earlier).execute(&db).await.unwrap();
        let payments = [(booked, "ch_1", 100_000i64, "USD", "2026-10-20 12:00Z"), (earlier, "ch_2", 30_000, "USD", "2026-09-15 12:00Z"), (booked, "ch_3", 9_000, "EUR", "2026-10-21 12:00Z")];
        for (lead, external_id, cents, currency, at) in payments {
            sqlx::query(
                "insert into payments (org_id, project_id, lead_id, provider, external_id, kind, amount_cents, currency, paid_at)
                 values ($1, $2, $3, 'stripe', $4, 'payment', $5, $6, $7::timestamptz)",
            )
            .bind(s.org)
            .bind(s.project)
            .bind(lead)
            .bind(external_id)
            .bind(cents)
            .bind(currency)
            .bind(at)
            .execute(&db)
            .await
            .unwrap();
        }
        let state = crate::autopilot::tests::state(db.clone());
        let api = || super::super::api_v1::project_analytics(State(state.clone()), ApiKeyAuth { org_id: s.org }, Path(s.project), Query(october()));
        let Json(usd) = api().await.unwrap();
        let usd = serde_json::to_value(usd).unwrap();
        assert_eq!((usd["totals"]["revenue_per_meeting_cents"].as_i64(), usd["previous"]["revenue_per_meeting_cents"].as_i64()), (Some(100_000), Some(30_000)));
        assert!(usd["totals"]["return_on_spend"].is_number());

        // Spend is in dollars, so revenue in euros has no return on it.
        sqlx::query("update organizations set currency = 'EUR' where id = $1").bind(s.org).execute(&db).await.unwrap();
        let Json(eur) = api().await.unwrap();
        let eur = serde_json::to_value(eur).unwrap();
        assert_eq!((eur["totals"]["revenue_cents"].as_i64(), eur["totals"]["revenue_per_meeting_cents"].as_i64()), (Some(9_000), Some(9_000)));
        assert!(eur["totals"]["return_on_spend"].is_null() && eur["previous"]["return_on_spend"].is_null());
        let campaigns = eur["campaigns"].as_array().unwrap();
        assert!(campaigns.iter().any(|c| c["revenue_cents"] == 9_000) && campaigns.iter().all(|c| c["return_on_spend"].is_null()));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_status_change_records_when(db: PgPool) {
        let s = seed(&db).await;
        let lead: Uuid = sqlx::query_scalar("select id from leads where project_id = $1 and status = 'hot'").bind(s.project).fetch_one(&db).await.unwrap();
        let at = || sqlx::query_scalar::<_, Option<Ts>>("select status_changed_at from leads where id = $1").bind(lead).fetch_one(&db);
        let before = at().await.unwrap().unwrap();
        // Setting the same status, or another column, leaves the time alone.
        sqlx::query("update leads set status = 'hot', notes = 'x' where id = $1").bind(lead).execute(&db).await.unwrap();
        assert_eq!(at().await.unwrap(), Some(before));
        sqlx::query("update leads set status = 'meeting_booked' where id = $1").bind(lead).execute(&db).await.unwrap();
        assert!(at().await.unwrap().unwrap() > before);
    }
}
