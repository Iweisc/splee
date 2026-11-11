//! Website visitors: the snippet a project adds to its site, the page views it sends, and the companies behind them. Each IP
//! is looked up once in 30 days, by an HMAC of it with APP_SECRET, which is all Splee keeps of it: through IPinfo when the
//! operator sets IPINFO_TOKEN, else its reverse DNS name and the RDAP record of its network. Home and mobile connections,
//! clouds, hosting and VPNs are dropped, so only companies' own networks show.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::hooks::{self, ChatAlert};
use crate::routes::util_input;
use crate::state::AppState;

/// Served as /api/v/{key}.js. It posts to the same address with /hit in place of .js, so it needs nothing filled in. It sends
/// nothing when the browser asks not to be tracked, keeps its visitor id in the site's own localStorage, and sets no cookie.
/// Single-page apps send a view for each pushState too.
pub const SNIPPET: &str = r#"(function(){var n=navigator,w=window,d=document,l=location,s=d.currentScript,o;
if(!s||n.doNotTrack=="1"||w.doNotTrack=="1"||n.globalPrivacyControl||!n.sendBeacon)return;
var u=s.src.replace(/\.js(\?.*)?$/,"/hit"),v="";
try{v=localStorage.getItem("splee_vid")||"";if(!v){v=w.crypto&&crypto.randomUUID?crypto.randomUUID():Math.random().toString(36).slice(2)+Date.now().toString(36);localStorage.setItem("splee_vid",v)}}catch(e){}
function h(){var p=l.pathname+l.search;if(p==o)return;o=p;var b={path:l.pathname,title:d.title,referrer:d.referrer,visitor:v};
new URLSearchParams(l.search).forEach(function(x,k){if(/^utm_(source|medium|campaign|term|content)$/.test(k))b[k]=x});
n.sendBeacon(u,JSON.stringify(b))}
h();var q=history.pushState;history.pushState=function(){q.apply(history,arguments);setTimeout(h,100)};w.addEventListener("popstate",h)})();
"#;

pub const MAX_BODY: usize = 4096;
pub const RETENTION_DAYS: i32 = 180;
const RECHECK_DAYS: i32 = 30;
/// Page views a company reads in a day before a high-intent page alerts even when it isn't in a campaign yet.
pub const ALERT_VIEWS: i64 = 3;
const TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Business,
    Isp,
    Mobile,
    Hosting,
    Vpn,
    Unknown,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Business => "business",
            Kind::Isp => "isp",
            Kind::Mobile => "mobile",
            Kind::Hosting => "hosting",
            Kind::Vpn => "vpn",
            Kind::Unknown => "unknown",
        }
    }

    fn parse(kind: &str) -> Kind {
        [Kind::Business, Kind::Isp, Kind::Mobile, Kind::Hosting, Kind::Vpn].into_iter().find(|k| k.as_str() == kind).unwrap_or(Kind::Unknown)
    }
}

/// Who an IP belongs to. A business can lack a domain when its network's records name it without one.
#[derive(Debug, Clone, PartialEq)]
pub struct Network {
    pub kind: Kind,
    pub org: String,
    pub domain: Option<String>,
    pub source: &'static str,
}

impl Network {
    fn none() -> Self {
        Network { kind: Kind::Unknown, org: String::new(), domain: None, source: "none" }
    }
}

// Matched against whole words of the network's name, so "cloud" doesn't catch Cloudinary. A business whose own name has one
// of these words is dropped too, which costs less than showing every home broadband user.
const NAME_WORDS: &[(&str, Kind)] = &[
    ("vpn", Kind::Vpn),
    ("proxy", Kind::Vpn),
    ("tor exit", Kind::Vpn),
    ("mobile", Kind::Mobile),
    ("wireless", Kind::Mobile),
    ("cellular", Kind::Mobile),
    ("hosting", Kind::Hosting),
    ("cloud", Kind::Hosting),
    ("datacenter", Kind::Hosting),
    ("data center", Kind::Hosting),
    ("colocation", Kind::Hosting),
    ("vps", Kind::Hosting),
    ("servers", Kind::Hosting),
    ("telecom", Kind::Isp),
    ("telekom", Kind::Isp),
    ("telecommunications", Kind::Isp),
    ("telecommunication", Kind::Isp),
    ("communications", Kind::Isp),
    ("broadband", Kind::Isp),
    ("cable", Kind::Isp),
    ("fiber", Kind::Isp),
    ("fibre", Kind::Isp),
    ("internet service", Kind::Isp),
    ("internet services", Kind::Isp),
    ("isp", Kind::Isp),
    ("dsl", Kind::Isp),
];

// Common carriers, clouds and VPNs whose names don't say what they are, as whole words of the name. Brands that are also
// common words, like Orange, Three or Sky, are caught by their domains, their AS numbers or the words above instead.
const NAMES: &[(&str, Kind)] = &[
    ("nordvpn", Kind::Vpn),
    ("expressvpn", Kind::Vpn),
    ("mullvad", Kind::Vpn),
    ("surfshark", Kind::Vpn),
    ("private internet access", Kind::Vpn),
    ("cyberghost", Kind::Vpn),
    ("ipvanish", Kind::Vpn),
    ("windscribe", Kind::Vpn),
    ("packethub", Kind::Vpn),
    ("m247", Kind::Vpn),
    ("zscaler", Kind::Vpn),
    ("icloud private relay", Kind::Vpn),
    ("t mobile", Kind::Mobile),
    ("vodafone", Kind::Mobile),
    ("jio", Kind::Mobile),
    ("airtel", Kind::Mobile),
    ("china mobile", Kind::Mobile),
    ("amazon", Kind::Hosting),
    ("aws", Kind::Hosting),
    ("google", Kind::Hosting),
    ("microsoft", Kind::Hosting),
    ("digitalocean", Kind::Hosting),
    ("linode", Kind::Hosting),
    ("akamai", Kind::Hosting),
    ("hetzner", Kind::Hosting),
    ("ovh", Kind::Hosting),
    ("scaleway", Kind::Hosting),
    ("vultr", Kind::Hosting),
    ("choopa", Kind::Hosting),
    ("oracle", Kind::Hosting),
    ("alibaba", Kind::Hosting),
    ("tencent", Kind::Hosting),
    ("cloudflare", Kind::Hosting),
    ("fastly", Kind::Hosting),
    ("leaseweb", Kind::Hosting),
    ("contabo", Kind::Hosting),
    ("hostinger", Kind::Hosting),
    ("godaddy", Kind::Hosting),
    ("ionos", Kind::Hosting),
    ("rackspace", Kind::Hosting),
    ("equinix", Kind::Hosting),
    ("comcast", Kind::Isp),
    ("verizon", Kind::Isp),
    ("at t", Kind::Isp),
    ("centurylink", Kind::Isp),
    ("lumen technologies", Kind::Isp),
    ("windstream", Kind::Isp),
    ("altice", Kind::Isp),
    ("mediacom", Kind::Isp),
    ("starlink", Kind::Isp),
    ("spacex", Kind::Isp),
    ("bell canada", Kind::Isp),
    ("telus", Kind::Isp),
    ("virgin media", Kind::Isp),
    ("british telecommunications", Kind::Isp),
    ("talktalk", Kind::Isp),
    ("orange sa", Kind::Isp),
    ("orange s a", Kind::Isp),
    ("orange polska", Kind::Isp),
    ("sfr", Kind::Isp),
    ("bouygues", Kind::Isp),
    ("free sas", Kind::Isp),
    ("telefonica", Kind::Isp),
    ("movistar", Kind::Isp),
    ("kpn", Kind::Isp),
    ("ziggo", Kind::Isp),
    ("swisscom", Kind::Isp),
    ("proximus", Kind::Isp),
    ("telia", Kind::Isp),
    ("telenor", Kind::Isp),
    ("liberty global", Kind::Isp),
    ("telstra", Kind::Isp),
    ("optus", Kind::Isp),
    ("kddi", Kind::Isp),
    ("softbank", Kind::Isp),
    ("chinanet", Kind::Isp),
    ("china unicom", Kind::Isp),
];

// The network's domain, or any domain under it.
const DOMAINS: &[(&str, Kind)] = &[
    ("amazonaws.com", Kind::Hosting),
    ("amazon.com", Kind::Hosting),
    ("googleusercontent.com", Kind::Hosting),
    ("google.com", Kind::Hosting),
    ("1e100.net", Kind::Hosting),
    ("microsoft.com", Kind::Hosting),
    ("azure.com", Kind::Hosting),
    ("digitalocean.com", Kind::Hosting),
    ("linode.com", Kind::Hosting),
    ("akamai.com", Kind::Hosting),
    ("akamaitechnologies.com", Kind::Hosting),
    ("hetzner.com", Kind::Hosting),
    ("your-server.de", Kind::Hosting),
    ("ovh.net", Kind::Hosting),
    ("ovh.com", Kind::Hosting),
    ("scaleway.com", Kind::Hosting),
    ("vultr.com", Kind::Hosting),
    ("oraclecloud.com", Kind::Hosting),
    ("aliyun.com", Kind::Hosting),
    ("cloudflare.com", Kind::Hosting),
    ("fastly.com", Kind::Hosting),
    ("leaseweb.com", Kind::Hosting),
    ("contabo.com", Kind::Hosting),
    ("comcast.net", Kind::Isp),
    ("comcastbusiness.net", Kind::Isp),
    ("verizon.net", Kind::Isp),
    ("verizon.com", Kind::Isp),
    ("att.net", Kind::Isp),
    ("att.com", Kind::Isp),
    ("sbcglobal.net", Kind::Isp),
    ("charter.com", Kind::Isp),
    ("spectrum.com", Kind::Isp),
    ("rr.com", Kind::Isp),
    ("cox.net", Kind::Isp),
    ("centurylink.net", Kind::Isp),
    ("virginmedia.com", Kind::Isp),
    ("btcentralplus.com", Kind::Isp),
    ("bt.com", Kind::Isp),
    ("sky.com", Kind::Isp),
    ("orange.fr", Kind::Isp),
    ("orange.com", Kind::Isp),
    ("telekom.de", Kind::Isp),
    ("t-ipconnect.de", Kind::Isp),
    ("telefonica.com", Kind::Isp),
    ("rogers.com", Kind::Isp),
    ("t-mobile.com", Kind::Mobile),
    ("tmobile.com", Kind::Mobile),
    ("vodafone.com", Kind::Mobile),
    ("myvzw.com", Kind::Mobile),
    ("nordvpn.com", Kind::Vpn),
    ("expressvpn.com", Kind::Vpn),
    ("mullvad.net", Kind::Vpn),
    ("zscaler.net", Kind::Vpn),
];

// Amazon, Google, Microsoft, DigitalOcean, Linode, Akamai, Hetzner, OVH, Scaleway, Vultr, Oracle, Alibaba, Cloudflare, Fastly,
// Leaseweb, Contabo, M247 and CDN77, by the AS numbers IPinfo gives.
const HOSTING_ASNS: &[u32] =
    &[16509, 14618, 15169, 396982, 8075, 14061, 63949, 20940, 24940, 16276, 12876, 20473, 31898, 45102, 13335, 54113, 60781, 28753, 51167, 9009, 212238];

/// Where abuse contacts of most networks point. Never a visitor's company.
const NOT_COMPANIES: &[&str] =
    &["arin.net", "ripe.net", "apnic.net", "lacnic.net", "afrinic.net", "gmail.com", "yahoo.com", "hotmail.com", "outlook.com", "abuse.net"];

/// Lowercase words, so "AT&T Services, Inc." reads "at t services inc".
fn words(text: &str) -> String {
    let spaced: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    format!(" {} ", spaced.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// What the built-in lists say a network is, from its name, domain or AS number. None when they don't know it.
pub fn listed(name: &str, domain: Option<&str>, asn: Option<u32>) -> Option<Kind> {
    if asn.is_some_and(|a| HOSTING_ASNS.contains(&a)) {
        return Some(Kind::Hosting);
    }
    if let Some(domain) = domain.map(str::to_lowercase) {
        let under = |d: &str| domain == d || domain.ends_with(&format!(".{d}"));
        if let Some((_, kind)) = DOMAINS.iter().find(|(d, _)| under(d)) {
            return Some(*kind);
        }
    }
    let name = words(name);
    NAMES.iter().chain(NAME_WORDS).find(|(needle, _)| name.contains(&format!(" {needle} "))).map(|(_, kind)| *kind)
}

/// A reverse DNS name an ISP generated for a customer, like 203-0-113-5.static.isp.net or dsl-pool7.isp.net, whose domain is
/// the ISP's, not the customer's.
pub fn generated_host(host: &str) -> bool {
    const WORDS: [&str; 14] = ["dsl", "adsl", "dyn", "dynamic", "pool", "dhcp", "cable", "ppp", "broadband", "cust", "customer", "static", "res", "client"];
    let labels: Vec<&str> = host.split(['.', '-']).collect();
    let numbers = labels.iter().filter(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit())).count();
    numbers >= 2 || labels.iter().any(|l| WORDS.iter().any(|w| l.trim_end_matches(|c: char| c.is_ascii_digit()) == *w))
}

/// The registered domain of a host: mail.acme.com is acme.com, and gw.acme.co.uk is acme.co.uk.
pub fn registrable(host: &str) -> Option<String> {
    const SECOND_LEVELS: [&str; 12] = ["co", "com", "net", "org", "ac", "gov", "edu", "ne", "or", "go", "ltd", "plc"];
    let host = host.trim().trim_end_matches('.').to_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    if n < 2 {
        return None;
    }
    let take = if n >= 3 && labels[n - 1].len() == 2 && SECOND_LEVELS.contains(&labels[n - 2]) { 3 } else { 2 };
    util_input::domain(&labels[n.saturating_sub(take)..].join("."))
}

/// rdap.org answers with a redirect to the registry that holds the network. Only those registries are followed, since the
/// redirect comes from the network and could otherwise point the server at its own.
const REGISTRIES: [&str; 6] = ["rdap.org", "rdap.arin.net", "rdap.db.ripe.net", "rdap.apnic.net", "rdap.lacnic.net", "rdap.afrinic.net"];

fn registry_redirect(attempt: reqwest::redirect::Attempt) -> reqwest::redirect::Action {
    let url = attempt.url();
    if attempt.previous().len() < 5 && url.scheme() == "https" && url.host_str().is_some_and(|h| REGISTRIES.contains(&h)) {
        attempt.follow()
    } else {
        attempt.stop()
    }
}

pub struct Lookup {
    client: reqwest::Client,
    ipinfo_url: String,
    ipinfo_token: Option<String>,
    rdap_url: String,
    /// None in tests, and when the system resolver can't be read.
    resolver: Option<TokioResolver>,
}

impl Lookup {
    pub fn new(ipinfo_token: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .user_agent("Splee")
                .redirect(reqwest::redirect::Policy::custom(registry_redirect))
                .build()
                .expect("the HTTP client builds"),
            ipinfo_url: "https://ipinfo.io".into(),
            ipinfo_token,
            rdap_url: "https://rdap.org".into(),
            resolver: crate::mail::dns::resolver().ok(),
        }
    }

    #[cfg(test)]
    pub fn at(base: &str, ipinfo_token: Option<&str>) -> Self {
        Self {
            client: reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap(),
            ipinfo_url: format!("{base}/ipinfo"),
            ipinfo_token: ipinfo_token.map(String::from),
            rdap_url: format!("{base}/rdap"),
            resolver: None,
        }
    }

    /// IPinfo first when there's a token, then reverse DNS and RDAP, which also cover IPinfo failing or being over its quota.
    pub async fn resolve(&self, db: &PgPool, ip: IpAddr) -> Network {
        if let Some(token) = &self.ipinfo_token
            && let Some(network) = self.ipinfo(ip, token).await
        {
            return network;
        }
        self.rdap(db, ip).await.unwrap_or_else(Network::none)
    }

    async fn ipinfo(&self, ip: IpAddr, token: &str) -> Option<Network> {
        let res = self.client.get(format!("{}/{ip}", self.ipinfo_url)).bearer_auth(token).send().await.ok()?;
        if !res.status().is_success() {
            tracing::warn!(status = %res.status(), "IPinfo lookup failed");
            return None;
        }
        Some(from_ipinfo(&res.json::<IpInfo>().await.ok()?))
    }

    async fn rdap(&self, db: &PgPool, ip: IpAddr) -> Option<Network> {
        let (record, host) = tokio::join!(
            async {
                let res = self.client.get(format!("{}/ip/{ip}", self.rdap_url)).header("Accept", "application/rdap+json").send().await.ok()?;
                if !res.status().is_success() {
                    return None;
                }
                res.json::<Value>().await.ok()
            },
            self.ptr(ip)
        );
        let record = record?;
        let org = rdap_org(&record);
        let emails = rdap_email_domains(&record);
        let host_domain = host.as_deref().filter(|h| !generated_host(h)).and_then(registrable);
        let listed_as = std::iter::once(None)
            .chain(emails.iter().map(|d| Some(d.as_str())))
            .chain(host.as_deref().map(Some))
            .find_map(|domain| listed(&org, domain, None));
        if let Some(kind) = listed_as {
            return Some(Network { kind, org, domain: None, source: "rdap" });
        }
        let domain = pick_domain(db, &emails, host_domain.as_deref()).await;
        let kind = if domain.is_some() || !org.is_empty() { Kind::Business } else { Kind::Unknown };
        Some(Network { kind, org, domain, source: "rdap" })
    }

    async fn ptr(&self, ip: IpAddr) -> Option<String> {
        let lookup = self.resolver.as_ref()?.reverse_lookup(ip).await.ok()?;
        lookup.answers().iter().find_map(|r| match &r.data {
            RData::PTR(name) => Some(name.to_string().trim_end_matches('.').to_lowercase()),
            _ => None,
        })
    }
}

/// The reverse DNS name's domain when a contact's domain matches it, else the first contact domain in the company database,
/// else the first contact domain, else the reverse DNS name's.
async fn pick_domain(db: &PgPool, emails: &[String], host: Option<&str>) -> Option<String> {
    if let Some(host) = host
        && emails.iter().any(|d| d == host)
    {
        return Some(host.to_string());
    }
    if !emails.is_empty() {
        let known: Option<String> = sqlx::query_scalar("select domain from directory_companies where domain = any($1) order by array_position($1, domain) limit 1")
            .bind(emails)
            .fetch_optional(db)
            .await
            .unwrap_or_default();
        if known.is_some() {
            return known;
        }
    }
    emails.first().cloned().or(host.map(String::from))
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct IpInfo {
    hostname: String,
    /// "AS15169 Google LLC" on every plan.
    org: String,
    company: Option<IpInfoOrg>,
    asn: Option<IpInfoOrg>,
    privacy: Privacy,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct IpInfoOrg {
    name: String,
    domain: String,
    #[serde(rename = "type")]
    kind: String,
    asn: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Privacy {
    vpn: bool,
    proxy: bool,
    tor: bool,
    relay: bool,
    hosting: bool,
}

/// The company fields come with paid plans, the AS ones with most, and `org` with all of them.
pub fn from_ipinfo(info: &IpInfo) -> Network {
    let p = &info.privacy;
    let (org_asn, org_name) = match info.org.split_once(' ') {
        Some((asn, name)) if asn.starts_with("AS") => (asn.trim_start_matches("AS").parse().ok(), name.to_string()),
        _ => (None, info.org.clone()),
    };
    let asn = info.asn.as_ref().and_then(|a| a.asn.trim_start_matches("AS").parse().ok()).or(org_asn);
    let named = info.company.as_ref().or(info.asn.as_ref()).filter(|o| !o.name.is_empty());
    let org = named.map_or(org_name, |o| o.name.clone());
    let given = named.map(|o| o.domain.to_lowercase()).filter(|d| !d.is_empty());
    let typed = match named.map(|o| o.kind.as_str()) {
        Some("isp") => Some(Kind::Isp),
        Some("hosting") => Some(Kind::Hosting),
        _ => None,
    };
    let kind = if p.vpn || p.proxy || p.tor || p.relay {
        Kind::Vpn
    } else if p.hosting {
        Kind::Hosting
    } else {
        typed.or_else(|| listed(&org, given.as_deref(), asn)).or_else(|| listed("", Some(&info.hostname), None)).unwrap_or(Kind::Business)
    };
    if kind != Kind::Business {
        return Network { kind, org, domain: None, source: "ipinfo" };
    }
    let domain = given.and_then(|d| util_input::domain(&d)).or_else(|| Some(info.hostname.as_str()).filter(|h| !generated_host(h)).and_then(registrable));
    Network { kind, org, domain, source: "ipinfo" }
}

/// Each entity with its nested entities, depth first.
fn entities(record: &Value) -> Vec<&Value> {
    let mut all = vec![];
    for e in record["entities"].as_array().into_iter().flatten() {
        all.push(e);
        all.extend(entities(e));
    }
    all
}

/// The values of a vCard property, like "fn" or "email", in a jCard: ["vcard", [[name, params, type, value], …]].
fn vcard<'a>(entity: &'a Value, property: &str) -> Vec<&'a str> {
    let props = entity["vcardArray"][1].as_array().map(Vec::as_slice).unwrap_or_default();
    props.iter().filter(|p| p[0] == property).filter_map(|p| p[3].as_str()).collect()
}

fn roles(entity: &Value) -> Vec<&str> {
    entity["roles"].as_array().into_iter().flatten().filter_map(Value::as_str).collect()
}

/// The organization holding the network: its registrant's name, else RIPE's description, else the network's name.
pub fn rdap_org(record: &Value) -> String {
    let all = entities(record);
    let registrant = all.iter().find(|e| roles(e).contains(&"registrant")).and_then(|e| vcard(e, "fn").first().copied());
    let description = record["remarks"].as_array().into_iter().flatten().find_map(|r| r["description"][0].as_str());
    registrant.or(description).or(record["name"].as_str()).unwrap_or_default().trim().to_string()
}

/// The domains of the network's contact addresses, the registrant's first, without registries' and free mail's.
pub fn rdap_email_domains(record: &Value) -> Vec<String> {
    let mut all = entities(record);
    all.sort_by_key(|e| !roles(e).contains(&"registrant"));
    let mut domains: Vec<String> = vec![];
    for email in all.iter().flat_map(|e| vcard(e, "email")) {
        let Some(domain) = email.rsplit_once('@').and_then(|(_, d)| registrable(d)) else { continue };
        if !NOT_COMPANIES.contains(&domain.as_str()) && !domains.contains(&domain) {
            domains.push(domain);
        }
    }
    domains
}

static LOOKUP: OnceLock<Lookup> = OnceLock::new();
/// Lookups running at once, so a burst of new visitors doesn't open hundreds of connections to RDAP.
static RESOLVING: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(8));
/// IPs being looked up, so a visitor's second page view doesn't look theirs up again.
static IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);
static COUNTS: LazyLock<Mutex<HashMap<String, (Instant, u32)>>> = LazyLock::new(Default::default);
const WINDOW: Duration = Duration::from_secs(60);

/// Counts a request against `key`'s limit for the minute. Kept in memory: a second backend just has its own counts.
pub fn allow(key: &str, per_minute: u32) -> bool {
    let now = Instant::now();
    let mut counts = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    if counts.len() > 50_000 {
        counts.retain(|_, (start, _)| now.duration_since(*start) < WINDOW);
    }
    let (start, count) = counts.entry(key.to_string()).or_insert((now, 0));
    if now.duration_since(*start) >= WINDOW {
        (*start, *count) = (now, 0);
    }
    *count += 1;
    *count <= per_minute
}

pub fn ip_hash(app_secret: &str, ip: IpAddr) -> String {
    crate::crypto::keyed_hash(app_secret, "visitor-ip", &ip.to_string())
}

/// A host is allowed when it's one of the site's domains or under one.
pub fn allowed_host(domains: &[String], host: &str) -> bool {
    let host = host.trim_end_matches('.').to_lowercase();
    domains.iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// A page view as the snippet sends it. Anything longer than the columns keep is cut.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Hit {
    pub path: String,
    pub title: String,
    pub referrer: String,
    pub visitor: String,
    pub utm_source: String,
    pub utm_medium: String,
    pub utm_campaign: String,
    pub utm_term: String,
    pub utm_content: String,
}

fn cut(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

/// A path as stored and matched: no query or fragment, and no trailing slash except the home page's.
pub fn clean_path(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or_default().trim();
    let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    let trimmed = path.trim_end_matches('/');
    cut(if trimmed.is_empty() { "/" } else { trimmed }, 300)
}

/// The referrer's origin and path, without its query, and its host, without www.
fn clean_referrer(referrer: &str) -> Option<(String, String)> {
    let url = reqwest::Url::parse(referrer.trim()).ok().filter(|u| matches!(u.scheme(), "http" | "https"))?;
    let host = url.host_str()?.trim_start_matches("www.").to_lowercase();
    Some((cut(&format!("{}://{}{}", url.scheme(), url.host_str()?, url.path()), 300), host))
}

pub struct Site {
    pub project_id: Uuid,
    pub domains: Vec<String>,
}

/// Stores the page view. A known network attributes it at once. A new one is looked up in the background and attributes
/// the IP's page views from the last day when it's done. The IP itself only lives in that task.
pub async fn record(state: &AppState, site: &Site, ip: IpAddr, hash: &str, hit: Hit) -> anyhow::Result<()> {
    let db = &state.db;
    let referrer = clean_referrer(&hit.referrer);
    let source = match (cut(&hit.utm_source, 100), &referrer) {
        (utm, _) if !utm.is_empty() => utm,
        (_, Some((_, host))) if !allowed_host(&site.domains, host) => host.clone(),
        _ => String::new(),
    };
    let cached: Option<(String, String, Option<String>)> = sqlx::query_as(
        "select kind, org, domain::text from visitor_networks
         where ip_hash = $1 and resolved_at > now() - case when source = 'none' then interval '1 day' else make_interval(days => $2) end",
    )
    .bind(hash)
    .bind(RECHECK_DAYS)
    .fetch_optional(db)
    .await?;
    // A lookup that failed is tried again a day later rather than in 30.
    let network = cached.map(|(kind, org, domain)| Network { kind: Kind::parse(&kind), org, domain, source: "cache" });
    let company = match &network {
        Some(n) => attribute(db, site.project_id, n).await?,
        None => None,
    };
    let path = clean_path(&hit.path);
    sqlx::query(
        "insert into visitor_hits (project_id, visitor_id, ip_hash, domain, company, path, title, referrer, source,
                                   utm_source, utm_medium, utm_campaign, utm_term, utm_content)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(site.project_id)
    .bind(cut(&hit.visitor, 64))
    .bind(hash)
    .bind(company.as_ref().map(|(d, _)| d))
    .bind(company.as_ref().map(|(_, c)| c.as_str()).unwrap_or_default())
    .bind(&path)
    .bind(cut(&hit.title, 200))
    .bind(referrer.map(|(r, _)| r).unwrap_or_default())
    .bind(source)
    .bind(cut(&hit.utm_source, 100))
    .bind(cut(&hit.utm_medium, 100))
    .bind(cut(&hit.utm_campaign, 100))
    .bind(cut(&hit.utm_term, 100))
    .bind(cut(&hit.utm_content, 100))
    .execute(db)
    .await?;
    sqlx::query("update visitor_sites set last_hit_at = now() where project_id = $1").bind(site.project_id).execute(db).await?;
    match (network, company) {
        (Some(_), Some((domain, _))) => {
            let state = state.clone();
            let project_id = site.project_id;
            tokio::spawn(async move { warn_on_error(alert(&state, project_id, &domain).await) });
        }
        (None, _) if hooks::is_public(ip) => look_up_later(state, ip, hash),
        _ => {}
    }
    Ok(())
}

fn warn_on_error<T>(result: anyhow::Result<T>) {
    if let Err(e) = result {
        tracing::warn!(error = ?e, "website visitor follow-up failed");
    }
}

fn look_up_later(state: &AppState, ip: IpAddr, hash: &str) {
    if !IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner()).insert(hash.to_string()) {
        return;
    }
    let (state, hash) = (state.clone(), hash.to_string());
    tokio::spawn(async move {
        let _permit = RESOLVING.acquire().await;
        let lookup = LOOKUP.get_or_init(|| Lookup::new(state.cfg.ipinfo_token.clone()));
        let network = lookup.resolve(&state.db, ip).await;
        warn_on_error(settle(&state, &hash, &network).await);
        IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner()).remove(&hash);
    });
}

/// Keeps what the lookup found and attributes the IP's page views from the last day in each project that has some.
pub async fn settle(state: &AppState, hash: &str, network: &Network) -> anyhow::Result<()> {
    let db = &state.db;
    sqlx::query(
        "insert into visitor_networks (ip_hash, kind, org, domain, source) values ($1, $2, $3, $4, $5)
         on conflict (ip_hash) do update set kind = excluded.kind, org = excluded.org, domain = excluded.domain,
           source = excluded.source, resolved_at = now()",
    )
    .bind(hash)
    .bind(network.kind.as_str())
    .bind(&network.org)
    .bind(&network.domain)
    .bind(network.source)
    .execute(db)
    .await?;
    let projects: Vec<Uuid> = sqlx::query_scalar(
        "select distinct project_id from visitor_hits where ip_hash = $1 and domain is null and created_at > now() - interval '1 day'",
    )
    .bind(hash)
    .fetch_all(db)
    .await?;
    for project_id in projects {
        let Some((domain, company)) = attribute(db, project_id, network).await? else { continue };
        sqlx::query(
            "update visitor_hits set domain = $3, company = $4
             where project_id = $1 and ip_hash = $2 and domain is null and created_at > now() - interval '1 day'",
        )
        .bind(project_id)
        .bind(hash)
        .bind(&domain)
        .bind(&company)
        .execute(db)
        .await?;
        alert(state, project_id, &domain).await?;
    }
    Ok(())
}

/// The company domain and name a business network's page views go under in this project. A network named without a domain
/// goes under the project's company of the same name, ignoring case, punctuation and endings like Inc or Ltd.
pub async fn attribute(db: &PgPool, project_id: Uuid, network: &Network) -> sqlx::Result<Option<(String, String)>> {
    if network.kind != Kind::Business {
        return Ok(None);
    }
    if let Some(domain) = &network.domain {
        return Ok(Some((domain.clone(), network.org.clone())));
    }
    let wanted = plain_name(&network.org);
    if wanted.is_empty() {
        return Ok(None);
    }
    let companies: Vec<(String, String)> =
        sqlx::query_as("select domain::text, name from companies where project_id = $1 and name <> ''").bind(project_id).fetch_all(db).await?;
    let mut matches = companies.into_iter().filter(|(_, name)| plain_name(name) == wanted);
    Ok(match (matches.next(), matches.next()) {
        (Some((domain, name)), None) => Some((domain, name)),
        _ => None,
    })
}

fn plain_name(name: &str) -> String {
    const ENDINGS: [&str; 14] = ["inc", "llc", "ltd", "limited", "corp", "corporation", "co", "company", "gmbh", "ag", "sa", "bv", "plc", "pty"];
    let mut words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    while words.last().is_some_and(|w| ENDINGS.contains(&w.to_lowercase().as_str())) {
        words.pop();
    }
    words.join(" ").to_lowercase()
}

/// Posts a hot-visitor alert to the project's chats when this company read one of the project's high-intent pages today and
/// is in a campaign or read at least ALERT_VIEWS pages today. Once per company per UTC day. True when it went out.
pub async fn alert(state: &AppState, project_id: Uuid, domain: &str) -> anyhow::Result<bool> {
    let row: Option<(i64, Option<String>, bool, String, String)> = sqlx::query_as(
        "with today as (
           select path, created_at from visitor_hits
           where project_id = $1 and domain = $2::citext and created_at >= date_trunc('day', now() at time zone 'utc') at time zone 'utc'
         )
         select (select count(*) from today),
                (select t.path from today t, unnest(s.intent_paths) as i(path)
                 where t.path = i.path or starts_with(t.path, i.path || '/') order by t.created_at desc limit 1),
                exists (select 1 from companies c where c.project_id = $1 and c.domain = $2::citext and c.segment_id is not null)
                  or exists (select 1 from leads l where l.project_id = $1 and l.company_domain = $2::citext and l.segment_id is not null),
                coalesce((select nullif(c.name, '') from companies c where c.project_id = $1 and c.domain = $2::citext),
                         (select d.name from directory_companies d where d.domain = lower($2)),
                         (select nullif(max(h.company), '') from visitor_hits h where h.project_id = $1 and h.domain = $2::citext), $2),
                p.name
         from visitor_sites s join projects p on p.id = s.project_id where s.project_id = $1",
    )
    .bind(project_id)
    .bind(domain)
    .fetch_optional(&state.db)
    .await?;
    let Some((views, Some(page), in_campaign, name, project)) = row else { return Ok(false) };
    if !in_campaign && views < ALERT_VIEWS {
        return Ok(false);
    }
    let claimed = sqlx::query(
        "insert into visitor_alerts (project_id, domain, day) values ($1, $2, (now() at time zone 'utc')::date) on conflict do nothing",
    )
    .bind(project_id)
    .bind(domain)
    .execute(&state.db)
    .await?
    .rows_affected();
    if claimed == 0 {
        return Ok(false);
    }
    let pages = if views == 1 { "1 page".to_string() } else { format!("{views} pages") };
    let summary = format!("{name} ({domain}) is reading {page} on {project}'s site, {pages} today");
    let link = format!("{}/app/p/{project_id}/companies?tab=visitors", state.cfg.public_url);
    let alert = ChatAlert { label: "Hot visitor", summary: &summary, quote: None, link: &link, link_text: "See your visitors" };
    hooks::post_chat(&state.db, project_id, &alert).await;
    Ok(true)
}

/// Page views go after RETENTION_DAYS, networks when they're due a new lookup, and alert claims once their day is over.
pub async fn expire(db: &PgPool) -> sqlx::Result<()> {
    sqlx::query("delete from visitor_hits where created_at < now() - make_interval(days => $1)").bind(RETENTION_DAYS).execute(db).await?;
    sqlx::query("delete from visitor_networks where resolved_at < now() - make_interval(days => $1)").bind(RECHECK_DAYS).execute(db).await?;
    sqlx::query("delete from visitor_alerts where day < (now() at time zone 'utc')::date - 1").execute(db).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use serde_json::json;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn rdap_redirects_only_go_to_the_registries() {
        let app = Router::new()
            .route("/to-local", get(|| async { (StatusCode::FOUND, [("location", "http://127.0.0.1:1/secret")]) }))
            .route("/to-arin", get(|| async { (StatusCode::FOUND, [("location", "https://rdap.arin.net.evil.test/ip/1.2.3.4")]) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::custom(registry_redirect)).build().unwrap();
        for path in ["to-local", "to-arin"] {
            assert_eq!(client.get(format!("{base}/{path}")).send().await.unwrap().status(), StatusCode::FOUND, "{path} isn't followed");
        }
    }

    #[test]
    fn snippet_is_small_sets_no_cookie_and_sends_nothing_when_asked_not_to_track() {
        assert!(SNIPPET.len() < 2048, "the snippet is {} bytes", SNIPPET.len());
        assert!(!SNIPPET.contains("cookie"));
        let bail = SNIPPET.find("return;").unwrap();
        for check in [r#"n.doNotTrack=="1""#, r#"w.doNotTrack=="1""#, "n.globalPrivacyControl"] {
            assert!(SNIPPET.find(check).is_some_and(|at| at < bail), "{check} is checked before anything is sent");
        }
        assert!(SNIPPET.find("sendBeacon(").unwrap() > bail);
        assert!(SNIPPET.contains("localStorage") && SNIPPET.contains(r#"replace(/\.js(\?.*)?$/,"/hit")"#));
    }

    #[test]
    fn isps_mobile_carriers_clouds_and_vpns_are_told_from_businesses() {
        assert_eq!(listed("Hetzner Online GmbH", None, None), Some(Kind::Hosting));
        assert_eq!(listed("Vodafone GmbH", None, None), Some(Kind::Mobile));
        assert_eq!(listed("Deutsche Telekom AG", None, None), Some(Kind::Isp));
        assert_eq!(listed("AT&T Services, Inc.", None, None), Some(Kind::Isp));
        assert_eq!(listed("Packethub S.A.", None, None), Some(Kind::Vpn));
        assert_eq!(listed("Example Networks", Some("ec2-3-4-5-6.compute-1.amazonaws.com"), None), Some(Kind::Hosting));
        assert_eq!(listed("Datacamp Limited", None, Some(212238)), Some(Kind::Hosting));
        // Whole words only, and brands that are also everyday words don't drop businesses named after them.
        assert_eq!(listed("Cloudinary Ltd", None, None), None);
        assert_eq!(listed("Orange County Dental", None, None), None);
        assert_eq!(listed("Brightline Logistics", Some("brightline.example"), Some(64500)), None);

        assert!(generated_host("203-0-113-5.static.isp.example"));
        assert!(generated_host("dsl-pool7.isp.example"));
        assert!(generated_host("c-73-1-2-3.hsd1.ca.comcast.net"));
        assert!(!generated_host("mail.acme.example"));
        assert!(!generated_host("gw1.acme.example"));
        assert_eq!(registrable("Mail.Acme.com.").as_deref(), Some("acme.com"));
        assert_eq!(registrable("gw.acme.co.uk").as_deref(), Some("acme.co.uk"));
        assert_eq!(registrable("localhost"), None);

        let info = |v: Value| from_ipinfo(&serde_json::from_value(v).unwrap());
        let paid = info(json!({"ip": "192.0.2.1", "hostname": "gw.acme.example", "org": "AS64500 Acme Corp",
            "company": {"name": "Acme Corp", "domain": "acme.example", "type": "business"},
            "asn": {"asn": "AS64500", "name": "Acme Corp", "domain": "acme.example", "type": "business"},
            "privacy": {"vpn": false, "proxy": false, "tor": false, "relay": false, "hosting": false}}));
        assert_eq!(paid, Network { kind: Kind::Business, org: "Acme Corp".into(), domain: Some("acme.example".into()), source: "ipinfo" });
        // The free plan has only org and hostname.
        let free = info(json!({"ip": "192.0.2.2", "hostname": "mail.kestrel.example", "org": "AS64501 Kestrel Labs"}));
        assert_eq!((free.kind, free.org.as_str(), free.domain.as_deref()), (Kind::Business, "Kestrel Labs", Some("kestrel.example")));
        let home = info(json!({"hostname": "c-73-1-2-3.hsd1.ca.comcast.net", "org": "AS7922 Comcast Cable Communications, LLC"}));
        assert_eq!((home.kind, home.domain), (Kind::Isp, None));
        assert_eq!(info(json!({"hostname": "ec2-3-4-5-6.compute-1.amazonaws.com", "org": "AS16509 Amazon.com, Inc."})).kind, Kind::Hosting);
        assert_eq!(info(json!({"org": "AS21928 T-Mobile USA, Inc."})).kind, Kind::Mobile);
        assert_eq!(info(json!({"org": "AS64502 Quiet Co", "privacy": {"vpn": true}})).kind, Kind::Vpn);
        assert_eq!(info(json!({"org": "AS64503 Small Town Net", "asn": {"asn": "AS64503", "name": "Small Town Net", "type": "isp"}})).kind, Kind::Isp);
    }

    /// IPinfo answers by IP, with a 429 for 192.0.2.99, and RDAP by IP, with a 404 for any IP it has no record for.
    #[derive(Clone, Default)]
    struct Fake {
        ipinfo: Arc<Mutex<Vec<(String, String)>>>,
        rdap: Arc<Mutex<HashMap<String, Value>>>,
    }

    async fn ipinfo(State(fake): State<Fake>, Path(ip): Path<String>, headers: HeaderMap) -> Response {
        let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        fake.ipinfo.lock().unwrap().push((ip.clone(), auth));
        match ip.as_str() {
            "192.0.2.99" => (StatusCode::TOO_MANY_REQUESTS, "slow down").into_response(),
            _ => axum::Json(json!({"ip": ip, "hostname": "gw.acme.example", "org": "AS64500 Acme Corp",
                "company": {"name": "Acme Corp", "domain": "acme.example", "type": "business"}}))
            .into_response(),
        }
    }

    async fn rdap(State(fake): State<Fake>, Path(ip): Path<String>) -> Response {
        match fake.rdap.lock().unwrap().get(&ip) {
            Some(record) => axum::Json(record.clone()).into_response(),
            None => (StatusCode::NOT_FOUND, "{}").into_response(),
        }
    }

    async fn serve() -> (String, Fake) {
        let fake = Fake::default();
        let app = Router::new().route("/ipinfo/{ip}", get(ipinfo)).route("/rdap/ip/{ip}", get(rdap)).with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, fake)
    }

    fn entity(roles: &[&str], name: &str, email: Option<&str>) -> Value {
        let mut card = vec![json!(["version", {}, "text", "4.0"]), json!(["fn", {}, "text", name])];
        card.extend(email.map(|e| json!(["email", {}, "text", e])));
        json!({"objectClassName": "entity", "roles": roles, "vcardArray": ["vcard", card]})
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn ipinfo_comes_first_and_rdap_covers_the_rest(db: PgPool) {
        let (base, fake) = serve().await;
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let mut registrant = entity(&["registrant"], "Northwind Studio Ltd", None);
        registrant["entities"] = json!([entity(&["technical"], "NOC", Some("noc@mail.northwind.example"))]);
        let abuse = entity(&["abuse"], "Abuse desk", Some("abuse@ripe.net"));
        let records = [
            ("192.0.2.10", json!({"name": "NORTHWIND-NET", "entities": [abuse.clone(), registrant]})),
            ("192.0.2.11", json!({"name": "COMCAST-1", "entities": [entity(&["registrant"], "Comcast Cable Communications, LLC", Some("abuse@comcast.net"))]})),
            // The holding company's contact comes first, but the company database knows the second.
            ("192.0.2.12", json!({"name": "BRIGHT-NET", "entities": [entity(&["registrant", "administrative"], "Brightline Group", Some("hostmaster@holdings.example")),
                entity(&["technical"], "IT", Some("it@brightline.example"))]})),
            ("192.0.2.99", json!({"name": "ACME-NET", "entities": [entity(&["registrant"], "Acme Corp", Some("noc@acme.example"))]})),
            ("192.0.2.13", json!({"name": "KESTREL", "remarks": [{"description": ["Kestrel Labs office network"]}], "entities": [abuse]})),
        ];
        fake.rdap.lock().unwrap().extend(records.into_iter().map(|(ip, r)| (ip.to_string(), r)));
        sqlx::query("insert into directory_companies (domain, name, website, country_code, source_id, release) values ('brightline.example', 'Brightline', 'https://brightline.example', 'GB', 's1', 'r1')")
            .execute(&db)
            .await
            .unwrap();

        let with_token = Lookup::at(&base, Some("tok"));
        let acme = with_token.resolve(&db, ip("192.0.2.1")).await;
        assert_eq!(acme, Network { kind: Kind::Business, org: "Acme Corp".into(), domain: Some("acme.example".into()), source: "ipinfo" });
        assert_eq!(fake.ipinfo.lock().unwrap()[0], ("192.0.2.1".to_string(), "Bearer tok".to_string()));
        // IPinfo over its quota falls back to RDAP.
        let fallback = with_token.resolve(&db, ip("192.0.2.99")).await;
        assert_eq!((fallback.source, fallback.domain.as_deref()), ("rdap", Some("acme.example")));

        let rdap_only = Lookup::at(&base, None);
        let calls = fake.ipinfo.lock().unwrap().len();
        let northwind = rdap_only.resolve(&db, ip("192.0.2.10")).await;
        assert_eq!(northwind, Network { kind: Kind::Business, org: "Northwind Studio Ltd".into(), domain: Some("northwind.example".into()), source: "rdap" });
        assert_eq!(fake.ipinfo.lock().unwrap().len(), calls, "no token, no IPinfo");
        let comcast = rdap_only.resolve(&db, ip("192.0.2.11")).await;
        assert_eq!((comcast.kind, comcast.domain), (Kind::Isp, None));
        assert_eq!(rdap_only.resolve(&db, ip("192.0.2.12")).await.domain.as_deref(), Some("brightline.example"));
        let kestrel = rdap_only.resolve(&db, ip("192.0.2.13")).await;
        assert_eq!((kestrel.kind, kestrel.org.as_str(), kestrel.domain), (Kind::Business, "Kestrel Labs office network", None));
        assert_eq!(rdap_only.resolve(&db, ip("192.0.2.200")).await, Network::none());
    }

    async fn project(db: &PgPool) -> Uuid {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name, domain) values ($1, 'Fieldnote', 'fieldnote.example') returning id").bind(org).fetch_one(db).await.unwrap();
        sqlx::query("insert into visitor_sites (project_id, domains, intent_paths) values ($1, '{fieldnote.example}', '{/pricing}')")
            .bind(project)
            .execute(db)
            .await
            .unwrap();
        project
    }

    fn business(org: &str, domain: Option<&str>) -> Network {
        Network { kind: Kind::Business, org: org.into(), domain: domain.map(String::from), source: "rdap" }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn networks_match_the_projects_companies_and_attribute_the_last_days_views(db: PgPool) {
        let state = crate::autopilot::tests::state(db.clone());
        let project = project(&db).await;
        for (domain, name) in [("brightline.example", "Brightline, Inc."), ("twin-a.example", "Twin"), ("twin-b.example", "Twin Ltd")] {
            sqlx::query("insert into companies (project_id, domain, name) values ($1, $2, $3)").bind(project).bind(domain).bind(name).execute(&db).await.unwrap();
        }
        let named = |org: &'static str| {
            let db = db.clone();
            async move { attribute(&db, project, &business(org, None)).await }
        };
        assert_eq!(named("BRIGHTLINE INC").await.unwrap(), Some(("brightline.example".into(), "Brightline, Inc.".into())));
        assert_eq!(named("Twin").await.unwrap(), None, "two companies have that name");
        assert_eq!(named("Somebody Else").await.unwrap(), None);
        assert_eq!(attribute(&db, project, &business("Acme", Some("acme.example"))).await.unwrap(), Some(("acme.example".into(), "Acme".into())));
        let isp = Network { kind: Kind::Isp, ..business("Comcast", Some("comcast.net")) };
        assert_eq!(attribute(&db, project, &isp).await.unwrap(), None);

        for age in ["1 hour", "2 days"] {
            sqlx::query("insert into visitor_hits (project_id, ip_hash, path, created_at) values ($1, 'h1', '/', now() - $2::interval)")
                .bind(project)
                .bind(age)
                .execute(&db)
                .await
                .unwrap();
        }
        settle(&state, "h1", &business("Brightline", None)).await.unwrap();
        let hits: Vec<Option<String>> =
            sqlx::query_scalar("select domain::text from visitor_hits order by created_at desc").fetch_all(&db).await.unwrap();
        assert_eq!(hits, [Some("brightline.example".to_string()), None]);
        let kept: (String, Option<String>) = sqlx::query_as("select kind, domain::text from visitor_networks where ip_hash = 'h1'").fetch_one(&db).await.unwrap();
        assert_eq!(kept, ("business".to_string(), None));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_hot_visitor_alerts_once_a_day(db: PgPool) {
        let state = crate::autopilot::tests::state(db.clone());
        let project = project(&db).await;
        let view = |domain: &'static str, path: &'static str| {
            sqlx::query("insert into visitor_hits (project_id, ip_hash, domain, path) values ($1, 'h', $2, $3)").bind(project).bind(domain).bind(path).execute(&db)
        };
        view("acme.example", "/pricing").await.unwrap();
        assert!(!alert(&state, project, "acme.example").await.unwrap(), "one page and not in a campaign");
        view("acme.example", "/").await.unwrap();
        view("acme.example", "/docs").await.unwrap();
        assert!(alert(&state, project, "acme.example").await.unwrap());
        view("acme.example", "/pricing/teams").await.unwrap();
        assert!(!alert(&state, project, "acme.example").await.unwrap(), "already alerted today");

        // A company in a campaign alerts on its first high-intent page, and yesterday's alert doesn't count.
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Q4') returning id").bind(project).fetch_one(&db).await.unwrap();
        sqlx::query("insert into leads (project_id, segment_id, full_name, email, company_domain) values ($1, $2, 'Bo', 'bo@kestrel.example', 'kestrel.example')")
            .bind(project)
            .bind(segment)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into visitor_alerts (project_id, domain, day) values ($1, 'kestrel.example', (now() at time zone 'utc')::date - 1)")
            .bind(project)
            .execute(&db)
            .await
            .unwrap();
        view("kestrel.example", "/blog").await.unwrap();
        assert!(!alert(&state, project, "kestrel.example").await.unwrap(), "not a high-intent page");
        view("kestrel.example", "/pricing").await.unwrap();
        assert!(alert(&state, project, "kestrel.example").await.unwrap());
        let alerts: i64 = sqlx::query_scalar("select count(*) from visitor_alerts where day = (now() at time zone 'utc')::date").fetch_one(&db).await.unwrap();
        assert_eq!(alerts, 2);

        // Without high-intent pages, nothing alerts.
        sqlx::query("update visitor_sites set intent_paths = '{}'").execute(&db).await.unwrap();
        for _ in 0..3 {
            view("quarry.example", "/pricing").await.unwrap();
        }
        assert!(!alert(&state, project, "quarry.example").await.unwrap());
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn page_views_go_after_180_days(db: PgPool) {
        let project = project(&db).await;
        for age in ["179 days", "181 days"] {
            sqlx::query("insert into visitor_hits (project_id, ip_hash, path, created_at) values ($1, $2, '/', now() - $2::interval)")
                .bind(project)
                .bind(age)
                .execute(&db)
                .await
                .unwrap();
            sqlx::query("insert into visitor_networks (ip_hash, kind, source, resolved_at) values ($1, 'isp', 'rdap', now() - $1::interval)")
                .bind(age)
                .execute(&db)
                .await
                .unwrap();
        }
        sqlx::query("insert into visitor_networks (ip_hash, kind, source) values ('fresh', 'business', 'rdap')").execute(&db).await.unwrap();
        crate::agent::expire_visitor_data(&db).await.unwrap();
        let hits: Vec<String> = sqlx::query_scalar("select ip_hash from visitor_hits").fetch_all(&db).await.unwrap();
        assert_eq!(hits, ["179 days"]);
        let networks: Vec<String> = sqlx::query_scalar("select ip_hash from visitor_networks").fetch_all(&db).await.unwrap();
        assert_eq!(networks, ["fresh"]);
    }
}
