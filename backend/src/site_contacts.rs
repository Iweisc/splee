//! A contact address from a business's own website, without Claude, for businesses added from Google Maps: the homepage
//! and up to two contact or about pages it links to on the same site, each only where robots.txt allows it. Only addresses
//! at the business's own domain count, and someone's own address beats a shared one like info@.

use std::time::Duration;

use reqwest::Url;

const USER_AGENT: &str = "Mozilla/5.0 (compatible; Splee contact finder)";
/// The robots.txt token Splee answers to, besides *.
const ROBOTS_AGENT: &str = "splee";
const PAGE_TIMEOUT: Duration = Duration::from_secs(5);
/// The whole read of one site, robots.txt included.
const SITE_TIMEOUT: Duration = Duration::from_secs(12);
/// Contact details sit in the visible text and links, well inside this.
const MAX_PAGE_BYTES: usize = 512 << 10;
const MAX_LINKED_PAGES: usize = 2;
/// Words in a link's address or text that make it a contact or about page, the likeliest first.
const PAGE_WORDS: [&str; 8] = ["contact", "kontakt", "impressum", "team", "staff", "about", "people", "meet"];
/// Shared addresses, the ones worth writing to first. Any other address that isn't in NEVER counts as a person's. The last
/// ones are words local businesses name their inboxes after, like smile@ at a dentist.
const ROLES: [&str; 47] = [
    "hello", "hi", "info", "contact", "contactus", "office", "enquiries", "enquiry", "inquiries", "inquiry", "reception", "frontdesk", "front.desk",
    "frontoffice", "appointments", "appointment", "bookings", "booking", "book", "team", "admin", "mail", "email", "sales", "studio", "clinic",
    "practice", "general", "manager", "support", "help", "service", "care", "patients", "newpatients", "smile", "smiles", "dental", "dentist",
    "ortho", "orders", "reservations", "events", "catering", "members", "membership", "welcome",
];
/// Addresses nobody reads, or that are for something other than a sales email.
const NEVER: [&str; 26] = [
    "noreply", "no-reply", "no_reply", "donotreply", "do-not-reply", "privacy", "legal", "abuse", "postmaster", "webmaster", "hostmaster", "unsubscribe",
    "jobs", "careers", "career", "hr", "recruiting", "press", "media", "billing", "invoices", "accounts", "accounting", "dmca", "gdpr", "security",
];

pub struct Sites {
    client: reqwest::Client,
    /// The site's address with {domain}, without a trailing slash.
    site: String,
    /// Off only for a fake site on this machine, in tests and sandboxes.
    public_only: bool,
}

/// An address found on the site.
#[derive(Debug, Clone, PartialEq)]
pub struct Address {
    pub email: String,
    /// Someone's own address rather than a shared one like info@.
    pub person: bool,
    /// The person's name when the address spells it, like maria.lopez@, else "".
    pub name: String,
}

/// What reading a site found. `addresses` are the best first.
#[derive(Debug, Default, PartialEq)]
pub struct Found {
    pub addresses: Vec<Address>,
    /// The homepage answered.
    pub read: bool,
    /// robots.txt asks not to read the homepage.
    pub blocked: bool,
}

impl Sites {
    /// `fake` is Config::local_sites_url: empty for the businesses' real sites, else a fake that serves each site under
    /// {fake}/site/{domain}.
    pub fn new(fake: &str) -> Self {
        // Redirects are followed by hand in hooks::get_page, so each one is checked before it's requested.
        let client = reqwest::Client::builder().user_agent(USER_AGENT).timeout(PAGE_TIMEOUT).connect_timeout(Duration::from_secs(4));
        let client = client.redirect(reqwest::redirect::Policy::none()).build();
        let client = client.expect("reqwest client builds");
        if fake.is_empty() {
            Self { client, site: "https://{domain}".into(), public_only: true }
        } else {
            Self { client, site: format!("{}/site/{{domain}}", fake.trim_end_matches('/')), public_only: false }
        }
    }

    /// The site a URL is on: its host without www., or under the fake, the domain after /site/.
    fn site(&self, u: &Url) -> Option<String> {
        let site = if self.public_only { u.host_str() } else { u.path().strip_prefix("/site/").and_then(|p| p.split('/').next()) };
        site.map(|s| s.trim_start_matches("www.").to_lowercase())
    }

    pub async fn find(&self, domain: &str) -> Found {
        tokio::time::timeout(SITE_TIMEOUT, self.read(domain)).await.unwrap_or_default()
    }

    async fn read(&self, domain: &str) -> Found {
        let base = self.site.replace("{domain}", domain);
        let robots = self.page(&format!("{base}/robots.txt")).await.map(|t| Robots::parse(&t)).unwrap_or_default();
        if !robots.allows("/") {
            return Found { blocked: true, ..Found::default() };
        }
        let Some(home) = self.page(&format!("{base}/")).await else { return Found::default() };
        let mut found = emails(&home, domain);
        for path in contact_paths(&home, domain).into_iter().filter(|p| robots.allows(p)).take(MAX_LINKED_PAGES) {
            for email in self.page(&format!("{base}{path}")).await.map(|page| emails(&page, domain)).unwrap_or_default() {
                if !found.contains(&email) {
                    found.push(email);
                }
            }
        }
        Found { addresses: rank(&found, domain), read: true, blocked: false }
    }

    /// The start of the page, when it answers and its redirects stay on the same site. A redirect elsewhere isn't followed.
    async fn page(&self, url: &str) -> Option<String> {
        let start = self.site(&Url::parse(url).ok()?)?;
        let same_site = |u: &Url| self.site(u).as_deref() == Some(start.as_str());
        let mut res = crate::hooks::get_page(&self.client, url, self.public_only, USER_AGENT, PAGE_TIMEOUT, &same_site).await?;
        let mut body = Vec::new();
        while body.len() < MAX_PAGE_BYTES {
            match res.chunk().await {
                Ok(Some(chunk)) => body.extend_from_slice(&chunk),
                Ok(None) => break,
                Err(_) if !body.is_empty() => break,
                Err(_) => return None,
            }
        }
        body.truncate(MAX_PAGE_BYTES);
        Some(String::from_utf8_lossy(&body).into_owned())
    }
}

/// The addresses at `domain` in the page, in mailto: links and plain text alike, in the order they appear.
fn emails(html: &str, domain: &str) -> Vec<String> {
    // Sites hide the @ from scrapers this way, and mailto: links can be percent-encoded.
    let text = html.replace("&#64;", "@").replace("&#x40;", "@").replace("&commat;", "@").replace("%40", "@");
    let bytes = text.as_bytes();
    let local = |b: u8| b.is_ascii_alphanumeric() || b"._%+-".contains(&b);
    let host = |b: u8| b.is_ascii_alphanumeric() || b == b'.' || b == b'-';
    let mut found: Vec<String> = Vec::new();
    for (at, _) in text.match_indices('@') {
        let start = (0..at).rev().take_while(|&i| local(bytes[i])).last().unwrap_or(at);
        let end = (at + 1..bytes.len()).take_while(|&i| host(bytes[i])).last().map_or(at + 1, |i| i + 1);
        let (name, site) = (text[start..at].trim_matches('.'), text[at + 1..end].trim_end_matches(['.', '-']));
        if name.is_empty() || !site.eq_ignore_ascii_case(domain) {
            continue;
        }
        let email = format!("{name}@{site}").to_lowercase();
        if !found.contains(&email) {
            found.push(email);
        }
    }
    found
}

/// Paths of the contact and about pages the page links to on the same site, the likeliest first.
fn contact_paths(html: &str, domain: &str) -> Vec<String> {
    let lower = html.to_lowercase();
    let mut links: Vec<(usize, String)> = Vec::new();
    for (at, _) in lower.match_indices("href=") {
        let rest = &html[at + 5..];
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'');
        let value = match quote {
            Some(q) => rest[1..].split(q).next().unwrap_or_default(),
            None => rest.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or_default(),
        };
        // The link's text, up to its closing tag, also says what the page is.
        let text = lower[at..].split("</a").next().unwrap_or_default().chars().take(300).collect::<String>();
        let Some(path) = same_site_path(value.trim(), domain) else { continue };
        let words = format!("{} {text}", path.to_lowercase());
        if let Some(rank) = PAGE_WORDS.iter().position(|w| words.contains(w))
            && path != "/"
            && !links.iter().any(|(_, p)| *p == path)
        {
            links.push((rank, path));
        }
    }
    links.sort_by_key(|(rank, _)| *rank);
    links.into_iter().map(|(_, path)| path).collect()
}

/// The path and query of a link to a page on the business's own site, or None for one elsewhere or to a file.
fn same_site_path(href: &str, domain: &str) -> Option<String> {
    let lower = href.to_lowercase();
    if href.is_empty() || href.starts_with('#') || ["mailto:", "tel:", "javascript:", "data:"].iter().any(|s| lower.starts_with(s)) {
        return None;
    }
    let path = if lower.starts_with("http://") || lower.starts_with("https://") || href.starts_with("//") {
        let url = Url::parse(&if href.starts_with("//") { format!("https:{href}") } else { href.to_string() }).ok()?;
        if !url.host_str()?.trim_start_matches("www.").eq_ignore_ascii_case(domain) {
            return None;
        }
        format!("{}{}", url.path(), url.query().map(|q| format!("?{q}")).unwrap_or_default())
    } else if href.starts_with('/') {
        href.to_string()
    } else {
        format!("/{href}")
    };
    let path = path.split('#').next().unwrap_or_default().to_string();
    let file = path.split('?').next().unwrap_or_default().rsplit('.').next().filter(|ext| ext.len() <= 4 && !ext.contains('/'));
    if file.is_some_and(|ext| !["html", "htm", "php", "asp", "aspx"].contains(&ext.to_lowercase().as_str())) {
        return None;
    }
    Some(path)
}

/// People's addresses first, those that spell a whole name before the rest, then shared ones in ROLES' order and the
/// business's own name last. Addresses in NEVER are left out.
fn rank(emails: &[String], domain: &str) -> Vec<Address> {
    let label = domain.split('.').next().unwrap_or_default();
    let mut ranked: Vec<(usize, Address)> = emails
        .iter()
        .filter_map(|email| {
            let local = email.split('@').next()?;
            let bare = local.trim_end_matches(|c: char| c.is_ascii_digit());
            if NEVER.iter().any(|n| bare == *n || bare.starts_with(&format!("{n}."))) {
                return None;
            }
            // The business's own name, like smiledental@smiledental.example, is shared too.
            let role = ROLES.iter().position(|r| bare == *r).or_else(|| (label.contains(bare) || bare.contains(label)).then_some(ROLES.len()));
            if let Some(role) = role {
                return Some((10 + role, Address { email: email.clone(), person: false, name: String::new() }));
            }
            let name = name_of(local);
            Some((if name.contains(' ') { 0 } else { 1 }, Address { email: email.clone(), person: true, name }))
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, a)| a).collect()
}

/// "maria.lopez" reads as Maria Lopez and "maria" as Maria. Anything else, like "dr.k2", gives no name.
fn name_of(local: &str) -> String {
    let parts: Vec<&str> = local.split(['.', '_', '-']).collect();
    let word = |p: &str| p.len() >= 2 && p.len() <= 20 && p.chars().all(|c| c.is_ascii_alphabetic());
    let title = |p: &str| p[..1].to_uppercase() + &p[1..];
    match parts.as_slice() {
        [first, last] if word(first) && word(last) => format!("{} {}", title(first), title(last)),
        [first] if word(first) && (3..=12).contains(&first.len()) => title(first),
        _ => String::new(),
    }
}

/// Allow (true) or Disallow, and its path pattern.
type Rule = (bool, String);

/// The rules of robots.txt that apply to Splee: its own group when it has one, else the * group.
#[derive(Debug, Default)]
struct Robots {
    rules: Vec<Rule>,
}

impl Robots {
    fn parse(text: &str) -> Self {
        let mut groups: Vec<(Vec<String>, Vec<Rule>)> = Vec::new();
        let mut naming = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            let Some((key, value)) = line.split_once(':') else { continue };
            let (key, value) = (key.trim().to_lowercase(), value.trim());
            match key.as_str() {
                "user-agent" => {
                    if !naming {
                        groups.push((Vec::new(), Vec::new()));
                        naming = true;
                    }
                    groups.last_mut().expect("just pushed").0.push(value.to_lowercase());
                }
                "allow" | "disallow" => {
                    naming = false;
                    if let Some(group) = groups.last_mut()
                        && !value.is_empty()
                    {
                        group.1.push((key == "allow", value.to_string()));
                    }
                }
                _ => {}
            }
        }
        let ours = |agent: &dyn Fn(&str) -> bool| groups.iter().filter(|g| g.0.iter().any(|a| agent(a))).flat_map(|g| g.1.clone()).collect::<Vec<_>>();
        let own = ours(&|a| a.contains(ROBOTS_AGENT));
        Self { rules: if own.is_empty() { ours(&|a| a == "*") } else { own } }
    }

    /// The longest matching rule decides, and Allow wins a tie, as Google reads robots.txt.
    fn allows(&self, path: &str) -> bool {
        let best = self.rules.iter().filter(|(_, pattern)| matches(pattern, path)).max_by_key(|(allow, pattern)| (pattern.len(), *allow));
        best.is_none_or(|(allow, _)| *allow)
    }
}

/// Whether a robots.txt path pattern, with * for anything and a final $ for the end, matches the start of `path`.
fn matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = pattern.strip_suffix('$').map_or((pattern, false), |p| (p, true));
    let mut parts = pattern.split('*');
    let Some(mut rest) = path.strip_prefix(parts.next().unwrap_or_default()) else { return false };
    let parts: Vec<&str> = parts.collect();
    for (i, part) in parts.iter().enumerate() {
        if anchored && i == parts.len() - 1 {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    !anchored || rest.is_empty()
}

#[cfg(test)]
pub mod fake {
    //! Websites on this machine: each page is served at /site/{domain}{path}, and every request is counted.

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path, State};
    use axum::http::{StatusCode, header};
    use axum::response::{Html, IntoResponse, Response};
    use axum::routing::get;

    #[derive(Clone, Default)]
    pub struct Fake {
        pages: Arc<Mutex<HashMap<String, String>>>,
        pub hits: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        /// A body of "REDIRECT <location>" answers with a redirect there instead.
        pub fn page(&self, domain: &str, path: &str, body: &str) {
            self.pages.lock().unwrap().insert(format!("{domain}{path}"), body.into());
        }

        pub fn fetched(&self, domain: &str, path: &str) -> bool {
            self.hits.lock().unwrap().contains(&format!("{domain}{path}"))
        }
    }

    async fn page(State(fake): State<Fake>, Path((domain, rest)): Path<(String, String)>) -> Response {
        let key = format!("{domain}/{rest}");
        fake.hits.lock().unwrap().push(key.clone());
        match fake.pages.lock().unwrap().get(&key) {
            Some(body) if body.starts_with("REDIRECT ") => (StatusCode::FOUND, [(header::LOCATION, body["REDIRECT ".len()..].to_string())]).into_response(),
            Some(body) => Html(body.clone()).into_response(),
            None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
        }
    }

    async fn home(state: State<Fake>, Path(domain): Path<String>) -> Response {
        page(state, Path((domain, String::new()))).await
    }

    /// Starts the fake and returns the base URL to use as Config::local_sites_url.
    pub async fn start() -> (String, Fake) {
        let fake = Fake::default();
        let app = Router::new()
            .route("/site/{domain}/", get(home))
            .route("/site/{domain}/{*rest}", get(page))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        (base, fake)
    }
}

#[cfg(test)]
mod tests {
    use super::{Address, Robots, Sites};

    fn person(email: &str, name: &str) -> Address {
        Address { email: email.into(), person: true, name: name.into() }
    }

    fn role(email: &str) -> Address {
        Address { email: email.into(), person: false, name: String::new() }
    }

    #[tokio::test]
    async fn people_on_the_contact_page_beat_the_shared_address_and_other_domains_are_left_out() {
        let (base, fake) = super::fake::start().await;
        fake.page(
            "smile.example",
            "/",
            r#"<a href="https://www.smile.example/contact-us">Contact</a> <a href="/about">About us</a> <a href="https://other.example/contact">Partner</a>
               <a href="/blog">Blog</a> <a href="mailto:info@smile.example?subject=Hi">Email us</a> Built by web@agency.example"#,
        );
        fake.page("smile.example", "/contact-us", "Write to maria.lopez&#64;smile.example or noreply@smile.example");
        fake.page("smile.example", "/about", "Dr. Kim: <a href='mailto:kim%40smile.example'>kim</a>, smile@smile.example");
        let found = Sites::new(&base).find("smile.example").await;
        assert!(found.read && !found.blocked);
        assert_eq!(found.addresses, [person("maria.lopez@smile.example", "Maria Lopez"), person("kim@smile.example", "Kim"), role("info@smile.example"),
            role("smile@smile.example")]);
        assert!(!fake.fetched("smile.example", "/blog"), "only contact and about pages are read");
        assert!(fake.hits.lock().unwrap().iter().all(|h| h.starts_with("smile.example")), "nothing on another site is read");
    }

    #[tokio::test]
    async fn a_redirect_to_another_site_is_never_requested() {
        let (base, fake) = super::fake::start().await;
        fake.page("smile.example", "/", r#"<a href="/contact">Contact</a> <a href="/about">About</a>"#);
        fake.page("smile.example", "/contact", "REDIRECT /site/www.smile.example/contact-us");
        fake.page("www.smile.example", "/contact-us", "maria@smile.example");
        fake.page("smile.example", "/about", "REDIRECT /site/other.example/about");
        fake.page("other.example", "/about", "kim@smile.example");
        let found = Sites::new(&base).find("smile.example").await;
        assert_eq!(found.addresses, [person("maria@smile.example", "Maria")], "www. is the same site");
        assert!(!fake.fetched("other.example", "/about"));
    }

    #[tokio::test]
    async fn robots_txt_keeps_pages_unread_and_a_shared_address_is_kept_when_alone() {
        let (base, fake) = super::fake::start().await;
        fake.page("gym.example", "/robots.txt", "User-agent: *\nDisallow: /contact\nAllow: /contact/hours$\n\nUser-agent: OtherBot\nDisallow: /");
        fake.page("gym.example", "/", r#"<a href="/contact">Contact</a><a href="/about.html">About</a>"#);
        fake.page("gym.example", "/contact", "owner.name@gym.example");
        fake.page("gym.example", "/about.html", "Questions? hello@gym.example");
        let found = Sites::new(&base).find("gym.example").await;
        assert_eq!(found.addresses, [role("hello@gym.example")]);
        assert!(!fake.fetched("gym.example", "/contact"));

        fake.page("closed.example", "/robots.txt", "User-agent: Splee\nDisallow: /\n\nUser-agent: *\nAllow: /");
        fake.page("closed.example", "/", "info@closed.example");
        let found = Sites::new(&base).find("closed.example").await;
        assert!(found.blocked && found.addresses.is_empty());
        assert!(!fake.fetched("closed.example", "/"));

        let found = Sites::new(&base).find("missing.example").await;
        assert!(!found.read && found.addresses.is_empty());
    }

    #[test]
    fn robots_rules_match_like_google_reads_them() {
        let robots = Robots::parse("User-agent: *\nDisallow: /private\nDisallow: /*.php$\nAllow: /private/contact\nDisallow:\n");
        assert!(!robots.allows("/private/team"));
        assert!(robots.allows("/private/contact"));
        assert!(!robots.allows("/contact.php"));
        assert!(robots.allows("/contact.php?x=1"));
        assert!(robots.allows("/about"));
        assert!(Robots::parse("").allows("/"));
    }
}
