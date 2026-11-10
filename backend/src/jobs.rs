//! Open roles from a company's public job board. Greenhouse, Lever, Ashby, Workable and Recruitee each publish a company's
//! board as JSON by the company's name on the board (its slug), with no key. A domain's board is found from links on its
//! homepage, /careers or /jobs, or else by trying a slug made from the domain, and kept in job_boards.

use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Greenhouse,
    Lever,
    Ashby,
    Workable,
    Recruitee,
}

impl Provider {
    const ALL: [Provider; 5] = [Provider::Greenhouse, Provider::Lever, Provider::Ashby, Provider::Workable, Provider::Recruitee];

    pub fn name(self) -> &'static str {
        match self {
            Provider::Greenhouse => "greenhouse",
            Provider::Lever => "lever",
            Provider::Ashby => "ashby",
            Provider::Workable => "workable",
            Provider::Recruitee => "recruitee",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    pub provider: Provider,
    pub slug: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Opening {
    pub title: String,
    pub location: String,
    pub department: String,
    pub url: String,
    pub posted_at: Option<DateTime<Utc>>,
    /// The start of the post as plain text. Workable's list has no description, so theirs are empty.
    pub excerpt: String,
}

/// Pages and boards that take longer than this are skipped. The worker checks hundreds of domains a day.
const TIMEOUT: Duration = Duration::from_secs(8);
const USER_AGENT: &str = "Mozilla/5.0 (compatible; Splee job board check)";
/// Greenhouse lists every post's full text, which runs to a few megabytes for the biggest boards.
const MAX_BOARD_BYTES: usize = 16 << 20;
const MAX_PAGE_BYTES: usize = 2 << 20;
const EXCERPT_CHARS: usize = 300;
/// A domain with no board is looked at again after this, in case it has started hiring through one.
const RETRY_MISSING_DAYS: i32 = 14;

pub struct Boards {
    client: reqwest::Client,
    /// URL templates with {slug}, and {domain} for the site.
    greenhouse: String,
    lever: String,
    ashby: String,
    workable: String,
    recruitee: String,
    site: String,
    /// Company sites are whatever domain a lead has, so off in tests only, whose fake site is on this machine.
    public_only: bool,
}

impl Boards {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().user_agent(USER_AGENT).timeout(TIMEOUT).connect_timeout(Duration::from_secs(4)).build()?,
            greenhouse: "https://boards-api.greenhouse.io/v1/boards/{slug}/jobs?content=true".into(),
            lever: "https://api.lever.co/v0/postings/{slug}?mode=json".into(),
            ashby: "https://api.ashbyhq.com/posting-api/job-board/{slug}".into(),
            workable: "https://apply.workable.com/api/v1/widget/accounts/{slug}".into(),
            recruitee: "https://{slug}.recruitee.com/api/offers/".into(),
            site: "https://{domain}".into(),
            public_only: true,
        })
    }

    /// Every board and site on one server, for tests.
    #[cfg(test)]
    pub fn at(base: &str) -> Self {
        Self {
            client: reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap(),
            greenhouse: format!("{base}/greenhouse/{{slug}}"),
            lever: format!("{base}/lever/{{slug}}"),
            ashby: format!("{base}/ashby/{{slug}}"),
            workable: format!("{base}/workable/{{slug}}"),
            recruitee: format!("{base}/recruitee/{{slug}}"),
            site: format!("{base}/site/{{domain}}"),
            public_only: false,
        }
    }

    fn board_url(&self, board: &Board) -> String {
        let template = match board.provider {
            Provider::Greenhouse => &self.greenhouse,
            Provider::Lever => &self.lever,
            Provider::Ashby => &self.ashby,
            Provider::Workable => &self.workable,
            Provider::Recruitee => &self.recruitee,
        };
        template.replace("{slug}", &board.slug)
    }

    /// The board's open roles. None when the board doesn't exist, an error when it couldn't be read.
    pub async fn list(&self, board: &Board) -> Result<Option<Vec<Opening>>, String> {
        let res = self.client.get(self.board_url(board)).send().await.map_err(|e| e.without_url().to_string())?;
        if matches!(res.status().as_u16(), 404 | 410) {
            return Ok(None);
        }
        if !res.status().is_success() {
            return Err(format!("{} answered {}", board.provider.name(), res.status()));
        }
        let body = read_capped(res, MAX_BOARD_BYTES).await?;
        let json: Value = serde_json::from_slice(&body).map_err(|e| format!("{} sent something that isn't a board: {e}", board.provider.name()))?;
        Ok(parse(board.provider, &json))
    }

    /// Looks for a board linked from the site first, then for one named like the domain. The first board that answers wins,
    /// even an empty one: a company between hires still uses it.
    pub async fn detect(&self, domain: &str) -> Option<(Board, Vec<Opening>)> {
        let mut tried: Vec<Board> = vec![];
        for path in ["", "/careers", "/jobs"] {
            let Some(html) = self.page(&format!("{}{path}", self.site.replace("{domain}", domain))).await else { continue };
            for board in links(&html) {
                if tried.contains(&board) {
                    continue;
                }
                if let Ok(Some(openings)) = self.list(&board).await {
                    return Some((board, openings));
                }
                tried.push(board);
            }
        }
        for slug in slug_guesses(domain) {
            for provider in Provider::ALL {
                let board = Board { provider, slug: slug.clone() };
                if tried.contains(&board) {
                    continue;
                }
                if let Ok(Some(openings)) = self.list(&board).await {
                    return Some((board, openings));
                }
            }
        }
        None
    }

    /// A page of the company's site. Each hop of a redirect is checked to be a public address before it's fetched, so a lead's
    /// domain can't point the server at its own network.
    async fn page(&self, url: &str) -> Option<String> {
        let mut url = reqwest::Url::parse(url).ok()?;
        for _ in 0..4 {
            let client = if self.public_only { crate::hooks::public_client(&url).await.ok()? } else { self.client.clone() };
            let res = client.get(url.clone()).header(reqwest::header::USER_AGENT, USER_AGENT).timeout(TIMEOUT).send().await.ok()?;
            if res.status().is_redirection() {
                url = url.join(res.headers().get(reqwest::header::LOCATION)?.to_str().ok()?).ok()?;
                continue;
            }
            if !res.status().is_success() {
                return None;
            }
            let body = read_capped(res, MAX_PAGE_BYTES).await.ok()?;
            return Some(String::from_utf8_lossy(&body).into_owned());
        }
        None
    }
}

async fn read_capped(mut res: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| e.without_url().to_string())? {
        if body.len() + chunk.len() > max {
            return Err(format!("the response is over {} MB", max >> 20));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The domain's open roles, from the board cached for it or a newly detected one. None when it has no board we can read.
/// A cached board that's gone is detected again, since companies move between job board providers.
pub async fn openings(db: &PgPool, boards: &Boards, domain: &str) -> anyhow::Result<Option<(Board, Vec<Opening>)>> {
    let cached: Option<(String, String, bool)> = sqlx::query_as(&format!(
        "select provider, slug, detected_at > now() - interval '{RETRY_MISSING_DAYS} days' from job_boards where domain = $1"
    ))
    .bind(domain)
    .fetch_optional(db)
    .await?;
    if let Some((provider, slug, fresh)) = cached {
        match Provider::parse(&provider) {
            Some(provider) => {
                let board = Board { provider, slug };
                let listed = boards.list(&board).await.map_err(|e| anyhow::anyhow!("could not read the {} board {}: {e}", provider.name(), board.slug))?;
                if let Some(openings) = listed {
                    return Ok(Some((board, openings)));
                }
            }
            None if fresh => return Ok(None),
            None => {}
        }
    }
    let found = boards.detect(domain).await;
    let (provider, slug) = found.as_ref().map_or(("", ""), |(b, _)| (b.provider.name(), b.slug.as_str()));
    sqlx::query(
        "insert into job_boards (domain, provider, slug) values ($1, $2, $3)
         on conflict (domain) do update set provider = excluded.provider, slug = excluded.slug, detected_at = now()",
    )
    .bind(domain)
    .bind(provider)
    .bind(slug)
    .execute(db)
    .await?;
    Ok(found)
}

/// Job board links in a page: the hosted boards, Greenhouse's embed script and the boards' own subdomains.
pub fn links(html: &str) -> Vec<Board> {
    const AFTER: [(&str, Provider); 9] = [
        ("boards.greenhouse.io/embed/job_board/js?for=", Provider::Greenhouse),
        ("boards.greenhouse.io/embed/job_board?for=", Provider::Greenhouse),
        ("boards-api.greenhouse.io/v1/boards/", Provider::Greenhouse),
        ("job-boards.greenhouse.io/", Provider::Greenhouse),
        ("boards.greenhouse.io/", Provider::Greenhouse),
        ("jobs.lever.co/", Provider::Lever),
        ("jobs.ashbyhq.com/", Provider::Ashby),
        ("apply.workable.com/", Provider::Workable),
        ("jobs.workable.com/company/", Provider::Workable),
    ];
    const BEFORE: [(&str, Provider); 2] = [(".recruitee.com", Provider::Recruitee), (".workable.com", Provider::Workable)];
    // Paths on those hosts that aren't a company's board.
    const NOT_SLUGS: [&str; 8] = ["embed", "api", "j", "v1", "apply", "www", "jobs", "careers"];

    // Lowercasing ASCII keeps byte offsets, so slugs are cut from the original text with their case.
    let lower = html.to_ascii_lowercase();
    let slug_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let mut found: Vec<Board> = vec![];
    let mut add = |provider: Provider, slug: &str| {
        let board = Board { provider, slug: slug.to_string() };
        if !slug.is_empty() && !NOT_SLUGS.contains(&slug.to_ascii_lowercase().as_str()) && !found.contains(&board) {
            found.push(board);
        }
    };
    for (marker, provider) in AFTER {
        for (at, _) in lower.match_indices(marker) {
            // boards.greenhouse.io/ also matches inside job-boards.greenhouse.io/, which the earlier marker already read.
            if marker == "boards.greenhouse.io/" && lower[..at].ends_with("job-") {
                continue;
            }
            let rest = &html[at + marker.len()..];
            add(provider, &rest[..rest.find(|c: char| !slug_char(c)).unwrap_or(rest.len())]);
        }
    }
    for (marker, provider) in BEFORE {
        for (at, _) in lower.match_indices(marker) {
            let head = &html[..at];
            add(provider, &head[head.rfind(|c: char| !slug_char(c)).map_or(0, |i| i + 1)..]);
        }
    }
    found
}

/// Board names to try for a domain: acme-labs.co.uk gives acme-labs and acmelabs, and getacme.com and acmehq.com give acme
/// too, since companies often register a longer name than the one they use on their board.
fn slug_guesses(domain: &str) -> Vec<String> {
    let name = domain.trim_start_matches("www.").split('.').next().unwrap_or_default().to_ascii_lowercase();
    let joined = name.replace('-', "");
    let mut core = joined.as_str();
    for prefix in ["the", "get", "try", "use", "join"] {
        core = core.strip_prefix(prefix).filter(|rest| rest.len() >= 4).unwrap_or(core);
    }
    for suffix in ["hq", "app"] {
        core = core.strip_suffix(suffix).filter(|rest| rest.len() >= 4).unwrap_or(core);
    }
    let mut guesses: Vec<String> = vec![];
    for guess in [name.as_str(), joined.as_str(), core] {
        if guess.len() >= 2 && !guesses.iter().any(|g| g == guess) {
            guesses.push(guess.to_string());
        }
    }
    guesses
}

/// Reads one provider's board. None when it isn't shaped like one.
fn parse(provider: Provider, json: &Value) -> Option<Vec<Opening>> {
    let text = |v: &Value| v.as_str().unwrap_or_default().trim().to_string();
    let list = match provider {
        Provider::Lever => json.as_array()?,
        Provider::Recruitee => json["offers"].as_array()?,
        _ => json["jobs"].as_array()?,
    };
    let openings = list.iter().filter_map(|j| {
        let opening = match provider {
            Provider::Greenhouse => Opening {
                title: text(&j["title"]),
                location: text(&j["location"]["name"]),
                department: text(&j["departments"][0]["name"]),
                url: text(&j["absolute_url"]),
                posted_at: rfc3339(&j["first_published"]).or_else(|| rfc3339(&j["updated_at"])),
                // Greenhouse escapes the post's HTML once more.
                excerpt: excerpt(&plain(&unescape(j["content"].as_str().unwrap_or_default()))),
            },
            Provider::Lever => Opening {
                title: text(&j["text"]),
                location: text(&j["categories"]["location"]),
                department: [&j["categories"]["department"], &j["categories"]["team"]].into_iter().map(text).find(|s| !s.is_empty()).unwrap_or_default(),
                url: text(&j["hostedUrl"]),
                posted_at: j["createdAt"].as_i64().and_then(|ms| Utc.timestamp_millis_opt(ms).single()),
                excerpt: excerpt(j["descriptionPlain"].as_str().unwrap_or_default()),
            },
            Provider::Ashby => {
                if j["isListed"].as_bool() == Some(false) {
                    return None;
                }
                Opening {
                    title: text(&j["title"]),
                    location: text(&j["location"]),
                    department: text(&j["department"]),
                    url: text(&j["jobUrl"]),
                    posted_at: rfc3339(&j["publishedAt"]),
                    excerpt: excerpt(j["descriptionPlain"].as_str().unwrap_or_default()),
                }
            }
            Provider::Workable => Opening {
                title: text(&j["title"]),
                location: [&j["city"], &j["country"]].into_iter().map(text).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
                department: text(&j["department"]),
                url: text(&j["url"]),
                posted_at: date(&j["published_on"]).or_else(|| date(&j["created_at"])),
                excerpt: String::new(),
            },
            Provider::Recruitee => {
                if j["status"].as_str().is_some_and(|s| s != "published") {
                    return None;
                }
                Opening {
                    title: text(&j["title"]),
                    location: text(&j["location"]),
                    department: text(&j["department"]),
                    url: text(&j["careers_url"]),
                    posted_at: recruitee_time(&j["published_at"]).or_else(|| recruitee_time(&j["created_at"])),
                    excerpt: excerpt(&plain(j["description"].as_str().unwrap_or_default())),
                }
            }
        };
        (!opening.title.is_empty() && opening.url.starts_with("http")).then_some(opening)
    });
    Some(openings.collect())
}

fn rfc3339(v: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(v.as_str()?).ok().map(|t| t.with_timezone(&Utc))
}

/// Workable's dates, like 2026-02-12.
fn date(v: &Value) -> Option<DateTime<Utc>> {
    NaiveDate::parse_from_str(v.as_str()?, "%Y-%m-%d").ok().map(|d| d.and_time(NaiveTime::MIN).and_utc())
}

/// Recruitee's times, like 2026-11-06 17:01:19 UTC.
fn recruitee_time(v: &Value) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(v.as_str()?, "%Y-%m-%d %H:%M:%S UTC").ok().map(|t| t.and_utc())
}

/// HTML as text, with entities as their characters. Block tags become spaces, so paragraphs don't run together, and inline
/// ones vanish, so "<b>Note</b>:" stays "Note:".
fn plain(html: &str) -> String {
    const BLOCKS: [&str; 13] = ["p", "br", "div", "li", "ul", "ol", "h1", "h2", "h3", "h4", "h5", "h6", "tr"];
    let mut text = String::with_capacity(html.len());
    let mut tag: Option<String> = None;
    for c in html.chars() {
        match (&mut tag, c) {
            (None, '<') => tag = Some(String::new()),
            (Some(name), '>') => {
                let name = name.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or_default().to_ascii_lowercase();
                if BLOCKS.contains(&name.as_str()) {
                    text.push(' ');
                }
                tag = None;
            }
            (Some(name), c) => name.push(c),
            (None, c) => text.push(c),
        }
    }
    unescape(&text)
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&rsquo;", "\u{2019}")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// The first EXCERPT_CHARS of the text with its whitespace collapsed, cut at a word.
fn excerpt(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
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

/// The roles in `roles` that the title is for. A role matches when its words appear in the title in order, so "AE" doesn't
/// match "Head of AEM" and "CTO" doesn't match "Director". A plural in the title still matches, like "SDRs".
pub fn matching_roles<'a>(title: &str, roles: &'a [String]) -> Vec<&'a str> {
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect()
    };
    let title = words(title);
    roles
        .iter()
        .filter(|role| {
            let role = words(role);
            !role.is_empty()
                && title.windows(role.len()).any(|w| w.iter().zip(&role).all(|(t, r)| t == r || t.strip_suffix('s') == Some(r.as_str())))
        })
        .map(|r| r.trim())
        .collect()
}

#[cfg(test)]
pub mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::response::{Html, IntoResponse, Response};
    use axum::routing::get;
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;

    /// Boards by "provider/slug" and site pages by "domain/path", served as the real ones are. Every request is counted by path.
    #[derive(Clone, Default)]
    pub struct Fake {
        pub boards: Arc<Mutex<HashMap<String, Value>>>,
        pub pages: Arc<Mutex<HashMap<String, String>>>,
        pub hits: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        pub fn board(&self, key: &str, body: Value) {
            self.boards.lock().unwrap().insert(key.into(), body);
        }

        pub fn remove_board(&self, key: &str) {
            self.boards.lock().unwrap().remove(key);
        }

        pub fn page(&self, key: &str, html: &str) {
            self.pages.lock().unwrap().insert(key.into(), html.into());
        }

        pub fn hits(&self, prefix: &str) -> usize {
            self.hits.lock().unwrap().iter().filter(|h| h.starts_with(prefix)).count()
        }
    }

    async fn board(State(fake): State<Fake>, Path((provider, slug)): Path<(String, String)>) -> Response {
        let key = format!("{provider}/{slug}");
        fake.hits.lock().unwrap().push(key.clone());
        match fake.boards.lock().unwrap().get(&key) {
            Some(body) => axum::Json(body.clone()).into_response(),
            None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
        }
    }

    async fn page(State(fake): State<Fake>, Path(rest): Path<String>) -> Response {
        fake.hits.lock().unwrap().push(format!("site/{rest}"));
        match fake.pages.lock().unwrap().get(&rest) {
            Some(html) => Html(html.clone()).into_response(),
            None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
        }
    }

    pub async fn serve() -> (Boards, Fake) {
        let fake = Fake::default();
        let app = Router::new()
            .route("/site/{*rest}", get(page))
            .route("/{provider}/{slug}", get(board))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (Boards::at(&base), fake)
    }

    pub fn greenhouse(jobs: &[(&str, &str)]) -> Value {
        let jobs: Vec<Value> = jobs
            .iter()
            .map(|(title, url)| {
                json!({
                    "id": 1, "title": title, "absolute_url": url, "location": {"name": "Remote, US"},
                    "updated_at": "2026-11-06T15:25:07-05:00", "first_published": "2026-10-22T13:38:40-04:00",
                    "departments": [{"id": 4, "name": "Sales"}],
                    "content": "&lt;p&gt;Join &amp;amp; build our &lt;strong&gt;outbound&lt;/strong&gt;.&lt;/p&gt;&lt;p&gt;Remote.&lt;/p&gt;"
                })
            })
            .collect();
        json!({"jobs": jobs, "meta": {"total": jobs.len()}})
    }

    #[tokio::test]
    async fn every_board_format_reads_into_openings() {
        let (boards, fake) = serve().await;
        fake.board("greenhouse/acme", greenhouse(&[("Sales Development Representative", "https://job-boards.greenhouse.io/acme/jobs/1")]));
        fake.board(
            "lever/acme",
            json!([{
                "id": "ad20", "text": "Account Executive", "hostedUrl": "https://jobs.lever.co/acme/ad20", "createdAt": 1788199939703i64,
                "categories": {"commitment": "Full time", "department": "", "team": "Revenue", "location": "Atlanta, Georgia"},
                "descriptionPlain": "We sell\u{a0}to   dentists.\n\nYou'll own the full cycle."
            }]),
        );
        fake.board(
            "ashby/acme",
            json!({"apiVersion": "1", "jobs": [
                {"title": "SDR", "department": "Sales", "location": "London", "isListed": true, "jobUrl": "https://jobs.ashbyhq.com/acme/7458",
                 "publishedAt": "2024-03-04T14:29:08.532+00:00", "descriptionPlain": "Book meetings."},
                {"title": "Hidden", "isListed": false, "jobUrl": "https://jobs.ashbyhq.com/acme/1"}
            ]}),
        );
        fake.board(
            "workable/acme",
            json!({"name": "Acme", "description": null, "jobs": [{
                "title": "Head of Sales", "shortcode": "186545F8C1", "department": "Commercial", "url": "https://apply.workable.com/j/186545F8C1",
                "published_on": "2026-02-12", "created_at": "2025-09-30", "country": "Greece", "city": "Athens"
            }]}),
        );
        fake.board(
            "recruitee/acme",
            json!({"offers": [
                {"title": "Sales Lead", "status": "published", "department": "Sales", "location": "Amsterdam, Netherlands",
                 "careers_url": "https://careers.acme.com/o/sales-lead", "published_at": "2026-11-06 17:01:19 UTC",
                 "description": "<p>Keeping users&#39; money safe &amp; sound.</p>"},
                {"title": "Old", "status": "closed", "careers_url": "https://careers.acme.com/o/old"}
            ]}),
        );

        let list = async |provider| boards.list(&Board { provider, slug: "acme".into() }).await;
        let gh = list(Provider::Greenhouse).await.unwrap().unwrap();
        assert_eq!(
            gh,
            vec![Opening {
                title: "Sales Development Representative".into(),
                location: "Remote, US".into(),
                department: "Sales".into(),
                url: "https://job-boards.greenhouse.io/acme/jobs/1".into(),
                posted_at: Some(Utc.with_ymd_and_hms(2026, 10, 22, 17, 38, 40).unwrap()),
                excerpt: "Join & build our outbound. Remote.".into(),
            }]
        );
        let lever = list(Provider::Lever).await.unwrap().unwrap();
        assert_eq!((lever[0].department.as_str(), lever[0].excerpt.as_str()), ("Revenue", "We sell to dentists. You'll own the full cycle."));
        assert_eq!(lever[0].posted_at, Utc.timestamp_millis_opt(1788199939703).single());
        let ashby = list(Provider::Ashby).await.unwrap().unwrap();
        assert_eq!(ashby.iter().map(|o| o.title.as_str()).collect::<Vec<_>>(), ["SDR"]);
        let workable = list(Provider::Workable).await.unwrap().unwrap();
        assert_eq!((workable[0].location.as_str(), workable[0].posted_at), ("Athens, Greece", Some(Utc.with_ymd_and_hms(2026, 2, 12, 0, 0, 0).unwrap())));
        let recruitee = list(Provider::Recruitee).await.unwrap().unwrap();
        assert_eq!(recruitee.len(), 1);
        assert_eq!(recruitee[0].excerpt, "Keeping users' money safe & sound.");
        assert_eq!(recruitee[0].posted_at, Some(Utc.with_ymd_and_hms(2026, 11, 6, 17, 1, 19).unwrap()));

        assert_eq!(boards.list(&Board { provider: Provider::Lever, slug: "nobody".into() }).await, Ok(None));
    }

    #[test]
    fn links_find_each_board_host() {
        let html = r#"<a href="https://job-boards.greenhouse.io/GitLab/jobs/1">Jobs</a>
            <script src="https://boards.greenhouse.io/embed/job_board/js?for=acme"></script>
            <a href="https://jobs.lever.co/leverdemo?team=x">Lever</a> <a href="https://jobs.ashbyhq.com/Ashby">Ashby</a>
            <a href="https://apply.workable.com/blueground/">W</a> <a href="https://apply.workable.com/j/186545F8C1">post</a>
            <a href='https://bunq.recruitee.com/o/security-lead'>R</a> <a href="https://www.recruitee.com">vendor</a>"#;
        let found: Vec<String> = links(html).iter().map(|b| format!("{}/{}", b.provider.name(), b.slug)).collect();
        assert_eq!(found, ["greenhouse/acme", "greenhouse/GitLab", "lever/leverdemo", "ashby/Ashby", "workable/blueground", "recruitee/bunq"]);
        assert!(links("<p>We're hiring! Email jobs@acme.com</p>").is_empty());
    }

    #[test]
    fn roles_match_whole_words_in_order() {
        let roles: Vec<String> = ["SDR", "sales development", "Account Executive", "CTO", " "].map(String::from).to_vec();
        assert_eq!(matching_roles("Senior Account Executive, EMEA", &roles), ["Account Executive"]);
        assert_eq!(matching_roles("Sales Development Representative (SDR)", &roles), ["SDR", "sales development"]);
        assert_eq!(matching_roles("Hiring SDRs in Austin", &roles), ["SDR"]);
        assert!(matching_roles("Director of Engineering", &roles).is_empty());
        assert!(matching_roles("Executive Account Manager", &roles).is_empty());
    }

    #[test]
    fn excerpts_stop_at_a_word() {
        let long = "word ".repeat(100);
        let cut = excerpt(&long);
        assert!(cut.ends_with("word…") && cut.chars().count() <= EXCERPT_CHARS + 1, "{cut}");
        assert_eq!(slug_guesses("acme-labs.co.uk"), ["acme-labs", "acmelabs"]);
        assert_eq!(slug_guesses("ashbyhq.com"), ["ashbyhq", "ashby"]);
        assert_eq!(slug_guesses("theblueground.com"), ["theblueground", "blueground"]);
        assert_eq!(slug_guesses("getapp.io"), ["getapp"]);
    }

    #[tokio::test]
    async fn company_sites_on_private_addresses_are_not_fetched() {
        let (mut boards, fake) = serve().await;
        fake.page("private.com", r#"<a href="https://jobs.lever.co/private">Jobs</a>"#);
        let url = boards.site.replace("{domain}", "private.com");
        assert!(boards.page(&url).await.is_some());
        // The fake site is on 127.0.0.1, like an internal service a lead's domain could point at.
        boards.public_only = true;
        assert!(boards.page(&url).await.is_none());
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn detection_reads_the_site_then_guesses_and_is_cached(db: PgPool) {
        let (boards, fake) = serve().await;
        // Linked from the careers page under a name that isn't the domain's.
        fake.page("linked.com/careers", r#"<a href="https://jobs.lever.co/linked-hq">Open roles</a>"#);
        fake.board("lever/linked-hq", json!([{"text": "SDR", "hostedUrl": "https://jobs.lever.co/linked-hq/1"}]));
        // No links anywhere, but the domain's name is a Recruitee board.
        fake.board("recruitee/guessed", json!({"offers": []}));

        let (board, listed) = openings(&db, &boards, "linked.com").await.unwrap().unwrap();
        assert_eq!((board.provider, board.slug.as_str(), listed.len()), (Provider::Lever, "linked-hq", 1));
        let (board, listed) = openings(&db, &boards, "guessed.io").await.unwrap().unwrap();
        assert_eq!((board.provider, listed.len()), (Provider::Recruitee, 0));
        assert!(openings(&db, &boards, "nothing.org").await.unwrap().is_none());
        let site_hits = fake.hits("site/");

        // Cached: the next look reads the board straight away, and a domain with none isn't searched again.
        openings(&db, &boards, "linked.com").await.unwrap().unwrap();
        assert!(openings(&db, &boards, "nothing.org").await.unwrap().is_none());
        assert_eq!(fake.hits("site/"), site_hits);
        assert_eq!(fake.hits("lever/linked-hq"), 2);

        // A board that's gone is looked for again, and the company's new one is found.
        fake.remove_board("lever/linked-hq");
        fake.page("linked.com", r#"<script src="https://boards.greenhouse.io/embed/job_board/js?for=linkedco"></script>"#);
        fake.board("greenhouse/linkedco", greenhouse(&[]));
        let (board, _) = openings(&db, &boards, "linked.com").await.unwrap().unwrap();
        assert_eq!((board.provider, board.slug.as_str()), (Provider::Greenhouse, "linkedco"));
        let cached: String = sqlx::query_scalar("select provider from job_boards where domain = 'linked.com'").fetch_one(&db).await.unwrap();
        assert_eq!(cached, "greenhouse");
    }
}
