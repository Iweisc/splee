//! Open and click tracking for campaign emails. The sender rewrites each link to {base}/c/{token} and, when the campaign
//! tracks opens, adds a pixel at {base}/o/{token}.gif. The base is the sending domain's verified tracking host, like
//! track.acme.com, else PUBLIC_URL. Mail security scanners open and click everything, so every hit is stored with why it
//! was filtered out, when it was, and only the rest count in analytics, the score and the lead's timeline.

use std::net::IpAddr;
use std::time::Duration;

use reqwest::Url;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::Config;
use crate::crypto::random_token;
use crate::models::{EmailMessage, Lead, Mailbox, Ts};
use crate::state::AppState;
use crate::visitors::Kind;

/// A 1x1 transparent GIF.
pub const PIXEL: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff\x21\xf9\x04\x01\x00\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00\x3b";

/// A scanner that opens an email usually does so within seconds of delivery. A person rarely does.
const TOO_SOON_SECONDS: i64 = 60;
/// Clicks on two different links of one email this close together are a scanner following every link.
pub const BURST: Duration = Duration::from_secs(2);

/// The kinds that get tracking: the campaign's first email and its follow-ups. Replies go out like any other message.
const TRACKED_KINDS: [&str; 2] = ["initial", "follow_up"];

/// Mail clients that only ever show your own domain to the people you email. Their tracking host can't be set up.
const FREE_MAIL: [&str; 14] = [
    "gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "live.com", "msn.com", "yahoo.com", "icloud.com", "me.com", "aol.com", "proton.me",
    "protonmail.com", "gmx.com", "zoho.com",
];

pub fn own_domain(domain: &str) -> bool {
    !domain.is_empty() && !FREE_MAIL.contains(&domain)
}

/// Agents of mail security scanners, link previews and scripts. A person's mail client or browser has none of these words.
const SCANNER_AGENTS: [&str; 34] = [
    "bot", "crawler", "spider", "preview", "scanner", "headless", "phantomjs", "python-requests", "python-urllib", "curl/", "wget/", "go-http-client",
    "java/", "okhttp", "libwww", "httpclient", "axios/", "node-fetch", "barracuda", "proofpoint", "mimecast", "symantec", "messagelabs", "trendmicro",
    "fireeye", "forcepoint", "zscaler", "sophos", "ironport", "fortinet", "facebookexternalhit", "whatsapp", "embedly", "google-safety",
];

/// The published outbound ranges of the big mail security gateways: Microsoft Exchange Online Protection, Proofpoint,
/// Mimecast and Barracuda. Their link checks come from here. Scanners in ordinary clouds are caught by the network lookup.
const SECURITY_RANGES: [&str; 19] = [
    "40.92.0.0/15",
    "40.107.0.0/16",
    "52.100.0.0/14",
    "104.47.0.0/17",
    "2a01:111:f400::/48",
    "2a01:111:f403::/48",
    "67.231.144.0/20",
    "148.163.128.0/19",
    "205.220.160.0/19",
    "185.132.180.0/22",
    "205.139.110.0/24",
    "207.211.30.0/24",
    "207.211.31.0/25",
    "170.10.128.0/21",
    "185.58.84.0/22",
    "91.220.42.0/24",
    "146.101.78.0/24",
    "64.235.144.0/20",
    "209.222.80.0/21",
];

fn in_range(ip: IpAddr, cidr: &str) -> bool {
    let Some((net, bits)) = cidr.split_once('/') else { return false };
    let (Ok(net), Ok(bits)) = (net.parse::<IpAddr>(), bits.parse::<u32>()) else { return false };
    match (ip, net) {
        (IpAddr::V4(ip), IpAddr::V4(net)) => bits <= 32 && (u32::from(ip) ^ u32::from(net)).checked_shr(32 - bits).unwrap_or(0) == 0,
        (IpAddr::V6(ip), IpAddr::V6(net)) => bits <= 128 && (u128::from(ip) ^ u128::from(net)).checked_shr(128 - bits).unwrap_or(0) == 0,
        _ => false,
    }
}

/// What the request looked like, for the filters.
pub struct Seen<'a> {
    pub head: bool,
    pub user_agent: &'a str,
    pub ip: Option<IpAddr>,
    pub network: Kind,
    pub at: Ts,
}

/// Why a hit isn't a person, or None when it counts. Clicks in a burst are found later, against the email's other clicks.
pub fn filter_reason(seen: &Seen, sent_at: Option<Ts>) -> Option<&'static str> {
    let agent = seen.user_agent.to_ascii_lowercase();
    if seen.head {
        Some("head_request")
    } else if sent_at.is_some_and(|sent| (seen.at - sent).num_seconds() < TOO_SOON_SECONDS) {
        Some("too_soon")
    } else if agent.trim().is_empty() || SCANNER_AGENTS.iter().any(|s| agent.contains(s)) {
        Some("scanner_agent")
    } else if seen.ip.is_some_and(|ip| SECURITY_RANGES.iter().any(|r| in_range(ip, r))) {
        Some("security_gateway")
    } else if seen.network == Kind::Hosting {
        Some("cloud")
    } else {
        None
    }
}

/// Where a link ends: at a space, a quote or an angle bracket, without the punctuation that ends the sentence around it.
fn link_end(s: &str) -> usize {
    let mut end = s.find(|c: char| c.is_whitespace() || "<>\"'`".contains(c)).unwrap_or(s.len());
    while let Some(last) = s[..end].chars().last() {
        let url = &s[..end];
        let trailing = match last {
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            ')' => url.matches('(').count() < url.matches(')').count(),
            ']' => url.matches('[').count() < url.matches(']').count(),
            _ => false,
        };
        if !trailing {
            break;
        }
        end -= last.len_utf8();
    }
    end
}

/// The http(s) links in `body` as byte ranges, each with whether it's in the quoted thread: the lines from the first
/// "On ... wrote:" or "-----Original Message-----" on, and lines starting with ">".
fn find_links(body: &str) -> Vec<(usize, usize, bool)> {
    let lower = body.to_ascii_lowercase();
    let mut found = vec![];
    let (mut offset, mut quoted, mut previous) = (0, false, (0, ""));
    for line in body.split_inclusive('\n') {
        let t = line.trim();
        if !quoted && (t.starts_with("-----Original Message-----") || (t.starts_with("On ") && t.ends_with("wrote:"))) {
            quoted = true;
        }
        // Gmail wraps a long "On <date> <name> <address> wrote:" line, so only its last piece ends with "wrote:".
        if !quoted && t.ends_with("wrote:") && previous.1.trim().starts_with("On ") {
            quoted = true;
            found.retain(|(start, _, _)| *start < previous.0);
        }
        let in_quote = quoted || t.starts_with('>');
        let mut at = offset;
        let end_of_line = offset + line.len();
        while let Some(i) = ["https://", "http://"].iter().filter_map(|p| lower[at..end_of_line].find(p)).min() {
            let start = at + i;
            let end = start + link_end(&body[start..end_of_line]);
            if end > start + 8 {
                found.push((start, end, in_quote));
            }
            at = end.max(start + 7);
        }
        previous = (offset, line);
        offset = end_of_line;
    }
    found
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn escape_lines(text: &str) -> String {
    escape(text).replace("\r\n", "\n").replace('\n', "<br>\n")
}

pub struct Link {
    pub token: String,
    pub url: String,
}

/// The email with its links tracked: the plain text, an HTML version whose links show where they go, and the tokens to keep.
pub struct Rewritten {
    pub text: String,
    pub html: String,
    pub links: Vec<Link>,
}

/// Points each http(s) link outside the quoted thread at {base}/c/{token}, except those `left_alone` picks out. Without
/// `track`, nothing changes and the HTML just links the links.
pub fn rewrite(body: &str, base: &str, track: bool, left_alone: impl Fn(&str) -> bool) -> Rewritten {
    let (mut text, mut html, mut links) = (String::with_capacity(body.len()), String::new(), vec![]);
    let mut last = 0;
    for (start, end, quoted) in find_links(body) {
        let url = &body[start..end];
        let parsed = Url::parse(url).ok().filter(|u| u.host_str().is_some());
        let tracked = match parsed {
            Some(parsed) if track && !quoted && !left_alone(url) => {
                let token = random_token();
                let tracked = format!("{base}/c/{token}");
                links.push(Link { token, url: parsed.to_string() });
                Some(tracked)
            }
            _ => None,
        };
        text.push_str(&body[last..start]);
        text.push_str(tracked.as_deref().unwrap_or(url));
        html.push_str(&escape_lines(&body[last..start]));
        html.push_str(&format!("<a href=\"{}\">{}</a>", escape(tracked.as_deref().unwrap_or(url)), escape(url)));
        last = end;
    }
    text.push_str(&body[last..]);
    html.push_str(&escape_lines(&body[last..]));
    Rewritten { text, html, links }
}

/// The HTML part: the text, and the pixel when there is one.
pub fn html_document(fragment: &str, pixel: Option<&str>) -> String {
    let img = pixel.map(|src| format!("<img src=\"{}\" width=\"1\" height=\"1\" alt=\"\" style=\"display:block;border:0;width:1px;height:1px\">", escape(src)));
    format!("<!doctype html>\n<html><body><div style=\"font-family:sans-serif;font-size:14px;line-height:1.5\">{fragment}</div>{}</body></html>", img.unwrap_or_default())
}

/// Links that keep their own address: the unsubscribe page, which must work without Splee in the way.
fn left_alone(url: &str, cfg: &Config) -> bool {
    let unsub = cfg.unsubscribe_url.trim_end_matches('/');
    [format!("{unsub}/unsubscribe/"), format!("{unsub}/api/unsubscribe/")].iter().any(|prefix| url.starts_with(prefix.as_str()))
}

/// Whether the email gets (click, open) tracking: its campaign's settings for first emails and follow-ups, and the
/// defaults for those outside a campaign.
async fn settings(db: &PgPool, email: &EmailMessage) -> sqlx::Result<(bool, bool)> {
    if !TRACKED_KINDS.contains(&email.kind.as_str()) {
        return Ok((false, false));
    }
    let set: Option<(bool, bool)> =
        sqlx::query_as("select track_clicks, track_opens from segments where id = $1").bind(email.segment_id).fetch_optional(db).await?;
    Ok(set.unwrap_or((true, false)))
}

/// Where the mailbox's tracked links point: its domain's verified tracking host, else PUBLIC_URL.
pub async fn base_for(db: &PgPool, public_url: &str, mb: &Mailbox) -> sqlx::Result<String> {
    let domain = mb.email.rsplit('@').next().unwrap_or_default().to_lowercase();
    let host: Option<String> = sqlx::query_scalar("select host from tracking_domains where org_id = $1 and domain = $2 and verified_at is not null")
        .bind(mb.org_id)
        .bind(domain)
        .fetch_optional(db)
        .await?;
    Ok(host.map_or_else(|| public_url.trim_end_matches('/').to_string(), |h| format!("https://{h}")))
}

pub struct Prepared {
    pub text: String,
    pub html: Option<String>,
}

/// Tracks the email's links and adds its pixel, as its campaign says, and keeps the tokens. `body` is the email as sent,
/// footer included. Plain-text emails get click tracking only: a pixel needs an HTML part, which opens add.
pub async fn prepare(state: &AppState, email: &EmailMessage, mb: &Mailbox, body: &str) -> anyhow::Result<Prepared> {
    let (clicks, opens) = settings(&state.db, email).await?;
    let base = if clicks || opens { base_for(&state.db, &state.cfg.public_url, mb).await? } else { String::new() };
    let r = rewrite(body, &base, clicks, |url| left_alone(url, &state.cfg));
    let open_token = opens.then(random_token);
    let mut tx = state.db.begin().await?;
    // A send tried again after a deferral gets new tokens, and the old ones never went out.
    sqlx::query("delete from tracked_links where email_id = $1").bind(email.id).execute(&mut *tx).await?;
    sqlx::query("insert into tracked_links (token, email_id, position, url) select t, $1, p - 1, u from unnest($2::text[], $3::text[]) with ordinality as x(t, u, p)")
        .bind(email.id)
        .bind(r.links.iter().map(|l| l.token.clone()).collect::<Vec<_>>())
        .bind(r.links.iter().map(|l| l.url.clone()).collect::<Vec<_>>())
        .execute(&mut *tx)
        .await?;
    sqlx::query("update email_messages set track_clicks = $2, track_opens = $3, open_token = $4 where id = $1")
        .bind(email.id)
        .bind(clicks)
        .bind(opens)
        .bind(&open_token)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let html = open_token.map(|token| html_document(&r.html, Some(&format!("{base}/o/{token}.gif"))));
    Ok(Prepared { text: r.text, html })
}

/// An open or a click as it arrived.
pub struct Hit {
    /// "open" or "click".
    pub kind: &'static str,
    pub email_id: Uuid,
    pub link_token: Option<String>,
    pub head: bool,
    pub user_agent: String,
    pub ip: Option<IpAddr>,
    pub ip_hash: String,
    pub at: Ts,
}

/// Stores the hit with why it doesn't count, if it doesn't, and files it and the clicks next to it under link_burst when
/// two different links were clicked within BURST. Returns its id. The tracking routes do the same in three steps, with
/// the network lookup and a wait in between.
#[cfg(test)]
pub async fn record(db: &PgPool, hit: &Hit, network: Kind) -> anyhow::Result<Option<Uuid>> {
    let Some(id) = begin(db, hit).await? else { return Ok(None) };
    finish(db, id, hit, network).await?;
    burst(db, id).await?;
    Ok(Some(id))
}

/// Stores the hit as pending, which nothing counts, as soon as it comes in. A click that came with it is then in the table
/// when either one's `burst` runs, however long their network lookups take.
pub async fn begin(db: &PgPool, hit: &Hit) -> anyhow::Result<Option<Uuid>> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "insert into tracking_hits (project_id, email_id, lead_id, kind, link_token, filter_reason, ip_hash, user_agent, created_at)
         select project_id, id, lead_id, $2, $3, 'pending', $4, $5, $6 from email_messages where id = $1 returning id",
    )
    .bind(hit.email_id)
    .bind(hit.kind)
    .bind(&hit.link_token)
    .bind(&hit.ip_hash)
    .bind(hit.user_agent.chars().take(300).collect::<String>())
    .bind(hit.at)
    .fetch_optional(db)
    .await?;
    Ok(id)
}

/// Gives the pending hit its own reason not to count, or none, once the network it came from is known.
pub async fn finish(db: &PgPool, id: Uuid, hit: &Hit, network: Kind) -> anyhow::Result<()> {
    let sent_at: Option<Ts> = sqlx::query_scalar("select sent_at from email_messages where id = $1").bind(hit.email_id).fetch_one(db).await?;
    let seen = Seen { head: hit.head, user_agent: &hit.user_agent, ip: hit.ip, network, at: hit.at };
    sqlx::query("update tracking_hits set filter_reason = $2 where id = $1 and filter_reason = 'pending'")
        .bind(id)
        .bind(filter_reason(&seen, sent_at))
        .execute(db)
        .await?;
    Ok(())
}

/// Files a counted click, and the counted clicks next to it, under link_burst when two different links were clicked within
/// BURST. Clicks still pending count as next to it, so a slow lookup can't let a burst through. Run it BURST after the
/// click came in, so the ones after it are in the table too.
pub async fn burst(db: &PgPool, id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "with me as (select email_id, created_at from tracking_hits where id = $1 and kind = 'click' and filter_reason is null),
         near as (
           select h.id, h.link_token from tracking_hits h, me
           where h.email_id = me.email_id and h.kind = 'click' and (h.filter_reason is null or h.filter_reason in ('link_burst', 'pending'))
             and h.created_at between me.created_at - make_interval(secs => $2) and me.created_at + make_interval(secs => $2))
         update tracking_hits set filter_reason = 'link_burst'
         where id in (select id from near) and filter_reason is null and (select count(distinct link_token) from near) >= 2",
    )
    .bind(id)
    .bind(BURST.as_secs_f64())
    .execute(db)
    .await?;
    Ok(())
}

/// Sends email.opened for an email's first counted open and email.clicked for the first counted click on each of its
/// links. Run it after `burst`, so a click that turned out to be part of one says nothing.
pub async fn announce(state: &AppState, hit_id: Uuid) -> anyhow::Result<()> {
    #[derive(sqlx::FromRow)]
    struct Counted {
        kind: String,
        project_id: Uuid,
        email_id: Uuid,
        url: Option<String>,
        subject: String,
        step: i32,
        at: Ts,
    }
    let row: Option<Counted> = sqlx::query_as(
        "select h.kind, h.project_id, h.email_id, k.url, e.subject, e.step, h.created_at as at
         from tracking_hits h join email_messages e on e.id = h.email_id left join tracked_links k on k.token = h.link_token
         where h.id = $1 and h.filter_reason is null
           and not exists (select 1 from tracking_hits o where o.email_id = h.email_id and o.kind = h.kind and o.filter_reason is null
                           and o.link_token is not distinct from h.link_token and (o.created_at, o.id) < (h.created_at, h.id))",
    )
    .bind(hit_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(Counted { kind, project_id, email_id, url, subject, step, at }) = row else { return Ok(()) };
    let lead = sqlx::query_as::<_, Lead>("select l.* from leads l join tracking_hits h on h.lead_id = l.id where h.id = $1").bind(hit_id).fetch_one(&state.db).await?;
    let event = if kind == "open" { "email.opened" } else { "email.clicked" };
    let detail = json!({ "id": email_id, "subject": subject, "step": step, "url": url, "at": at });
    crate::hooks::send_lead_event(state, project_id, event, &lead, Some(("email", detail))).await;
    Ok(())
}

/// "pricing" for https://acme.com/pricing, and the host for a home page: how the score and the timeline name a link.
pub fn link_name(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else { return url.to_string() };
    let last = parsed.path_segments().and_then(|mut s| s.rfind(|p| !p.is_empty())).map(|p| p.replace(['-', '_'], " "));
    let host = parsed.host_str().unwrap_or_default().trim_start_matches("www.").to_string();
    last.filter(|p| !p.contains('.')).unwrap_or(host).to_lowercase()
}

/// The host and path of a link without its query, like acme.com/pricing.
pub fn link_label(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else { return url.to_string() };
    let host = parsed.host_str().unwrap_or_default().trim_start_matches("www.");
    format!("{host}{}", parsed.path().trim_end_matches('/'))
}

/// What a tracking host's CNAME points at: PUBLIC_URL's host, unless it's a name DNS can't point to.
pub fn cname_target(public_url: &str) -> Option<String> {
    let host = Url::parse(public_url).ok()?.host_str()?.to_lowercase();
    let local = host == "localhost" || host.ends_with(".localhost") || host.parse::<IpAddr>().is_ok() || host.starts_with('[') || !host.contains('.');
    (!local).then_some(host)
}

/// The DNS answers a tracking host check needs, so tests can stand in for DNS.
pub(crate) trait Resolve {
    async fn cnames(&self, name: &str) -> Vec<String>;
    async fn addresses(&self, name: &str) -> Vec<IpAddr>;
}

impl Resolve for hickory_resolver::TokioResolver {
    async fn cnames(&self, name: &str) -> Vec<String> {
        let Ok(lookup) = self.lookup(format!("{}.", name.trim_end_matches('.')), hickory_resolver::proto::rr::RecordType::CNAME).await else { return vec![] };
        lookup
            .answers()
            .iter()
            .filter_map(|r| match &r.data {
                hickory_resolver::proto::rr::RData::CNAME(c) => Some(c.0.to_string()),
                _ => None,
            })
            .collect()
    }

    async fn addresses(&self, name: &str) -> Vec<IpAddr> {
        self.lookup_ip(format!("{}.", name.trim_end_matches('.'))).await.map(|l| l.iter().collect()).unwrap_or_default()
    }
}

/// Ok when `host` is a CNAME to `target`, or, behind DNS that flattens CNAMEs like Cloudflare's, resolves to its addresses.
pub(crate) async fn check_host(r: &impl Resolve, host: &str, target: &str) -> Result<(), String> {
    let plain = |n: &str| n.trim_end_matches('.').to_lowercase();
    let cnames = r.cnames(host).await;
    if cnames.iter().any(|c| plain(c) == plain(target)) {
        return Ok(());
    }
    if let Some(other) = cnames.first() {
        return Err(format!("{host} points to {}, not {target}", plain(other)));
    }
    let (theirs, ours) = (r.addresses(host).await, r.addresses(target).await);
    if !theirs.is_empty() && theirs.iter().all(|a| ours.contains(a)) {
        return Ok(());
    }
    Err(format!("No CNAME record for {host} yet. Add one pointing to {target}"))
}

/// Checks a tracking host now and keeps how it went. It stays verified from its first pass until a check fails.
pub async fn check_and_save(db: &PgPool, r: &impl Resolve, org_id: Uuid, domain: &str, target: &str) -> sqlx::Result<()> {
    let host = format!("track.{domain}");
    let result = check_host(r, &host, target).await;
    sqlx::query(
        "insert into tracking_domains (org_id, domain, host, verified_at, checked_at, error)
         values ($1, $2, $3, case when $4 then now() end, now(), $5)
         on conflict (org_id, domain) do update set host = excluded.host, checked_at = now(), error = excluded.error,
           verified_at = case when $4 then coalesce(tracking_domains.verified_at, now()) end",
    )
    .bind(org_id)
    .bind(domain)
    .bind(&host)
    .bind(result.is_ok())
    .bind(result.err().unwrap_or_default())
    .execute(db)
    .await?;
    Ok(())
}

/// Every 10 minutes: tracking hosts waiting for DNS, like the ones set on Porkbun, for a week, and verified ones once a day
/// so one whose CNAME was removed stops being used.
pub async fn recheck_hosts(state: &AppState) -> anyhow::Result<()> {
    let Some(target) = cname_target(&state.cfg.public_url) else { return Ok(()) };
    let due: Vec<(Uuid, String)> = sqlx::query_as(
        "select org_id, domain from tracking_domains
         where (verified_at is null and created_at > now() - interval '7 days' and (checked_at is null or checked_at < now() - interval '10 minutes'))
            or (verified_at is not null and checked_at < now() - interval '1 day')
         limit 100",
    )
    .fetch_all(&state.db)
    .await?;
    if due.is_empty() {
        return Ok(());
    }
    let resolver = crate::mail::dns::resolver()?;
    for (org_id, domain) in due {
        check_and_save(&state.db, &resolver, org_id, &domain, &target).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
