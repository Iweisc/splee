//! The company database: businesses from Overture Maps' places (https://docs.overturemaps.org/guides/places/), an open
//! dataset of about 88 million places under CDLA Permissive 2.0, kept one row per website so each company appears once.
//! `splee import-companies [COUNTRY…]` and the Companies page's "Load a country" (directory_queue.rs) load it. Overture
//! publishes its places as GeoParquet files every month, and they're read straight over HTTPS, a few byte ranges at a
//! time, so nothing large is ever written to disk.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use arrow_array::cast::AsArray;
use arrow_array::{Array, ArrayRef, RecordBatch, StructArray};
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt, TryStreamExt};
use parquet::arrow::arrow_reader::ArrowReaderOptions;
use parquet::arrow::async_reader::AsyncFileReader;
use parquet::arrow::{ParquetRecordBatchStreamBuilder, ProjectionMask};
use parquet::file::metadata::{ParquetMetaData, ParquetMetaDataReader};
use parquet::file::statistics::Statistics;
use reqwest::Url;
use serde::Serialize;
use serde_json::Value;
use sqlx::{FromRow, PgPool, Row};
use uuid::Uuid;

pub const CATALOG: &str = "https://stac.overturemaps.org/catalog.json";
/// Byte ranges closer than this are fetched in one request, since a row group's columns sit side by side.
const MERGE_GAP: u64 = 1 << 20;
/// Files read at once.
const PARALLEL_FILES: usize = 4;
const COLUMNS: [&str; 10] =
    ["id", "names.primary", "websites", "phones", "emails", "addresses", "taxonomy.primary", "taxonomy.hierarchy", "confidence", "operating_status"];

/// How far an import is. Every file's row groups that can hold the countries are counted before any rows are read.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    pub groups_done: usize,
    pub groups_total: usize,
    pub rows_seen: usize,
    /// Companies saved so far, counting a domain again when another batch has it too. The result has the real count.
    pub rows_kept: usize,
}

/// Called after every row group, from whichever file finished it.
pub type Report<'a> = &'a (dyn Fn(Progress) + Sync);

/// Imports every place with a website in `countries` (ISO codes like GB), or everywhere when it's empty, and then removes
/// the companies of those countries that the latest release no longer has. `catalog` is Overture's STAC catalog, CATALOG
/// outside tests.
pub async fn import(db: &PgPool, catalog: &str, countries: &[String], report: Option<Report<'_>>) -> anyhow::Result<Progress> {
    // Overture's catalog refuses requests without a user agent.
    // Sizes come from Content-Length, which a gzipped answer would drop.
    let client = reqwest::Client::builder().user_agent("splee").no_gzip().timeout(Duration::from_secs(120)).build()?;
    let (release, files) = latest_files(&client, catalog).await?;
    println!("Overture release {release}: {} files", files.len());
    let countries: Vec<String> = countries.iter().map(|c| c.trim().to_uppercase()).collect();
    let release = release.as_str();
    let files: Vec<(Reader, Vec<usize>)> = futures::stream::iter(files.into_iter().enumerate())
        .map(|(i, url)| {
            let (client, countries) = (client.clone(), &countries);
            async move { open(client, url, countries).await.with_context(|| format!("file {i}")) }
        })
        .buffered(PARALLEL_FILES)
        .try_collect()
        .await?;
    let tally = Tally {
        progress: Mutex::new(Progress { groups_total: files.iter().map(|(_, groups)| groups.len()).sum(), ..Progress::default() }),
        report,
    };
    tally.add(0, 0, 0);
    futures::stream::iter(files.into_iter().enumerate())
        .map(|(i, file)| {
            let (countries, tally) = (&countries, &tally);
            async move {
                let kept = import_file(db, file, release, countries, tally).await.with_context(|| format!("file {i}"))?;
                if kept > 0 {
                    println!("file {i}: {kept} companies");
                }
                anyhow::Ok(())
            }
        })
        .buffer_unordered(PARALLEL_FILES)
        .try_collect::<()>()
        .await?;
    let gone = if countries.is_empty() {
        sqlx::query("delete from directory_companies where release <> $1").bind(release).execute(db).await?
    } else {
        sqlx::query("delete from directory_companies where release <> $1 and country_code = any($2)").bind(release).bind(&countries).execute(db).await?
    };
    // A domain kept in several batches was counted each time, so the final count is what the countries now hold.
    let held: i64 = sqlx::query_scalar("select count(*) from directory_companies where cardinality($1::text[]) = 0 or country_code = any($1)")
        .bind(&countries)
        .fetch_one(db)
        .await?;
    let progress = Progress { rows_kept: held as usize, ..*tally.progress.lock().expect("not poisoned") };
    println!("The import kept {} companies. Removed {} that are no longer listed.", progress.rows_kept, gone.rows_affected());
    Ok(progress)
}

struct Tally<'a> {
    progress: Mutex<Progress>,
    report: Option<Report<'a>>,
}

impl Tally<'_> {
    fn add(&self, groups: usize, seen: usize, kept: usize) {
        let mut p = self.progress.lock().expect("not poisoned");
        p.groups_done += groups;
        p.rows_seen += seen;
        p.rows_kept += kept;
        // Reported under the lock, so two files finishing at once can't report out of order.
        if let Some(report) = self.report {
            report(*p);
        }
    }
}

/// What to look for in the company database. The Companies page and the agent's `directory` tool both search with it.
pub struct Search {
    /// Word prefixes as a tsquery, from `prefixes`.
    words: Option<String>,
    country: Option<String>,
    city: Option<String>,
    category: Option<String>,
    has_email: bool,
    has_phone: bool,
    /// Leaves out the companies this project already has, so an agent searching again sees new ones.
    pub skip_project: Option<Uuid>,
    /// Only companies whose tech scan found one of these tools. Only domains in some project have been scanned.
    pub tech: Option<Vec<String>>,
}

impl Search {
    /// Blank filters are ignored. The error is a message for the person or model that sent `country`.
    pub fn new(
        q: Option<&str>,
        country: Option<&str>,
        city: Option<&str>,
        category: Option<&str>,
        has_email: bool,
        has_phone: bool,
    ) -> Result<Self, String> {
        fn blank(s: Option<&str>) -> Option<&str> {
            s.map(str::trim).filter(|s| !s.is_empty())
        }
        let country = blank(country).map(str::to_uppercase);
        if country.as_ref().is_some_and(|c| c.len() != 2 || !c.chars().all(|c| c.is_ascii_alphabetic())) {
            return Err("Pick a country by its two-letter code, like GB".into());
        }
        Ok(Self {
            words: q.and_then(prefixes),
            country,
            city: blank(city).map(str::to_string),
            category: blank(category).map(|c| c.to_lowercase().replace(' ', "_")),
            has_email,
            has_phone,
            skip_project: None,
            tech: None,
        })
    }

    fn bind<'q>(&'q self, sql: &'q str) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
        sqlx::query(sql)
            .bind(&self.words)
            .bind(&self.country)
            .bind(&self.city)
            .bind(&self.category)
            .bind(self.has_email)
            .bind(self.has_phone)
            .bind(self.skip_project)
            .bind(&self.tech)
    }
}

const FILTER: &str = "from directory_companies d
    where ($1::text is null or search @@ to_tsquery('simple', $1))
      and ($2::text is null or country_code = $2)
      and ($3::text is null or lower(city) = lower($3))
      and ($4::text is null or categories @> array[$4])
      and (not $5 or email is not null)
      and (not $6 or phone is not null)
      and ($7::uuid is null or not exists (select 1 from companies c where c.project_id = $7 and c.domain = d.domain::citext))
      and ($8::text[] is null or d.domain in (select lower(t.domain::text) from tech_scans t where t.techs && $8))";

#[derive(Serialize, sqlx::FromRow)]
pub struct Listing {
    pub domain: String,
    pub name: String,
    pub website: String,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub address: Option<String>,
    pub city: Option<String>,
    pub region: Option<String>,
    pub postcode: Option<String>,
    pub country_code: String,
    pub category: Option<String>,
    /// The tools its tech scan found, empty when it hasn't been scanned.
    pub tech: Vec<String>,
}

/// The matches, the most reliable listings first.
pub async fn search(db: &PgPool, search: &Search, limit: i64, offset: i64) -> sqlx::Result<Vec<Listing>> {
    let sql = format!(
        "select domain, name, website, phone, email, address, city, region, postcode, country_code, category,
                coalesce((select t.techs from tech_scans t where t.domain = d.domain::citext), '{{}}') as tech
         {FILTER}
         order by confidence desc, domain limit $9 offset $10"
    );
    let rows = search.bind(&sql).bind(limit).bind(offset).fetch_all(db).await?;
    rows.iter().map(Listing::from_row).collect()
}

/// How many companies match, counting no further than `cap` + 1, since counting every restaurant in the US is slow.
pub async fn count(db: &PgPool, search: &Search, cap: i64) -> sqlx::Result<i64> {
    let sql = format!("select count(*) from (select 1 {FILTER} limit {cap} + 1) matches");
    search.bind(&sql).fetch_one(db).await?.try_get(0)
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Country {
    pub country_code: String,
    pub companies: i64,
}

/// Grouping millions of rows takes seconds, and the list only changes with an import, so it's kept for a while.
const COUNTRIES_TTL: Duration = Duration::from_secs(10 * 60);
static COUNTRIES: Mutex<Option<(Instant, Arc<Vec<Country>>)>> = Mutex::new(None);

/// The countries that have companies, most companies first.
pub async fn countries(db: &PgPool) -> sqlx::Result<Arc<Vec<Country>>> {
    if let Some((at, list)) = COUNTRIES.lock().expect("not poisoned").as_ref()
        && at.elapsed() < COUNTRIES_TTL
    {
        return Ok(list.clone());
    }
    let list = Arc::new(
        sqlx::query_as::<_, Country>("select country_code, count(*) as companies from directory_companies group by 1 order by 2 desc, 1")
            .fetch_all(db)
            .await?,
    );
    // An empty table isn't kept, so the page sees a first import right away instead of after the wait.
    if !list.is_empty() {
        *COUNTRIES.lock().expect("not poisoned") = Some((Instant::now(), list.clone()));
    }
    Ok(list)
}

/// Called when an import from the Companies page finishes, so its country shows up there right away.
pub fn forget_countries() {
    *COUNTRIES.lock().expect("not poisoned") = None;
}

/// Every word as a prefix, so "dent" finds dental_clinic and general_dentistry, as a tsquery. Only letters and digits get
/// through, so a query can't hold tsquery syntax.
fn prefixes(q: &str) -> Option<String> {
    let words: Vec<String> = q.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| format!("{}:*", w.to_lowercase())).collect();
    (!words.is_empty()).then(|| words.join(" & "))
}

/// The newest release's places files, from Overture's STAC catalog.
async fn latest_files(client: &reqwest::Client, catalog: &str) -> anyhow::Result<(String, Vec<Url>)> {
    let get = |url: String| async move { anyhow::Ok(client.get(url).send().await?.error_for_status()?.json::<Value>().await?) };
    let links = |v: &Value, rel: &str| -> Vec<String> {
        v["links"].as_array().into_iter().flatten().filter(|l| l["rel"] == rel).filter_map(|l| l["href"].as_str().map(str::to_string)).collect()
    };
    let catalog = get(catalog.into()).await?;
    let release = links(&catalog, "child").into_iter().max().context("Overture's catalog lists no release")?;
    let base = release.trim_end_matches("/catalog.json").to_string();
    let collection = get(format!("{base}/places/place/collection.json")).await?;
    let mut files = Vec::new();
    for item in links(&collection, "item") {
        let item = get(Url::parse(&format!("{base}/places/place/"))?.join(&item)?.to_string()).await?;
        files.push(Url::parse(item["assets"]["azure"]["href"].as_str().context("a places file has no download link")?)?);
    }
    let name = base.rsplit('/').next().unwrap_or_default().to_string();
    Ok((name, files))
}

type Reader = ParquetRecordBatchStreamBuilder<HttpFile>;

/// The file's metadata and the row groups to read from it.
async fn open(client: reqwest::Client, url: Url, countries: &[String]) -> anyhow::Result<(Reader, Vec<usize>)> {
    let head = client.head(url.clone()).send().await?.error_for_status()?;
    // reqwest's content_length() is the HEAD response's own empty body, so the header has to be read directly.
    let size: u64 = head.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse().ok()).context("no file size")?;
    let builder = ParquetRecordBatchStreamBuilder::new(HttpFile { client, url, size }).await?;
    let groups = row_groups(builder.metadata(), countries);
    Ok((builder, groups))
}

/// Returns the companies kept. Row groups are read whole, one at a time, so progress moves a group at a time.
async fn import_file(
    db: &PgPool,
    (builder, groups): (Reader, Vec<usize>),
    release: &str,
    countries: &[String],
    tally: &Tally<'_>,
) -> anyhow::Result<usize> {
    let mask = ProjectionMask::columns(builder.parquet_schema(), COLUMNS);
    let mut stream = builder.with_projection(mask).with_row_groups(groups).with_batch_size(8192).build()?;
    let mut kept = 0;
    while let Some(group) = stream.next_row_group().await? {
        let (mut seen, mut group_kept) = (0, 0);
        for batch in group {
            let batch = batch?;
            seen += batch.num_rows();
            let companies = companies(&batch, countries);
            save(db, &companies, release).await?;
            group_kept += companies.len();
        }
        tally.add(1, seen, group_kept);
        kept += group_kept;
    }
    Ok(kept)
}

/// The row groups that can hold a place in `countries`, by the address country's min and max. Overture sorts places by
/// location, so most groups are a single country.
fn row_groups(meta: &ParquetMetaData, countries: &[String]) -> Vec<usize> {
    let schema = meta.file_metadata().schema_descr();
    let column = (0..schema.num_columns()).find(|&i| schema.column(i).path().string() == "addresses.list.element.country");
    (0..meta.num_row_groups())
        .filter(|&g| {
            let stats = column.and_then(|c| meta.row_group(g).column(c).statistics());
            let Some(Statistics::ByteArray(s)) = stats else { return true };
            let (Some(min), Some(max)) = (s.min_opt().and_then(|m| m.as_utf8().ok()), s.max_opt().and_then(|m| m.as_utf8().ok())) else { return true };
            countries.is_empty() || countries.iter().any(|c| min <= c.as_str() && c.as_str() <= max)
        })
        .collect()
}

#[derive(Debug, PartialEq)]
struct Company {
    domain: String,
    name: String,
    website: String,
    phone: Option<String>,
    email: Option<String>,
    address: Option<String>,
    city: Option<String>,
    region: Option<String>,
    postcode: Option<String>,
    country_code: String,
    category: Option<String>,
    categories: Vec<String>,
    confidence: f32,
    source_id: String,
}

/// The batch's open places that have a company website and are in `countries`, the most confident one per domain.
fn companies(batch: &RecordBatch, countries: &[String]) -> Vec<Company> {
    let column = |name: &str| batch.column_by_name(name).cloned();
    let (Some(ids), Some(names), Some(websites), Some(phones), Some(emails), Some(addresses), Some(taxonomy), Some(confidence), Some(status)) = (
        column("id"),
        column("names"),
        column("websites"),
        column("phones"),
        column("emails"),
        column("addresses"),
        column("taxonomy"),
        column("confidence"),
        column("operating_status"),
    ) else {
        return Vec::new();
    };
    let names = names.as_struct().column_by_name("primary").cloned();
    let taxonomy = taxonomy.as_struct();
    let (primary, hierarchy) = (taxonomy.column_by_name("primary").cloned(), taxonomy.column_by_name("hierarchy").cloned());
    let mut best: HashMap<String, Company> = HashMap::new();
    for i in 0..batch.num_rows() {
        if matches!(text(&status, i).as_deref(), Some("permanently_closed" | "temporarily_closed")) {
            continue;
        }
        let address = first_struct(&addresses, i);
        let field = |name: &str| address.as_ref().and_then(|(a, j)| a.column_by_name(name).and_then(|c| text(c, *j)));
        let Some(country_code) = field("country").map(|c| c.to_uppercase()) else { continue };
        if !countries.is_empty() && !countries.contains(&country_code) {
            continue;
        }
        let (Some(website), Some(name)) = (list(&websites, i).into_iter().find_map(|w| domain(&w).map(|d| (w, d))), names.as_ref().and_then(|n| text(n, i)))
        else {
            continue;
        };
        let confidence = confidence.as_primitive_opt::<arrow_array::types::Float64Type>().filter(|c| c.is_valid(i)).map_or(0.0, |c| c.value(i) as f32);
        let company = Company {
            domain: website.1,
            name,
            website: website.0,
            phone: list(&phones, i).into_iter().next(),
            email: list(&emails, i).into_iter().next().map(|e| e.to_lowercase()),
            address: field("freeform"),
            city: field("locality"),
            region: field("region"),
            postcode: field("postcode"),
            country_code,
            category: primary.as_ref().and_then(|p| text(p, i)),
            categories: hierarchy.as_ref().map(|h| list(h, i)).unwrap_or_default(),
            confidence,
            source_id: text(&ids, i).unwrap_or_default(),
        };
        match best.get(&company.domain) {
            Some(seen) if seen.confidence >= company.confidence => {}
            _ => {
                best.insert(company.domain.clone(), company);
            }
        }
    }
    // Files are saved in parallel, and upserts that lock the same domains in different orders deadlock.
    let mut companies: Vec<Company> = best.into_values().collect();
    companies.sort_by(|a, b| a.domain.cmp(&b.domain));
    companies
}

/// A non-empty string at row `i`, from either string layout Arrow may use.
fn text(array: &ArrayRef, i: usize) -> Option<String> {
    if array.is_null(i) {
        return None;
    }
    let value = if let Some(s) = array.as_string_opt::<i32>() {
        s.value(i)
    } else if let Some(s) = array.as_string_view_opt() {
        s.value(i)
    } else {
        return None;
    };
    Some(value.trim().to_string()).filter(|v| !v.is_empty())
}

/// The non-empty strings in a list column at row `i`.
fn list(array: &ArrayRef, i: usize) -> Vec<String> {
    let Some(lists) = array.as_list_opt::<i32>() else { return Vec::new() };
    if lists.is_null(i) {
        return Vec::new();
    }
    let values = lists.value(i);
    (0..values.len()).filter_map(|j| text(&values, j)).collect()
}

/// The first struct in a list of structs at row `i`, with its index in the list's values.
fn first_struct(array: &ArrayRef, i: usize) -> Option<(StructArray, usize)> {
    let lists = array.as_list_opt::<i32>()?;
    if lists.is_null(i) || lists.value_length(i) == 0 {
        return None;
    }
    Some((lists.values().as_struct().clone(), lists.value_offsets()[i] as usize))
}

/// Hosts where a business only has a page, like a Facebook page or a booking link, so the address isn't the company's.
const NOT_COMPANY_SITES: &[&str] = &[
    "facebook.com", "fb.com", "fb.me", "instagram.com", "twitter.com", "x.com", "linkedin.com", "youtube.com", "youtu.be", "tiktok.com",
    "pinterest.com", "linktr.ee", "wa.me", "whatsapp.com", "t.me", "google.com", "goo.gl", "g.page", "business.site", "yelp.com",
    "tripadvisor.com", "booking.com", "airbnb.com", "opentable.com", "ubereats.com", "doordash.com", "grubhub.com", "just-eat.co.uk",
    "deliveroo.co.uk", "toasttab.com", "squareup.com", "bit.ly", "tinyurl.com", "apple.com", "yell.com", "yellowpages.com", "foursquare.com",
    "nextdoor.com", "treatwell.co.uk", "fresha.com", "vagaro.com", "booksy.com", "calendly.com", "wixsite.com", "weebly.com",
];

/// The company's domain, the website's host without www., or None when it's a page on someone else's site.
fn domain(website: &str) -> Option<String> {
    let website = website.trim();
    let url = Url::parse(website).or_else(|_| Url::parse(&format!("https://{website}"))).ok()?;
    let host = url.host_str()?.trim_end_matches('.').to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    let listed = NOT_COMPANY_SITES.iter().any(|s| host == *s || host.ends_with(&format!(".{s}")));
    (host.contains('.') && !listed && host.parse::<std::net::IpAddr>().is_err()).then_some(host)
}

/// Upserts a batch. A row from an older release is replaced, and within one release the most confident place wins.
async fn save(db: &PgPool, companies: &[Company], release: &str) -> anyhow::Result<()> {
    if companies.is_empty() {
        return Ok(());
    }
    macro_rules! column {
        ($field:ident) => {
            companies.iter().map(|c| c.$field.clone()).collect::<Vec<_>>()
        };
    }
    // Postgres can't unnest a text[][], so the categories go in as one JSON array per row.
    let categories: Vec<String> = companies.iter().map(|c| serde_json::to_string(&c.categories).unwrap_or_default()).collect();
    sqlx::query(
        "insert into directory_companies (domain, name, website, phone, email, address, city, region, postcode, country_code, category,
            categories, confidence, source_id, release)
         select d, n, w, p, e, a, ci, r, pc, cc, cat, array(select jsonb_array_elements_text(cats::jsonb)), conf, s, $15
         from unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::text[], $8::text[], $9::text[],
                     $10::text[], $11::text[], $12::text[], $13::real[], $14::text[])
              as t(d, n, w, p, e, a, ci, r, pc, cc, cat, cats, conf, s)
         on conflict (domain) do update set
            name = excluded.name, website = excluded.website, phone = excluded.phone, email = excluded.email,
            address = excluded.address, city = excluded.city, region = excluded.region, postcode = excluded.postcode,
            country_code = excluded.country_code, category = excluded.category, categories = excluded.categories,
            confidence = excluded.confidence, source_id = excluded.source_id, release = excluded.release, updated_at = now()
         where directory_companies.release <> excluded.release or directory_companies.confidence < excluded.confidence",
    )
    .bind(column!(domain))
    .bind(column!(name))
    .bind(column!(website))
    .bind(column!(phone))
    .bind(column!(email))
    .bind(column!(address))
    .bind(column!(city))
    .bind(column!(region))
    .bind(column!(postcode))
    .bind(column!(country_code))
    .bind(column!(category))
    .bind(categories)
    .bind(column!(confidence))
    .bind(column!(source_id))
    .bind(release)
    .execute(db)
    .await?;
    Ok(())
}

/// A Parquet file read over HTTPS with range requests.
struct HttpFile {
    client: reqwest::Client,
    url: Url,
    size: u64,
}

impl HttpFile {
    async fn fetch(&self, range: Range<u64>) -> parquet::errors::Result<Bytes> {
        let failed = |e: reqwest::Error| parquet::errors::ParquetError::External(Box::new(e.without_url()));
        let res = self.client.get(self.url.clone()).header("range", format!("bytes={}-{}", range.start, range.end - 1)).send().await.map_err(failed)?;
        res.error_for_status().map_err(failed)?.bytes().await.map_err(failed)
    }
}

impl AsyncFileReader for HttpFile {
    fn get_bytes(&mut self, range: Range<u64>) -> BoxFuture<'_, parquet::errors::Result<Bytes>> {
        self.fetch(range).boxed()
    }

    /// Ranges close together are fetched as one and sliced, so a row group is one or two requests, not one per column.
    fn get_byte_ranges(&mut self, ranges: Vec<Range<u64>>) -> BoxFuture<'_, parquet::errors::Result<Vec<Bytes>>> {
        async move {
            let mut out = vec![Bytes::new(); ranges.len()];
            let mut order: Vec<usize> = (0..ranges.len()).collect();
            order.sort_by_key(|&i| ranges[i].start);
            let mut k = 0;
            while k < order.len() {
                let start = ranges[order[k]].start;
                let mut end = ranges[order[k]].end;
                let mut last = k;
                while last + 1 < order.len() && ranges[order[last + 1]].start <= end + MERGE_GAP {
                    last += 1;
                    end = end.max(ranges[order[last]].end);
                }
                let merged = self.fetch(start..end).await?;
                for &i in &order[k..=last] {
                    let r = &ranges[i];
                    out[i] = merged.slice((r.start - start) as usize..(r.end - start) as usize);
                }
                k = last + 1;
            }
            Ok(out)
        }
        .boxed()
    }

    fn get_metadata<'a>(&'a mut self, options: Option<&'a ArrowReaderOptions>) -> BoxFuture<'a, parquet::errors::Result<Arc<ParquetMetaData>>> {
        async move {
            let size = self.size;
            let reader = ParquetMetaDataReader::new().with_arrow_reader_options(options).with_prefetch_hint(Some(1 << 20));
            Ok(Arc::new(reader.load_and_finish(self, size).await?))
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use sqlx::PgPool;

    use super::{Progress, domain, import};

    #[test]
    fn a_search_becomes_word_prefixes() {
        assert_eq!(super::prefixes("Dentist  London"), Some("dentist:* & london:*".into()));
        assert_eq!(super::prefixes("o'brien & sons | !x"), Some("o:* & brien:* & sons:* & x:*".into()));
        assert_eq!(super::prefixes(" &|! "), None);
    }

    #[test]
    fn a_website_becomes_its_domain_unless_its_a_page_elsewhere() {
        assert_eq!(domain("http://www.Emrktng.co.uk/about"), Some("emrktng.co.uk".into()));
        assert_eq!(domain("pawswalktailstalk.com"), Some("pawswalktailstalk.com".into()));
        assert_eq!(domain("https://shop.acme.com"), Some("shop.acme.com".into()));
        assert_eq!(domain("https://www.facebook.com/acme"), None);
        assert_eq!(domain("https://m.facebook.com/acme"), None);
        assert_eq!(domain("https://acme.wixsite.com/home"), None);
        assert_eq!(domain(""), None);
        assert_eq!(domain("http://10.0.0.1/"), None);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn an_import_reports_each_row_group_it_reads(db: PgPool) {
        let base = super::fake::start().await;
        let seen = Mutex::new(Vec::new());
        let report = |p: Progress| seen.lock().unwrap().push(p);
        let countries = ["mt".to_string()];
        let done = import(&db, &format!("{base}/catalog.json"), &countries, Some(&report)).await.unwrap();

        let start = Progress { groups_done: 0, groups_total: 1, rows_seen: 0, rows_kept: 0 };
        let end = Progress { groups_done: 1, groups_total: 1, rows_seen: 2, rows_kept: 1 };
        assert_eq!(*seen.lock().unwrap(), [start, end]);
        assert_eq!(done, end);
        let (domain, city, release): (String, String, String) =
            sqlx::query_as("select domain, city, release from directory_companies").fetch_one(&db).await.unwrap();
        assert_eq!((domain.as_str(), city.as_str(), release.as_str()), ("valletta-dental.mt", "Valletta", "2026-10-15.0"));
    }
}

/// A local stand-in for Overture: a STAC catalog with two releases, the newest holding one GeoParquet file with a row group
/// of places in LU and one in MT, served with range requests.
#[cfg(test)]
pub mod fake {
    use std::sync::Arc;

    use arrow_array::builder::{ListBuilder, StringBuilder, StructBuilder};
    use arrow_array::{ArrayRef, Float64Array, RecordBatch, StringArray, StructArray};
    use arrow_schema::{DataType, Field, Fields};
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};
    use bytes::Bytes;
    use parquet::arrow::ArrowWriter;
    use serde_json::json;

    struct Place {
        id: &'static str,
        name: &'static str,
        website: &'static str,
        email: Option<&'static str>,
        city: &'static str,
        country: &'static str,
        category: &'static str,
        status: &'static str,
        confidence: f64,
    }

    const LU: [Place; 4] = [
        Place { id: "lu1", name: "Boulangerie Kayl", website: "https://www.kayl.lu/", email: Some("Hi@Kayl.lu"), city: "Luxembourg", country: "LU",
                category: "bakery", status: "open", confidence: 0.9 },
        Place { id: "lu2", name: "Kayl Fans", website: "https://facebook.com/kayl", email: None, city: "Luxembourg", country: "LU",
                category: "bakery", status: "open", confidence: 0.9 },
        Place { id: "lu3", name: "Old Mill", website: "https://oldmill.lu", email: None, city: "Esch", country: "LU",
                category: "restaurant", status: "permanently_closed", confidence: 0.9 },
        Place { id: "lu4", name: "Garage Weber", website: "weber.lu", email: None, city: "Esch", country: "LU",
                category: "car_repair", status: "open", confidence: 0.5 },
    ];
    const MT: [Place; 2] = [
        Place { id: "mt1", name: "Valletta Dental", website: "https://valletta-dental.mt", email: None, city: "Valletta", country: "MT",
                category: "dental_clinic", status: "open", confidence: 0.8 },
        Place { id: "mt2", name: "No Site", website: "", email: None, city: "Valletta", country: "MT", category: "cafe", status: "open", confidence: 0.8 },
    ];

    /// Overture names list items "element", and directory.rs looks for the address country's statistics by that name.
    fn strings() -> ListBuilder<StringBuilder> {
        ListBuilder::new(StringBuilder::new()).with_field(Arc::new(Field::new("element", DataType::Utf8, true)))
    }

    fn batch(places: &[Place]) -> RecordBatch {
        let text = |f: fn(&Place) -> Option<&'static str>| Arc::new(places.iter().map(f).collect::<StringArray>()) as ArrayRef;
        let list = |f: fn(&Place) -> Vec<&'static str>| {
            let mut b = strings();
            for p in places {
                f(p).into_iter().for_each(|v| b.values().append_value(v));
                b.append(true);
            }
            Arc::new(b.finish()) as ArrayRef
        };
        let struct_of = |fields: Vec<(&str, ArrayRef)>| {
            let (fields, arrays): (Vec<Field>, Vec<ArrayRef>) =
                fields.into_iter().map(|(name, a)| (Field::new(name, a.data_type().clone(), true), a)).unzip();
            Arc::new(StructArray::new(Fields::from(fields), arrays, None)) as ArrayRef
        };
        let address_fields: Vec<Field> =
            ["freeform", "locality", "postcode", "region", "country"].iter().map(|n| Field::new(*n, DataType::Utf8, true)).collect();
        let mut addresses = ListBuilder::new(StructBuilder::from_fields(address_fields.clone(), places.len()))
            .with_field(Arc::new(Field::new("element", DataType::Struct(address_fields.into()), true)));
        for p in places {
            let address = addresses.values();
            for (i, value) in [Some("1 Main Street"), Some(p.city), None, None, Some(p.country)].into_iter().enumerate() {
                address.field_builder::<StringBuilder>(i).unwrap().append_option(value);
            }
            address.append(true);
            addresses.append(true);
        }
        RecordBatch::try_from_iter([
            ("id", text(|p| Some(p.id))),
            ("names", struct_of(vec![("primary", text(|p| Some(p.name)))])),
            ("websites", list(|p| [p.website].into_iter().filter(|w| !w.is_empty()).collect())),
            ("phones", list(|_| vec!["+352 123"])),
            ("emails", list(|p| p.email.into_iter().collect())),
            ("addresses", Arc::new(addresses.finish()) as ArrayRef),
            ("taxonomy", struct_of(vec![("primary", text(|p| Some(p.category))), ("hierarchy", list(|p| vec!["services", p.category]))])),
            ("confidence", Arc::new(places.iter().map(|p| Some(p.confidence)).collect::<Float64Array>()) as ArrayRef),
            ("operating_status", text(|p| Some(p.status))),
        ])
        .unwrap()
    }

    fn parquet() -> Bytes {
        let (lu, mt) = (batch(&LU), batch(&MT));
        let mut file = Vec::new();
        let mut writer = ArrowWriter::try_new(&mut file, lu.schema(), None).unwrap();
        writer.write(&lu).unwrap();
        writer.flush().unwrap();
        writer.write(&mt).unwrap();
        writer.close().unwrap();
        file.into()
    }

    /// Serves `bytes=a-b` like Azure's blob storage does, and the whole file otherwise.
    fn ranged(file: &Bytes, headers: &HeaderMap) -> Response {
        let range = headers.get(header::RANGE).and_then(|v| {
            let (a, b) = v.to_str().ok()?.strip_prefix("bytes=")?.split_once('-')?;
            Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
        });
        let (status, body) = match range {
            Some((a, b)) => (StatusCode::PARTIAL_CONTENT, file.slice(a..=b.min(file.len() - 1))),
            None => (StatusCode::OK, file.clone()),
        };
        (status, [(header::CONTENT_LENGTH, body.len().to_string())], body).into_response()
    }

    /// Returns the server's base URL. Its /catalog.json is the catalog.
    pub async fn start() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let file = parquet();
        let release = |name: &str| json!({ "rel": "child", "href": format!("{base}/{name}/catalog.json") });
        let catalog = json!({ "links": [release("2026-09-17.0"), release("2026-10-15.0"), { "rel": "root", "href": "./catalog.json" }] });
        let item = json!({ "assets": { "azure": { "href": format!("{base}/places.parquet") } } });
        let app = Router::new()
            .route("/catalog.json", get(move || async move { Json(catalog) }))
            .route(
                "/2026-10-15.0/places/place/collection.json",
                get(|| async { Json(json!({ "links": [{ "rel": "item", "href": "./00000/00000.json" }, { "rel": "parent", "href": "x" }] })) }),
            )
            .route("/2026-10-15.0/places/place/00000/00000.json", get(move || async move { Json(item) }))
            .route("/places.parquet", get(move |headers: HeaderMap| async move { ranged(&file, &headers) }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }
}
