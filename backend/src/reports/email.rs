//! Each report link's summary email, sent through mail::send_system_html. Weekly ones cover Monday to Sunday and go out
//! on Monday from SEND_HOUR in the project's time zone, monthly ones cover the month before and go out on the 1st. A
//! period with no activity sends nothing, and each recipient's unsubscribe stops only that link's emails.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use uuid::Uuid;

use super::{Brand, Person, ReportLink, UNSUBSCRIBE, VIEW, brand, meetings, recipient_token};
use crate::models::Ts;
use crate::routes::analytics::{self, Analytics, AnalyticsQuery, Totals};
use crate::state::AppState;

pub const SEND_HOUR: u32 = 8;
pub const MAX_RECIPIENTS: usize = 5;
/// Names an email lists under the meetings booked. The page has the rest.
const LISTED_MEETINGS: usize = 10;
/// Every 15 minutes, so the first check after 8:00 in each time zone sends it. A copy that fails is tried again at the next
/// check, and the rest still go.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    for email in due(state, Utc::now()).await? {
        let send = || crate::mail::send_system_html(state, email.org_id, &email.to, &email.subject, &email.text, &email.html, &email.one_click_url);
        if let Err(e) = deliver(&state.db, &email, send).await {
            tracing::warn!(link = %email.link_id, error = ?e, "couldn't send a client report, trying again at the next check");
        }
    }
    Ok(())
}

/// Sends one copy while holding its report_deliveries row locked, so another backend skips it until this one has sent it
/// and saved that, or failed and let go. A crash rolls back and leaves it unsent for the next check. Ok(false) when another
/// backend has it or it went out already.
pub async fn deliver<F, Fut>(db: &sqlx::PgPool, email: &ReportEmail, send: F) -> anyhow::Result<bool>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<bool>>,
{
    let mut tx = db.begin().await?;
    sqlx::query("insert into report_deliveries (recipient_id, period) values ($1, $2) on conflict do nothing")
        .bind(email.recipient_id)
        .bind(email.period)
        .execute(&mut *tx)
        .await?;
    let mine: Option<i32> =
        sqlx::query_scalar("select 1 from report_deliveries where recipient_id = $1 and period = $2 and sent_at is null for update skip locked")
            .bind(email.recipient_id)
            .bind(email.period)
            .fetch_optional(&mut *tx)
            .await?;
    if mine.is_none() {
        return Ok(false);
    }
    anyhow::ensure!(send().await?, "no active mailbox to send it from");
    sqlx::query("update report_deliveries set sent_at = now() where recipient_id = $1 and period = $2")
        .bind(email.recipient_id)
        .bind(email.period)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// The days a link's email covers when it's due at `local`, the time in the project's time zone.
pub fn period(schedule: &str, local: NaiveDateTime) -> Option<(NaiveDate, NaiveDate)> {
    let today = local.date();
    let yesterday = today - Duration::days(1);
    match schedule {
        _ if local.hour() < SEND_HOUR => None,
        "weekly" if today.weekday() == Weekday::Mon => Some((today - Duration::days(7), yesterday)),
        "monthly" if today.day() == 1 => Some((yesterday.with_day(1)?, yesterday)),
        _ => None,
    }
}

pub struct ReportEmail {
    pub link_id: Uuid,
    pub recipient_id: Uuid,
    /// The first day of the period it covers, which its delivery row is keyed on.
    pub period: NaiveDate,
    pub org_id: Uuid,
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
    /// What the List-Unsubscribe header points to, for the one-click POST mail clients send. The email links to the page.
    pub one_click_url: String,
}

#[derive(sqlx::FromRow)]
struct Scheduled {
    #[sqlx(flatten)]
    link: ReportLink,
    timezone: String,
}

/// Each recipient's copy that is due at `now` and not sent yet. deliver() makes sure only one backend sends it.
/// emailed_period marks a link whose period is done, so later checks skip it cheaply.
pub async fn due(state: &AppState, now: Ts) -> anyhow::Result<Vec<ReportEmail>> {
    let scheduled: Vec<Scheduled> = sqlx::query_as(
        "select l.*, coalesce(ps.timezone, 'UTC') as timezone from report_links l left join project_settings ps on ps.project_id = l.project_id
         where l.email_schedule <> 'none' and l.revoked_at is null and (l.expires_at is null or l.expires_at > $1)",
    )
    .bind(now)
    .fetch_all(&state.db)
    .await?;
    let mut out = vec![];
    for Scheduled { link, timezone } in scheduled {
        let tz: Tz = timezone.parse().unwrap_or(Tz::UTC);
        let Some((from, to)) = period(&link.email_schedule, now.with_timezone(&tz).naive_local()) else { continue };
        if link.emailed_period == Some(from) {
            continue;
        }
        let finished = || sqlx::query("update report_links set emailed_period = $2 where id = $1").bind(link.id).bind(from).execute(&state.db);
        let numbers = analytics::analytics(&state.db, link.project_id, &AnalyticsQuery { from: Some(from), to: Some(to), ..Default::default() }).await?;
        if quiet(&numbers.totals) {
            finished().await?;
            continue;
        }
        let waiting: Vec<(Uuid, String)> = sqlx::query_as(
            "select r.id, r.email from report_recipients r where r.link_id = $1 and r.unsubscribed_at is null
               and not exists (select 1 from report_deliveries d where d.recipient_id = r.id and d.period = $2 and d.sent_at is not null)
             order by r.created_at",
        )
        .bind(link.id)
        .bind(from)
        .fetch_all(&state.db)
        .await?;
        if waiting.is_empty() {
            finished().await?;
            continue;
        }
        let recipients = waiting;
        let brand = brand(&state.db, &link).await?;
        let booked = if link.show_leads { meetings(&state.db, link.project_id, from, to, tz).await? } else { vec![] };
        for (id, to_email) in recipients {
            let secret = &state.cfg.app_secret;
            let page_url = format!("{}/report/{}", state.cfg.public_url, recipient_token(secret, VIEW, id));
            let unsubscribe = recipient_token(secret, UNSUBSCRIBE, id);
            let unsubscribe_url = format!("{}/report-unsubscribe/{unsubscribe}", state.cfg.public_url);
            let one_click_url = format!("{}/api/report-unsubscribe/{unsubscribe}", state.cfg.public_url);
            let (subject, text, html) = compose(&brand, &link, &numbers, &booked, &page_url, &unsubscribe_url);
            out.push(ReportEmail { link_id: link.id, recipient_id: id, period: from, org_id: link.org_id, to: to_email, subject, text, html, one_click_url });
        }
    }
    Ok(out)
}

fn quiet(t: &Totals) -> bool {
    t.sent == 0 && t.replies == 0 && t.meetings == 0 && t.deals_won == 0
}

/// "Nov 2 – 8, 2026" for a week, "October 2026" for a whole month.
fn period_text(from: NaiveDate, to: NaiveDate) -> String {
    if from.day() == 1 && (to + Duration::days(1)).day() == 1 && from.month() == to.month() {
        return from.format("%B %Y").to_string();
    }
    let end = if from.month() == to.month() { to.format("%-d, %Y") } else { to.format("%b %-d, %Y") };
    format!("{} – {end}", from.format("%b %-d"))
}

/// A headline number and how it moved since the period before.
struct Line {
    label: &'static str,
    value: String,
    change: String,
}

fn count_line(label: &'static str, cur: i64, prev: i64, before: &str) -> Line {
    let change = match cur.cmp(&prev) {
        std::cmp::Ordering::Equal => format!("same as {before}"),
        std::cmp::Ordering::Greater => format!("up from {prev} {before}"),
        std::cmp::Ordering::Less => format!("down from {prev} {before}"),
    };
    Line { label, value: cur.to_string(), change }
}

fn usd(micros: i64) -> String {
    format!("${:.2}", micros as f64 / 1_000_000.0)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The subject, the plain text and the HTML of one recipient's email.
pub fn compose(brand: &Brand, link: &ReportLink, a: &Analytics, booked: &[Person], page_url: &str, unsubscribe_url: &str) -> (String, String, String) {
    let weekly = link.email_schedule == "weekly";
    let before = if weekly { "the week before" } else { "the month before" };
    let when = period_text(a.from, a.to);
    let title = if link.title.is_empty() { format!("{} outreach report", brand.name) } else { link.title.clone() };
    let subject = format!("{title}: {when}");
    let (t, p) = (&a.totals, &a.previous);
    let mut lines = vec![
        count_line("Emails sent", t.sent, p.sent, before),
        count_line("Replies", t.replies, p.replies, before),
        count_line("Hot leads", t.hot, p.hot, before),
        count_line("Meetings booked", t.meetings, p.meetings, before),
    ];
    if link.show_spend {
        lines.push(Line { label: "Spend", value: usd(t.spend_micros), change: format!("{} {before}", usd(p.spend_micros)) });
    }
    // The campaign with the most hot leads, then replies. Leads outside any campaign aren't one to name.
    let top = a.campaigns.iter().filter(|c| c.segment_id.is_some() && c.replies > 0).max_by_key(|c| (c.hot, c.replies, c.sent));
    let top = top.map(|c| {
        let hot = if c.hot == 1 { "1 hot lead".to_string() } else { format!("{} hot leads", c.hot) };
        let replies = if c.replies == 1 { "1 reply".to_string() } else { format!("{} replies", c.replies) };
        (super::no_addresses(&c.name), format!("{replies} and {hot} from {} emails", c.sent))
    });
    let names: Vec<String> = booked
        .iter()
        .take(LISTED_MEETINGS)
        .map(|m| if m.company.is_empty() { m.first_name.clone() } else { format!("{} at {}", m.first_name, m.company) })
        .collect();

    let mut text = format!("{title}\n{when}\n\n");
    if !link.intro.is_empty() {
        text += &format!("{}\n\n", link.intro);
    }
    for l in &lines {
        text += &format!("{}: {}, {}\n", l.label, l.value, l.change);
    }
    if let Some((name, detail)) = &top {
        text += &format!("\nTop campaign: {name}, {detail}\n");
    }
    if !names.is_empty() {
        text += &format!("\nMeetings booked:\n{}\n", names.iter().map(|n| format!("- {n}")).collect::<Vec<_>>().join("\n"));
    }
    text += &format!("\nSee the full report: {page_url}\n\nTo stop getting this report, unsubscribe: {unsubscribe_url}\n");

    let cell = "padding:10px 0;border-top:1px solid #e7e5e4";
    let rows: String = lines
        .iter()
        .map(|l| {
            format!(
                "<tr><td style=\"{cell};color:#57534e\">{}</td><td style=\"{cell};text-align:right;font-weight:600;font-size:18px\">{}</td></tr>\
                 <tr><td colspan=\"2\" style=\"padding:0 0 10px;text-align:right;color:#78716c;font-size:13px\">{}</td></tr>",
                l.label,
                escape(&l.value),
                escape(&l.change)
            )
        })
        .collect();
    let logo = if brand.logo_url.is_empty() {
        String::new()
    } else {
        format!("<img src=\"{}\" width=\"32\" height=\"32\" alt=\"\" style=\"border-radius:6px;vertical-align:middle;margin-right:10px\">", escape(&brand.logo_url))
    };
    let intro = if link.intro.is_empty() { String::new() } else { format!("<p style=\"margin:16px 0 0;color:#44403c\">{}</p>", escape(&link.intro)) };
    let top_html = top.map_or(String::new(), |(name, detail)| {
        format!("<h2 style=\"margin:28px 0 6px;font-size:15px\">Top campaign</h2><p style=\"margin:0;color:#44403c\"><b>{}</b>, {}</p>", escape(&name), escape(&detail))
    });
    let meetings_html = if names.is_empty() {
        String::new()
    } else {
        let items: String = names.iter().map(|n| format!("<li style=\"margin:2px 0\">{}</li>", escape(n))).collect();
        format!("<h2 style=\"margin:28px 0 6px;font-size:15px\">Meetings booked</h2><ul style=\"margin:0;padding-left:20px;color:#44403c\">{items}</ul>")
    };
    let html = format!(
        "<!doctype html><html><body style=\"margin:0;padding:24px 12px;background:#f5f5f4;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif;color:#1c1917\">\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"max-width:520px;margin:0 auto;background:#fff;border-radius:12px\">\
         <tr><td style=\"padding:28px 28px 32px\">\
         <p style=\"margin:0;font-size:14px;color:#57534e\">{logo}{}</p>\
         <h1 style=\"margin:16px 0 4px;font-size:22px;line-height:1.3\">{}</h1>\
         <p style=\"margin:0;color:#78716c;font-size:14px\">{}</p>{intro}\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"margin-top:20px;font-size:15px\">{rows}</table>\
         {top_html}{meetings_html}\
         <p style=\"margin:32px 0 0\"><a href=\"{}\" style=\"display:inline-block;background:#1c1917;color:#fff;text-decoration:none;padding:12px 20px;border-radius:8px;font-weight:600\">See the full report</a></p>\
         </td></tr></table>\
         <p style=\"max-width:520px;margin:16px auto 0;font-size:12px;color:#78716c;text-align:center\">{} sends you this report.<br>\
         <a href=\"{}\" style=\"color:#78716c\">Unsubscribe from this report</a></p>\
         </body></html>",
        escape(&brand.name),
        escape(&title),
        escape(&when),
        escape(page_url),
        escape(&brand.org),
        escape(unsubscribe_url),
    );
    (subject, text, html)
}
