//! The company database: businesses from Overture Maps' places (https://docs.overturemaps.org/guides/places/), an open
//! dataset of about 88 million places under CDLA Permissive 2.0, kept one row per website so each company appears once.
//! `splee import-companies [COUNTRY…]` loads it. Overture publishes its places as GeoParquet files every month, and they're
//! read straight over HTTPS, a few byte ranges at a time, so nothing large is ever written to disk.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

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
use serde_json::Value;
use sqlx::PgPool;

const CATALOG: &str = "https://stac.overturemaps.org/catalog.json";
/// Byte ranges closer than this are fetched in one request, since a row group's columns sit side by side.
const MERGE_GAP: u64 = 1 << 20;
/// Files read at once.
const PARALLEL_FILES: usize = 4;
const COLUMNS: [&str; 10] =
    ["id", "names.primary", "websites", "phones", "emails", "addresses", "taxonomy.primary", "taxonomy.hierarchy", "confidence", "operating_status"];

/// Imports every place with a website in `countries` (ISO codes like GB), or everywhere when it's empty, and then removes
/// the companies of those countries that the latest release no longer has.
pub async fn import(db: &PgPool, countries: &[String]) -> anyhow::Result<()> {
    // Overture's catalog refuses requests without a user agent.
    let client = reqwest::Client::builder().user_agent("splee").timeout(Duration::from_secs(120)).build()?;
    let (release, files) = latest_files(&client).await?;
    println!("Overture release {release}: {} files", files.len());
    let countries: Vec<String> = countries.iter().map(|c| c.trim().to_uppercase()).collect();
    let release = release.as_str();
    let kept: Vec<usize> = futures::stream::iter(files.into_iter().enumerate())
        .map(|(i, url)| {
            let (client, countries) = (client.clone(), &countries);
            async move {
                let kept = import_file(db, client, url, release, countries).await.with_context(|| format!("file {i}"))?;
                if kept > 0 {
                    println!("file {i}: {kept} companies");
                }
                anyhow::Ok(kept)
            }
        })
        .buffer_unordered(PARALLEL_FILES)
        .try_collect()
        .await?;
    let gone = if countries.is_empty() {
        sqlx::query("delete from directory_companies where release <> $1").bind(release).execute(db).await?
    } else {
        sqlx::query("delete from directory_companies where release <> $1 and country_code = any($2)").bind(release).bind(&countries).execute(db).await?
    };
    println!("Imported {} companies. Removed {} that are no longer listed.", kept.iter().sum::<usize>(), gone.rows_affected());
    Ok(())
}

/// The newest release's places files, from Overture's STAC catalog.
async fn latest_files(client: &reqwest::Client) -> anyhow::Result<(String, Vec<Url>)> {
    let get = |url: String| async move { anyhow::Ok(client.get(url).send().await?.error_for_status()?.json::<Value>().await?) };
    let links = |v: &Value, rel: &str| -> Vec<String> {
        v["links"].as_array().into_iter().flatten().filter(|l| l["rel"] == rel).filter_map(|l| l["href"].as_str().map(str::to_string)).collect()
    };
    let catalog = get(CATALOG.into()).await?;
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

async fn import_file(db: &PgPool, client: reqwest::Client, url: Url, release: &str, countries: &[String]) -> anyhow::Result<usize> {
    let head = client.head(url.clone()).send().await?.error_for_status()?;
    // reqwest's content_length() is the HEAD response's own empty body, so the header has to be read directly.
    let size: u64 = head.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse().ok()).context("no file size")?;
    let file = HttpFile { client, url, size };
    let builder = ParquetRecordBatchStreamBuilder::new(file).await?;
    let groups = row_groups(builder.metadata(), countries);
    let mask = ProjectionMask::columns(builder.parquet_schema(), COLUMNS);
    let mut stream = builder.with_projection(mask).with_row_groups(groups).with_batch_size(8192).build()?;
    let mut kept = 0;
    while let Some(batch) = stream.next().await {
        let companies = companies(&batch?, countries);
        kept += save(db, &companies, release).await?;
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
async fn save(db: &PgPool, companies: &[Company], release: &str) -> anyhow::Result<usize> {
    if companies.is_empty() {
        return Ok(0);
    }
    macro_rules! column {
        ($field:ident) => {
            companies.iter().map(|c| c.$field.clone()).collect::<Vec<_>>()
        };
    }
    // Postgres can't unnest a text[][], so the categories go in as one JSON array per row.
    let categories: Vec<String> = companies.iter().map(|c| serde_json::to_string(&c.categories).unwrap_or_default()).collect();
    let result = sqlx::query(
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
    Ok(result.rows_affected() as usize)
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
    use super::domain;

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
}
