//! Company news from each company's own blog or newsroom feed: the RSS or Atom feed its homepage links to, else one at a common
//! path. The newest posts are read, and those whose title or summary says the company raised money, launched something, bought a
//! company, opened an office or hired a leader become signals. Keyword lists, no Claude.

use std::time::Duration;

use chrono::NaiveDate;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use reqwest::Url;
use sqlx::PgPool;

use super::Found;

const TIMEOUT: Duration = Duration::from_secs(8);
const USER_AGENT: &str = "Mozilla/5.0 (compatible; Splee news check)";
const MAX_BYTES: usize = 2 << 20;
/// The newest posts read from a feed. A company that posts more than this between two checks a week apart is rare.
const MAX_ITEMS: usize = 20;
/// Tried in order when the homepage links no feed.
const PATHS: [&str; 3] = ["/feed", "/rss.xml", "/blog/rss.xml"];
/// A domain with no feed is looked at again after this.
const RETRY_MISSING_DAYS: i32 = 30;
/// Enough of a post's summary to classify it. The rest is usually the post itself.
const SUMMARY_CHARS: usize = 500;

pub struct Feeds {
    client: reqwest::Client,
    /// The homepage URL template, with {domain}.
    site: String,
    /// Off for a fake site on this machine, in tests and sandboxes.
    public_only: bool,
}

impl Feeds {
    /// `base` is empty for the companies' real sites, else a fake that serves each one under /site/{domain}.
    pub fn new(base: &str) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder().user_agent(USER_AGENT).timeout(TIMEOUT).connect_timeout(Duration::from_secs(4)).build()?;
        let (site, public_only) = if base.is_empty() { ("https://{domain}".to_string(), true) } else { (format!("{base}/site/{{domain}}"), false) };
        Ok(Self { client, site, public_only })
    }

    /// The page's text and the URL it ended up at, through the same public-address checks as the tech scan.
    async fn get(&self, url: &str) -> Option<(String, String)> {
        let mut res = crate::hooks::get_page(&self.client, url, self.public_only, USER_AGENT, TIMEOUT, &|_| true).await?;
        let at = res.url().to_string();
        let mut body = Vec::new();
        while let Ok(Some(chunk)) = res.chunk().await {
            body.extend_from_slice(&chunk);
            if body.len() >= MAX_BYTES {
                break;
            }
        }
        body.truncate(MAX_BYTES);
        Some((String::from_utf8_lossy(&body).into_owned(), at))
    }

    /// The feed the homepage links to, or the first common path that serves one. None when there's neither.
    pub async fn discover(&self, domain: &str) -> Option<String> {
        let www = (!domain.starts_with("www.")).then(|| format!("www.{domain}"));
        let mut home = None;
        for host in std::iter::once(domain).chain(www.as_deref()) {
            let url = self.site.replace("{domain}", host);
            if let Some((html, at)) = self.get(&url).await {
                home = Some(url);
                if let Some(feed) = linked_feed(&html, &at) {
                    return Some(feed);
                }
                break;
            }
        }
        let home = home.unwrap_or_else(|| self.site.replace("{domain}", domain));
        for path in PATHS {
            let url = format!("{}{path}", home.trim_end_matches('/'));
            if let Some((body, _)) = self.get(&url).await
                && parse(&body).is_some()
            {
                return Some(url);
            }
        }
        None
    }

    /// The feed's newest items, or an error when it doesn't answer with a feed.
    pub async fn items(&self, url: &str) -> Result<Vec<Item>, String> {
        let (body, at) = self.get(url).await.ok_or("the feed didn't answer")?;
        let mut items = parse(&body).ok_or("the feed isn't RSS or Atom")?;
        let base = Url::parse(&at).ok();
        for item in &mut items {
            if let Some(link) = base.as_ref().and_then(|b| b.join(&item.link).ok()) {
                item.link = link.to_string();
            }
        }
        items.truncate(MAX_ITEMS);
        Ok(items)
    }
}

/// The domain's flagged posts from the last `window` days before `today`. The feed's URL is kept in company_feeds, so it's only
/// looked for once a month while there's none. A feed that stops answering is looked for again on the next check.
pub async fn find(db: &PgPool, feeds: &Feeds, domain: &str, today: NaiveDate, window: i64) -> anyhow::Result<Vec<Found>> {
    let cached: Option<(String, bool)> =
        sqlx::query_as("select feed_url, found_at > now() - make_interval(days => $2) from company_feeds where domain = $1")
            .bind(domain)
            .bind(RETRY_MISSING_DAYS)
            .fetch_optional(db)
            .await?;
    let url = match cached {
        Some((url, _)) if !url.is_empty() => url,
        Some((_, true)) => return Ok(vec![]),
        _ => {
            let url = feeds.discover(domain).await.unwrap_or_default();
            sqlx::query(
                "insert into company_feeds (domain, feed_url) values ($1, $2) on conflict (domain) do update set feed_url = excluded.feed_url, found_at = now()",
            )
            .bind(domain)
            .bind(&url)
            .execute(db)
            .await?;
            if url.is_empty() {
                return Ok(vec![]);
            }
            url
        }
    };
    let items = match feeds.items(&url).await {
        Ok(items) => items,
        Err(e) => {
            sqlx::query("delete from company_feeds where domain = $1").bind(domain).execute(db).await?;
            anyhow::bail!("{e}");
        }
    };
    let since = today - chrono::Duration::days(window);
    Ok(items
        .into_iter()
        .filter(|i| i.date.is_some_and(|d| d > since && d <= today + chrono::Duration::days(1)) && !i.link.is_empty())
        .filter_map(|i| {
            let kind = classify(&i.title, &i.summary)?;
            Some(Found {
                kind,
                key: format!("feed:{}", i.link),
                happened_on: i.date?,
                title: i.title,
                amount: None,
                offered: None,
                industry: String::new(),
                url: i.link,
            })
        })
        .collect())
}

/// The first feed a page's <link rel="alternate"> tags point to, made absolute. A comments feed is skipped.
pub fn linked_feed(html: &str, page: &str) -> Option<String> {
    let base = Url::parse(page).ok()?;
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(start) = lower[at..].find("<link").map(|i| at + i) {
        let end = lower[start..].find('>').map_or(lower.len(), |i| start + i);
        let tag = &html[start..end];
        at = end;
        let rel = attribute(tag, "rel").unwrap_or_default().to_ascii_lowercase();
        let kind = attribute(tag, "type").unwrap_or_default().to_ascii_lowercase();
        let Some(href) = attribute(tag, "href") else { continue };
        let is_feed = matches!(kind.trim(), "application/rss+xml" | "application/atom+xml");
        if rel.split_whitespace().any(|r| r == "alternate") && is_feed && !href.contains("/comments/") {
            return base.join(&href.replace("&amp;", "&")).ok().map(String::from);
        }
    }
    None
}

/// An attribute's value in a tag, quoted or not.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name).map(|i| from + i) {
        from = i + name.len();
        let before = lower[..i].chars().last();
        let rest = lower[from..].trim_start();
        if !before.is_some_and(char::is_whitespace) || !rest.starts_with('=') {
            continue;
        }
        let value = tag[tag.len() - rest.len() + 1..].trim_start();
        return Some(match value.chars().next() {
            Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or_default().to_string(),
            _ => value.split_whitespace().next().unwrap_or_default().to_string(),
        });
    }
    None
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Item {
    pub title: String,
    pub link: String,
    pub date: Option<NaiveDate>,
    pub summary: String,
}

/// The items of an RSS 2.0, RSS 1.0 (RDF) or Atom feed, in feed order. None when the document isn't one.
pub fn parse(xml: &str) -> Option<Vec<Item>> {
    let mut reader = Reader::from_str(xml);
    // Feeds are often hand-made, and a mismatched tag far down shouldn't lose the items above it.
    reader.config_mut().check_end_names = false;
    let (mut root, mut items) = (false, vec![]);
    let mut item: Option<Item> = None;
    // Atom's updated date, used when an entry has no published one.
    let mut updated: Option<NaiveDate> = None;
    let (mut depth, mut item_depth) = (0usize, 0usize);
    let (mut field, mut text) = (String::new(), String::new());
    loop {
        let event = reader.read_event();
        match event {
            Ok(Event::Start(ref e) | Event::Empty(ref e)) => {
                let name = e.local_name().as_ref().to_string();
                let empty = matches!(event, Ok(Event::Empty(_)));
                if !root {
                    if !matches!(name.as_str(), "rss" | "feed" | "RDF") {
                        return None;
                    }
                    root = true;
                }
                if matches!(name.as_str(), "item" | "entry") && item.is_none() {
                    item = Some(Item::default());
                    updated = None;
                    item_depth = depth + 1;
                } else if let Some(it) = item.as_mut()
                    && depth == item_depth
                {
                    // Atom's link is an attribute: the alternate one, or the first without a rel.
                    if name == "link" && it.link.is_empty() && let Some(href) = atom_link(e) {
                        it.link = href;
                    }
                    field = name;
                    text.clear();
                }
                if !empty {
                    depth += 1;
                }
            }
            Ok(Event::Text(t)) => text.push_str(&t.xml10_content()),
            Ok(Event::CData(t)) => text.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(r)) => text.push_str(&super::entity(&r)),
            Ok(Event::End(_)) => {
                depth = depth.saturating_sub(1);
                if let Some(it) = item.as_mut() {
                    if depth == item_depth && !field.is_empty() {
                        fill(it, &mut updated, &field, text.trim());
                        field.clear();
                    } else if depth + 1 == item_depth {
                        let mut done = item.take().unwrap_or_default();
                        done.date = done.date.or(updated.take());
                        items.push(done);
                    }
                }
                text.clear();
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    root.then_some(items)
}

fn atom_link(e: &BytesStart) -> Option<String> {
    let attr = |key: &str| {
        e.attributes().flatten().find(|a| a.key.as_ref() == key).and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok()).map(|v| v.into_owned())
    };
    let rel = attr("rel").unwrap_or_default();
    (rel.is_empty() || rel == "alternate").then(|| attr("href")).flatten()
}

fn fill(item: &mut Item, updated: &mut Option<NaiveDate>, field: &str, text: &str) {
    match field {
        "title" if item.title.is_empty() => item.title = text.split_whitespace().collect::<Vec<_>>().join(" "),
        "link" if item.link.is_empty() => item.link = text.to_string(),
        "pubDate" | "published" | "date" | "issued" if item.date.is_none() => item.date = date(text),
        "updated" | "modified" if updated.is_none() => *updated = date(text),
        "description" | "summary" | "encoded" | "content" if item.summary.is_empty() => {
            item.summary = strip_tags(text).chars().take(SUMMARY_CHARS).collect();
        }
        _ => {}
    }
}

/// RSS's RFC 2822 dates, Atom's RFC 3339 ones, or a bare 2026-10-03.
fn date(text: &str) -> Option<NaiveDate> {
    chrono::DateTime::parse_from_rfc2822(text)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(text))
        .map(|d| d.date_naive())
        .ok()
        .or_else(|| NaiveDate::parse_from_str(text.get(..10)?, "%Y-%m-%d").ok())
}

/// A summary's text without its HTML. Feeds put escaped HTML in descriptions, which is plain markup once unescaped.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What each kind of post says, in English, German, French, Spanish, Portuguese, Italian and Dutch. Matched as whole words, and
/// a phrase that ends in a currency sign matches before an amount, like "raises $12M". Funding comes first, so "raises $50M to
/// fund acquisitions" is funding, and launches last, since products are launched in posts about everything else too. Words that
/// blogs use for other things, like "customer acquisition" or "how to launch", are left out on purpose.
const KEYWORDS: [(&str, &[&str]); 5] = [
    ("funding_post", &[
        "raises $", "raises €", "raises £", "raised $", "raised €", "raised £", "in funding", "funding round", "round of funding", "seed round",
        "seed funding", "pre seed", "series a", "series b", "series c", "series d", "series e", "growth round",
        "finanzierungsrunde", "millionen euro", "levée de fonds", "lève", "tour de table", "ronda de financiación", "ronda de inversión",
        "recauda", "rodada de investimento", "rodada de financiamento", "capta", "round di finanziamento", "raccoglie", "financieringsronde",
    ]),
    ("acquisition", &[
        "acquires", "acquired", "acquisition of", "announces acquisition", "completes acquisition", "to acquire", "merger", "merges with", "übernimmt", "übernahme", "acquiert", "rachète",
        "rachat", "adquiere", "adquisición", "adquire", "aquisição", "acquisisce", "acquisizione", "neemt over", "overname",
    ]),
    ("leadership", &[
        "appoints", "appointed", "new ceo", "new cto", "new cfo", "new coo", "new cro", "new cmo", "joins as", "chief executive",
        "chief technology officer", "chief revenue officer", "chief financial officer", "vp of sales", "head of sales", "ernennt",
        "neuer ceo", "neue ceo", "geschäftsführer", "nomme", "nouveau directeur", "nombra", "nombramiento", "nuevo director",
        "nomeia", "novo diretor", "nomina", "nuovo direttore", "benoemt", "benoeming", "nieuwe ceo",
    ]),
    ("expansion", &[
        "new office", "opens office", "opens an office", "expands to", "expands into", "expansion", "new headquarters",
        "new market", "neues büro", "expandiert", "nouveau bureau", "ouvre un bureau", "s implante", "nueva oficina", "expansión",
        "se expande", "novo escritório", "expansão", "nuovo ufficio", "espansione", "nieuw kantoor", "uitbreiding",
    ]),
    ("launch", &[
        "launches", "launched", "launching", "introducing", "introduces", "unveils", "now available", "generally available",
        "startet", "stellt vor", "lance son", "lance sa", "lance une", "lance un", "lancement", "lanza", "lanzamiento", "lança", "lançamento", "lancia", "lancio", "lanceert",
        "introduceert",
    ]),
];

/// The post's kind from its title, else from its summary, or None when it says none of them.
pub fn classify(title: &str, summary: &str) -> Option<&'static str> {
    [title, summary].into_iter().map(normalise).find_map(|text| {
        KEYWORDS.iter().find(|(_, words)| words.iter().any(|w| says(&text, w))).map(|(kind, _)| *kind)
    })
}

/// Lowercase words between single spaces, with a space at each end. Currency signs stay, as words of their own when they
/// follow a space, so "$12M" is "$12m".
fn normalise(text: &str) -> String {
    let kept: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() || "$€£".contains(c) { c } else { ' ' }).collect();
    format!(" {} ", kept.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn says(text: &str, keyword: &str) -> bool {
    let k = normalise(keyword);
    let k = k.trim();
    if k.ends_with(['$', '€', '£']) { text.contains(&format!(" {k}")) } else { text.contains(&format!(" {k} ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/"><channel><title>Acme blog</title><link>https://acme.example/blog</link>
<item><title>Acme raises $12M Series A to grow its &amp; team</title><link>/blog/series-a</link><pubDate>Sat, 03 Oct 2026 09:00:00 GMT</pubDate>
<description><![CDATA[<p>We closed our round.</p>]]></description></item>
<item><title>Five tips for spring</title><link>https://acme.example/blog/tips</link><pubDate>Fri, 02 Oct 2026 09:00:00 +0000</pubDate>
<description>&lt;p&gt;Nothing new, but we &lt;b&gt;opened an office in Austin&lt;/b&gt;: our new office.&lt;/p&gt;</description></item>
</channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>News</title><link href="https://acme.example/news" rel="self"/>
<entry><title type="html">Introducing Acme Flow</title><link rel="edit" href="https://acme.example/edit/1"/>
<link rel="alternate" href="https://acme.example/news/flow"/><id>tag:acme,1</id><updated>2026-10-10T08:00:00Z</updated>
<published>2026-10-09T08:00:00+02:00</published><summary>A new way to ship.</summary></entry>
<entry><title>Acme appoints Dana Reyes as CEO</title><link href="https://acme.example/news/ceo"/><updated>2026-09-01T00:00:00Z</updated></entry>
</feed>"#;

    #[test]
    fn rss_items_are_read_with_their_dates_and_text() {
        let items = parse(RSS).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Acme raises $12M Series A to grow its & team");
        assert_eq!((items[0].link.as_str(), items[0].date.map(|d| d.to_string())), ("/blog/series-a", Some("2026-10-03".into())));
        assert_eq!(items[0].summary, "We closed our round.");
        assert_eq!(items[1].summary, "Nothing new, but we opened an office in Austin : our new office.");
    }

    #[test]
    fn atom_entries_take_the_alternate_link_and_the_published_date() {
        let items = parse(ATOM).unwrap();
        assert_eq!(items[0], Item {
            title: "Introducing Acme Flow".into(),
            link: "https://acme.example/news/flow".into(),
            date: NaiveDate::from_ymd_opt(2026, 10, 9),
            summary: "A new way to ship.".into(),
        });
        assert_eq!((items[1].link.as_str(), items[1].date), ("https://acme.example/news/ceo", NaiveDate::from_ymd_opt(2026, 9, 1)));
        assert!(parse("<html><body>Not a feed</body></html>").is_none());
        assert!(parse("garbage").is_none());
    }

    #[test]
    fn the_homepage_link_gives_the_feed() {
        let html = r#"<head><link rel="stylesheet" href="/a.css"><LINK REL="alternate" TYPE="application/rss+xml" title="Comments" href="/comments/feed/">
            <link type='application/atom+xml' rel='alternate' href='/news/atom.xml?x=1&amp;y=2'></head>"#;
        assert_eq!(linked_feed(html, "https://www.acme.example/").as_deref(), Some("https://www.acme.example/news/atom.xml?x=1&y=2"));
        assert_eq!(linked_feed(r#"<link rel=alternate type=application/rss+xml href=https://feeds.acme.example/blog>"#, "https://acme.example/").as_deref(),
                   Some("https://feeds.acme.example/blog"));
        assert_eq!(linked_feed(r#"<link rel="alternate" hreflang="de" href="/de/">"#, "https://acme.example/"), None);
    }

    #[test]
    fn each_kind_is_told_apart_by_its_words() {
        assert_eq!(classify("Acme raises $12M to expand into Europe", ""), Some("funding_post"));
        assert_eq!(classify("We closed our Series B", ""), Some("funding_post"));
        assert_eq!(classify("Acme lève 5 millions d'euros", ""), Some("funding_post"));
        assert_eq!(classify("Acme acquires Brightline", ""), Some("acquisition"));
        assert_eq!(classify("Acme übernimmt Brightline", ""), Some("acquisition"));
        assert_eq!(classify("Acme appoints Dana Reyes as Chief Revenue Officer", ""), Some("leadership"));
        assert_eq!(classify("Acme nombra nuevo director comercial", ""), Some("leadership"));
        assert_eq!(classify("Hello Austin: our new office", ""), Some("expansion"));
        assert_eq!(classify("Acme opent een nieuw kantoor in Gent", ""), Some("expansion"));
        assert_eq!(classify("Introducing Acme Flow", ""), Some("launch"));
        assert_eq!(classify("Acme lanza su nueva app", ""), Some("launch"));
        // The title decides before the summary, and words only count whole.
        assert_eq!(classify("Introducing Acme Flow", "Acme raised $12M last year"), Some("launch"));
        assert_eq!(classify("Product update", "We launched exports"), Some("launch"));
        assert_eq!(classify("Ten customer acquisition tips", ""), None);
        assert_eq!(classify("Five tips for spring", "Raise your hand if you love lunch"), None);
        assert_eq!(classify("Our relaunched site", ""), None);
    }
}
