//! Form D filings from SEC EDGAR: a US company raising money privately files one within 15 days of its first sale, with the
//! amount offered and sold. A company is looked up by name in EDGAR's full-text search, and a filing only counts when the
//! filer's legal name is the company's and its state or city agrees with what Splee knows. A one-word name, like Notion or
//! Linear, is shared by too many filers, so it also needs the state or city to agree.
//! EDGAR asks for a User-Agent that names the client with a contact, and at most 10 requests a second from a client.

use std::time::Duration;

use chrono::NaiveDate;
use quick_xml::Reader;
use quick_xml::events::Event;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::{Found, Target};

/// Requests this far apart at least, across every company checked at once: 4 a second, well under EDGAR's 10.
const GAP: Duration = Duration::from_millis(250);
const TIMEOUT: Duration = Duration::from_secs(10);
/// Filings searched back this far, so an amendment finds the filing it amends. Only the last 90 days count, see super::WINDOW_DAYS.
const SEARCH_DAYS: i64 = 365;
/// Offerings read per company. Each costs one more request, for its amounts.
const MAX_OFFERINGS: usize = 3;
const MAX_DOC_BYTES: usize = 1 << 20;

/// Spaces requests at least `gap` apart for everyone who shares it. Each caller takes the next free slot and sleeps until then.
pub struct Limiter {
    gap: Duration,
    next: Mutex<Instant>,
}

impl Limiter {
    pub fn new(gap: Duration) -> Self {
        Self { gap, next: Mutex::new(Instant::now()) }
    }

    pub async fn wait(&self) {
        let at = {
            let mut next = self.next.lock().await;
            let at = (*next).max(Instant::now());
            *next = at + self.gap;
            at
        };
        tokio::time::sleep_until(at).await;
    }
}

pub struct Edgar {
    client: reqwest::Client,
    /// Full-text search, and the filing archives, which also serve the links kept with each filing.
    search: String,
    archives: String,
    user_agent: String,
    limiter: Limiter,
}

impl Edgar {
    /// None without SEC_USER_AGENT: EDGAR turns away clients that don't say who they are. `base` is empty for EDGAR itself.
    pub fn new(user_agent: Option<&str>, base: &str) -> anyhow::Result<Option<Self>> {
        let Some(user_agent) = user_agent.map(str::trim).filter(|ua| !ua.is_empty()) else { return Ok(None) };
        let (search, archives) = if base.is_empty() {
            ("https://efts.sec.gov/LATEST/search-index".to_string(), "https://www.sec.gov/Archives/edgar/data".to_string())
        } else {
            (format!("{base}/LATEST/search-index"), format!("{base}/Archives/edgar/data"))
        };
        let client = reqwest::Client::builder().timeout(TIMEOUT).connect_timeout(Duration::from_secs(4)).build()?;
        Ok(Some(Self { client, search, archives, user_agent: user_agent.to_string(), limiter: Limiter::new(GAP) }))
    }

    async fn get(&self, url: &str, query: &[(&str, &str)]) -> anyhow::Result<reqwest::Response> {
        let url = reqwest::Url::parse_with_params(url, query)?;
        self.limiter.wait().await;
        let res = self.client.get(url).header(reqwest::header::USER_AGENT, &self.user_agent).send().await?;
        anyhow::ensure!(res.status().is_success(), "EDGAR answered {}", res.status());
        Ok(res)
    }

    /// The company's Form D offerings filed in the last year, each once with its first filing's date and its latest amounts.
    pub async fn find(&self, target: &Target, today: NaiveDate) -> anyhow::Result<Vec<Found>> {
        let name = core(&target.name);
        if name.is_empty() {
            return Ok(vec![]);
        }
        let start = (today - chrono::Duration::days(SEARCH_DAYS)).to_string();
        let (phrase, end) = (format!("\"{name}\""), today.to_string());
        let query = [("q", phrase.as_str()), ("forms", "D,D/A"), ("dateRange", "custom"), ("startdt", start.as_str()), ("enddt", end.as_str())];
        let body: Value = self.get(&self.search, &query).await?.json().await?;
        let mut found = vec![];
        for offering in offerings(&body, target).into_iter().take(MAX_OFFERINGS) {
            let latest = &offering.latest;
            let doc = format!("{}/{}/{}/{}", self.archives, latest.cik_number(), latest.adsh.replace('-', ""), latest.document);
            let xml = read_capped(self.get(&doc, &[]).await?, MAX_DOC_BYTES).await?;
            let form = form_d(&xml);
            let url = format!("{}/{}/{}/{}-index.htm", self.archives, latest.cik_number(), latest.adsh.replace('-', ""), latest.adsh);
            found.push(Found {
                kind: "funding_filing",
                key: format!("sec:{}:{}", latest.cik_number(), offering.file_num),
                happened_on: offering.filed_on,
                title: if form.entity.is_empty() { latest.name.clone() } else { form.entity },
                amount: form.sold.filter(|a| *a > 0),
                offered: form.offered.filter(|a| *a > 0),
                industry: form.industry,
                url,
            });
        }
        Ok(found)
    }
}

async fn read_capped(mut res: reqwest::Response, max: usize) -> anyhow::Result<String> {
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await? {
        anyhow::ensure!(body.len() + chunk.len() <= max, "the filing is over {} KB", max >> 10);
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// One filing in the search results.
#[derive(Debug, Clone)]
struct Hit {
    adsh: String,
    cik: String,
    /// The filer's name without EDGAR's "(CIK 0001234567)".
    name: String,
    form: String,
    filed_on: NaiveDate,
    file_num: String,
    state: String,
    city: String,
    document: String,
}

impl Hit {
    fn parse(source: &Value, id: &str) -> Option<Self> {
        let first = |key: &str| source[key].as_array().and_then(|a| a.first()).and_then(Value::as_str).unwrap_or_default().trim().to_string();
        let adsh = source["adsh"].as_str().map(str::to_string).or_else(|| id.split(':').next().map(str::to_string))?;
        let display = first("display_names");
        let name = display.split(" (CIK").next().unwrap_or_default().trim().to_string();
        let location = first("biz_locations");
        Some(Self {
            cik: first("ciks"),
            name,
            form: source["form"].as_str().or(source["file_type"].as_str()).unwrap_or_default().to_string(),
            filed_on: NaiveDate::parse_from_str(source["file_date"].as_str()?, "%Y-%m-%d").ok()?,
            file_num: first("file_num"),
            state: first("biz_states"),
            city: location.rsplit_once(',').map_or(location.as_str(), |(city, _)| city).trim().to_string(),
            document: id.split_once(':').map_or("primary_doc.xml", |(_, doc)| doc).to_string(),
            adsh,
        })
    }

    /// The CIK as the archive paths write it, without leading zeros.
    fn cik_number(&self) -> &str {
        let trimmed = self.cik.trim_start_matches('0');
        if trimmed.is_empty() { &self.cik } else { trimmed }
    }
}

/// An offering: its first Form D and its latest filing, which is an amendment once there is one.
#[derive(Debug)]
struct Offering {
    file_num: String,
    filed_on: NaiveDate,
    latest: Hit,
}

/// The matching filings grouped by offering, newest first. An amendment whose first filing is older than the search, like a
/// yearly D/A for a round from years ago, isn't news and is left out.
fn offerings(body: &Value, target: &Target) -> Vec<Offering> {
    let hits = body["hits"]["hits"].as_array().into_iter().flatten();
    let hits: Vec<Hit> = hits
        .filter_map(|h| Hit::parse(&h["_source"], h["_id"].as_str().unwrap_or_default()))
        .filter(|h| matches(target, &h.name, &h.state, &h.city))
        .collect();
    let mut groups: Vec<(String, Vec<Hit>)> = vec![];
    for hit in hits {
        let key = format!("{}:{}", hit.cik_number(), if hit.file_num.is_empty() { &hit.adsh } else { &hit.file_num });
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, group)) => group.push(hit),
            None => groups.push((key, vec![hit])),
        }
    }
    let mut offerings: Vec<Offering> = groups
        .into_iter()
        .filter_map(|(_, group)| {
            let first = group.iter().filter(|h| h.form == "D").min_by_key(|h| h.filed_on)?;
            let latest = group.iter().max_by_key(|h| (h.filed_on, h.form != "D")).unwrap_or(first);
            let file_num = if first.file_num.is_empty() { first.adsh.clone() } else { first.file_num.clone() };
            Some(Offering { file_num, filed_on: first.filed_on, latest: latest.clone() })
        })
        .collect();
    offerings.sort_by(|a, b| b.filed_on.cmp(&a.filed_on));
    offerings
}

/// Legal endings dropped before names are compared, so "Acme Robotics, Inc." is "Acme Robotics LLC".
const SUFFIXES: [&str; 19] =
    ["inc", "incorporated", "corp", "corporation", "co", "company", "ltd", "limited", "llc", "lp", "llp", "plc", "pbc", "gmbh", "ag", "sa", "bv", "nv", "pte"];

/// The name's words in lowercase, without punctuation, a leading "the" or the legal endings.
pub fn core(name: &str) -> String {
    let cleaned: String = name.to_lowercase().replace('.', "").replace('&', " and ").chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    let mut words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.first() == Some(&"the") {
        words.remove(0);
    }
    while words.len() > 1 && words.last().is_some_and(|w| SUFFIXES.contains(w)) {
        words.pop();
    }
    words.join(" ")
}

/// Whether a filer is the company. The legal names must be the same once the endings are gone, and the filer's state must be the
/// company's, or its city when Splee knows no state. Without either the name alone is never enough, since a wrong raise in an email
/// is worse than a missed one.
pub fn matches(target: &Target, filer: &str, state: &str, city: &str) -> bool {
    let name = core(&target.name);
    if name.is_empty() || name != core(filer) {
        return false;
    }
    match (state_code(&target.region), target.city.trim()) {
        (Some(code), _) => code.eq_ignore_ascii_case(state.trim()),
        (None, known) => !known.is_empty() && known.eq_ignore_ascii_case(city.trim()),
    }
}

const STATES: [(&str, &str); 51] = [
    ("AL", "alabama"), ("AK", "alaska"), ("AZ", "arizona"), ("AR", "arkansas"), ("CA", "california"), ("CO", "colorado"),
    ("CT", "connecticut"), ("DE", "delaware"), ("DC", "district of columbia"), ("FL", "florida"), ("GA", "georgia"), ("HI", "hawaii"),
    ("ID", "idaho"), ("IL", "illinois"), ("IN", "indiana"), ("IA", "iowa"), ("KS", "kansas"), ("KY", "kentucky"), ("LA", "louisiana"),
    ("ME", "maine"), ("MD", "maryland"), ("MA", "massachusetts"), ("MI", "michigan"), ("MN", "minnesota"), ("MS", "mississippi"),
    ("MO", "missouri"), ("MT", "montana"), ("NE", "nebraska"), ("NV", "nevada"), ("NH", "new hampshire"), ("NJ", "new jersey"),
    ("NM", "new mexico"), ("NY", "new york"), ("NC", "north carolina"), ("ND", "north dakota"), ("OH", "ohio"), ("OK", "oklahoma"),
    ("OR", "oregon"), ("PA", "pennsylvania"), ("RI", "rhode island"), ("SC", "south carolina"), ("SD", "south dakota"),
    ("TN", "tennessee"), ("TX", "texas"), ("UT", "utah"), ("VT", "vermont"), ("VA", "virginia"), ("WA", "washington"),
    ("WV", "west virginia"), ("WI", "wisconsin"), ("WY", "wyoming"),
];

/// A US state's two-letter code from its code ("CA", "US-CA") or its name, as the company database writes regions.
fn state_code(region: &str) -> Option<&'static str> {
    let region = region.trim();
    let region = region.strip_prefix("US-").unwrap_or(region);
    STATES.iter().find(|(code, name)| code.eq_ignore_ascii_case(region) || name.eq_ignore_ascii_case(region)).map(|(code, _)| *code)
}

/// What a Form D's primary document says. Amounts are None when it says "Indefinite".
#[derive(Debug, Default, PartialEq)]
pub struct FormD {
    pub entity: String,
    pub industry: String,
    pub offered: Option<i64>,
    pub sold: Option<i64>,
}

/// Reads the first of each field in document order, so the primary issuer's name wins over related persons' and other issuers'.
pub fn form_d(xml: &str) -> FormD {
    let mut form = FormD::default();
    let mut reader = Reader::from_str(xml);
    let (mut current, mut text) = (String::new(), String::new());
    let mut seen: Vec<String> = vec![];
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                current = e.local_name().as_ref().to_string();
                text.clear();
            }
            Ok(Event::Text(t)) => text.push_str(&t.xml10_content()),
            Ok(Event::CData(t)) => text.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(r)) => text.push_str(&super::entity(&r)),
            // Only a leaf's end has `current` set: a parent's end comes after a child's has cleared it.
            Ok(Event::End(_)) => {
                let value = text.trim();
                let amount = || value.replace(',', "").parse::<f64>().ok().map(|a| a.round() as i64);
                let field = ["entityName", "industryGroupType", "totalOfferingAmount", "totalAmountSold"].contains(&current.as_str());
                if field && !seen.contains(&current) {
                    match current.as_str() {
                        "entityName" => form.entity = value.to_string(),
                        "industryGroupType" => form.industry = value.to_string(),
                        "totalOfferingAmount" => form.offered = amount(),
                        _ => form.sold = amount(),
                    }
                    seen.push(current.clone());
                }
                current.clear();
                text.clear();
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    form
}

#[cfg(test)]
pub mod tests {
    use serde_json::{Value, json};

    use super::*;

    pub fn target(name: &str, city: &str, region: &str) -> Target {
        Target { domain: "acme.example".into(), name: name.into(), city: city.into(), region: region.into() }
    }

    /// A search hit as EDGAR's full-text search returns it.
    /// `location` is EDGAR's "San Francisco, CA".
    pub fn hit(adsh: &str, cik: &str, name: &str, form: &str, filed: &str, file_num: &str, location: &str) -> Value {
        let state = location.rsplit(", ").next().unwrap_or_default();
        json!({
            "_id": format!("{adsh}:primary_doc.xml"),
            "_source": {
                "adsh": adsh, "ciks": [cik], "display_names": [format!("{name}  (CIK {cik})")], "form": form, "root_forms": ["D"],
                "file_date": filed, "file_num": [file_num], "biz_states": [state], "biz_locations": [location],
            }
        })
    }

    pub fn primary_doc(entity: &str, industry: &str, offered: &str, sold: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><edgarSubmission><primaryIssuer><cik>0001234567</cik><entityName>{entity}</entityName>\
             <issuerAddress><city>SAN FRANCISCO</city><stateOrCountry>CA</stateOrCountry></issuerAddress></primaryIssuer>\
             <relatedPersonsList><relatedPersonInfo><relatedPersonName><firstName>Dana</firstName></relatedPersonName>\
             <relatedPersonAddress><city>AUSTIN</city></relatedPersonAddress></relatedPersonInfo></relatedPersonsList>\
             <offeringData><industryGroup><industryGroupType>{industry}</industryGroupType></industryGroup>\
             <offeringSalesAmounts><totalOfferingAmount>{offered}</totalOfferingAmount><totalAmountSold>{sold}</totalAmountSold>\
             </offeringSalesAmounts></offeringData></edgarSubmission>"
        )
    }

    #[test]
    fn names_are_compared_without_their_legal_endings() {
        assert_eq!(core("Acme Robotics, Inc."), "acme robotics");
        assert_eq!(core("The Acme Robotics Co., L.L.C."), "acme robotics");
        assert_eq!(core("Smith & Sons Ltd"), "smith and sons");
        assert_eq!(core("Inc"), "inc");
        assert_eq!(state_code("US-CA"), Some("CA"));
        assert_eq!(state_code("Texas"), Some("TX"));
        assert_eq!(state_code("Bavaria"), None);
    }

    #[test]
    fn a_filer_matches_only_by_name_and_place() {
        // Nothing known about the place: the name is never enough.
        assert!(!matches(&target("Acme Robotics", "", ""), "ACME ROBOTICS, INC.", "CA", "San Francisco"));
        assert!(matches(&target("Acme Robotics", "", "CA"), "ACME ROBOTICS, INC.", "CA", "San Francisco"));
        assert!(!matches(&target("Acme Robotics", "", "CA"), "Acme Robotics Holdings, Inc.", "CA", "San Francisco"));
        // A known state must agree, and a known city when the state isn't known.
        assert!(!matches(&target("Acme Robotics", "", "TX"), "Acme Robotics, Inc.", "CA", "San Francisco"));
        assert!(matches(&target("Acme Robotics", "Austin", "Texas"), "Acme Robotics, Inc.", "TX", "Dallas"));
        assert!(!matches(&target("Acme Robotics", "Austin", ""), "Acme Robotics, Inc.", "TX", "Dallas"));
        assert!(!matches(&target("Notion", "", ""), "Notion, Inc.", "CA", "San Francisco"));
        assert!(matches(&target("Notion", "San Francisco", ""), "Notion, Inc.", "CA", "SAN FRANCISCO"));
        assert!(matches(&target("Notion", "", "CA"), "Notion Inc", "CA", "Oakland"));
        assert!(!matches(&target("", "", ""), "", "CA", "Oakland"));
    }

    #[test]
    fn a_form_d_gives_its_amounts_and_the_issuers_name() {
        let form = form_d(&primary_doc("Acme Robotics, Inc.", "Other Technology", "15000000", "12000000"));
        assert_eq!(form, FormD { entity: "Acme Robotics, Inc.".into(), industry: "Other Technology".into(), offered: Some(15_000_000), sold: Some(12_000_000) });
        let open = form_d(&primary_doc("Acme &amp; Co", "Pooled Investment Fund", "Indefinite", "2500000.00"));
        assert_eq!((open.entity.as_str(), open.offered, open.sold), ("Acme & Co", None, Some(2_500_000)));
        assert_eq!(form_d("not xml at all"), FormD::default());
    }

    #[test]
    fn amendments_join_their_offering_and_old_rounds_are_left_out() {
        let t = target("Acme Robotics", "", "CA");
        let body = json!({ "hits": { "hits": [
            hit("0001234567-26-000002", "0001234567", "Acme Robotics, Inc.", "D/A", "2026-10-20", "021-111", "San Francisco, CA"),
            hit("0001234567-26-000001", "0001234567", "Acme Robotics, Inc.", "D", "2026-10-03", "021-111", "San Francisco, CA"),
            // A yearly amendment of a round whose Form D is older than the search.
            hit("0001234567-26-000003", "0001234567", "Acme Robotics, Inc.", "D/A", "2026-09-01", "021-050", "San Francisco, CA"),
            hit("0009999999-26-000001", "0009999999", "Acme Robotics Holdings LLC", "D", "2026-10-10", "021-222", "Dover, DE"),
        ] } });
        let found = offerings(&body, &t);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].file_num.as_str(), found[0].filed_on.to_string(), found[0].latest.adsh.as_str()), ("021-111", "2026-10-03".into(), "0001234567-26-000002"));
        assert_eq!(found[0].latest.cik_number(), "1234567");
    }
}
