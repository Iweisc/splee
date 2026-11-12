//! Local business search: dentists, gyms or restaurants by what they are and where, through Google Places Text Search
//! (places.rs) with the organization's own key. Each place's search is kept for 30 days, so searching it again doesn't call
//! Google, and every call counts against the organization's monthly cap. Adding businesses to a campaign makes them
//! companies, each with one lead whose address comes from its own website (site_contacts.rs), the project's verifier and,
//! when the site names someone, its email finders.

use std::collections::{HashMap, HashSet};

use chrono::Datelike;
use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::places::{self, Place, Places};
use crate::site_contacts::{Address, Found, Sites};
use crate::state::AppState;
use crate::verify::{self, Verdict};

/// Places one search can split over, like a list of ZIP codes. Each is up to 3 calls.
pub const MAX_PLACES: usize = 20;
/// Businesses added at once. Each one's website is read before the answer, a few seconds each, 10 at a time.
pub const MAX_ADD: usize = 50;
const PARALLEL_SITES: usize = 10;
/// Addresses from one website checked with the verifier before giving up on it.
const MAX_VERIFIED: usize = 3;

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub what: String,
    /// One place, or several split by new lines or semicolons, like "78701; 78702".
    #[serde(default, rename = "where")]
    pub place: String,
    pub min_rating: Option<f32>,
    pub min_reviews: Option<i32>,
    #[serde(default)]
    pub with_website: bool,
    /// Marks the businesses this project already has.
    pub project_id: Option<Uuid>,
}

/// A business as the search shows it. `known` is "company" or "lead" when the project already has it.
#[derive(Debug, Serialize, FromRow)]
pub struct Business {
    pub place_id: String,
    pub name: String,
    pub category: String,
    pub types: Vec<String>,
    pub address: String,
    pub city: String,
    pub region: String,
    pub country_code: String,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub phone: String,
    pub website: String,
    pub domain: Option<String>,
    pub rating: Option<f32>,
    pub review_count: i32,
    pub maps_url: String,
    pub known: Option<String>,
}

/// The organization's calls to Google this calendar month (UTC), and what they cost at most.
#[derive(Debug, Serialize, PartialEq)]
pub struct Usage {
    pub connected: bool,
    pub calls: i64,
    pub cap: i32,
    pub price_per_1000_cents: i64,
    pub cost_cents: i64,
}

#[derive(Debug, Serialize)]
pub struct Results {
    pub items: Vec<Business>,
    /// Businesses found before the filters.
    pub found: usize,
    /// Calls this search made. 0 when every place came from the last 30 days' searches.
    pub calls: i64,
    pub usage: Usage,
    /// Why some places weren't searched, like the monthly cap.
    pub note: Option<String>,
}

pub async fn usage(db: &PgPool, org_id: Uuid) -> sqlx::Result<Usage> {
    let (connected, cap, calls): (bool, i32, i64) = sqlx::query_as(
        "select k.org_id is not null, coalesce(k.monthly_cap, 2000),
           coalesce((select calls from places_usage where org_id = $1 and month = date_trunc('month', now() at time zone 'utc')::date), 0)::bigint
         from (select $1::uuid as org_id) o left join places_keys k on k.org_id = o.org_id",
    )
    .bind(org_id)
    .fetch_one(db)
    .await?;
    let price = places::PRICE_PER_1000_CENTS;
    Ok(Usage { connected, calls, cap, price_per_1000_cents: price, cost_cents: (calls * price + 999) / 1000 })
}

/// The organization's Google client and monthly cap.
async fn client(state: &AppState, org_id: Uuid) -> AppResult<(Places, i32)> {
    let row: Option<(String, i32)> =
        sqlx::query_as("select key_enc, monthly_cap from places_keys where org_id = $1").bind(org_id).fetch_optional(&state.db).await?;
    let (enc, cap) = row.ok_or_else(|| AppError::BadRequest("Connect a Google Maps key under Integrations first".into()))?;
    Ok((Places::new(&state.cfg.places_api_url, &crypto::decrypt(&state.cfg.app_secret, &enc)?), cap))
}

/// Checks the key with Google, then saves it. The cap stays as it was.
pub async fn connect(state: &AppState, org_id: Uuid, key: &str) -> AppResult<Usage> {
    let key = key.trim();
    if key.is_empty() {
        return Err(AppError::BadRequest("Paste the API key".into()));
    }
    Places::new(&state.cfg.places_api_url, key).check().await.map_err(|e| AppError::BadRequest(e.to_string()))?;
    sqlx::query("insert into places_keys (org_id, key_enc) values ($1, $2) on conflict (org_id) do update set key_enc = excluded.key_enc")
        .bind(org_id)
        .bind(crypto::encrypt(&state.cfg.app_secret, key))
        .execute(&state.db)
        .await?;
    Ok(usage(&state.db, org_id).await?)
}

/// Counts one call, or refuses it once the month's calls reach the cap.
async fn spend(db: &PgPool, org_id: Uuid, cap: i32) -> sqlx::Result<bool> {
    let counted: Option<i32> = sqlx::query_scalar(
        "insert into places_usage as u (org_id, month, calls) select $1, date_trunc('month', now() at time zone 'utc')::date, 1 where $2 > 0
         on conflict (org_id, month) do update set calls = u.calls + 1 where u.calls < $2
         returning calls",
    )
    .bind(org_id)
    .bind(cap)
    .fetch_optional(db)
    .await?;
    Ok(counted.is_some())
}

fn capped(cap: i32) -> String {
    let next = (chrono::Utc::now().date_naive().with_day(1).expect("day 1 exists") + chrono::Months::new(1)).format("%B %-d");
    format!("This month's Google Maps calls reached your cap of {}. Raise the cap under Integrations, or search again on {next}.", thousands(cap))
}

/// 2000 as "2,000".
fn thousands(n: i32) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Lowercase, with runs of spaces as one, so the same search typed differently is the same search.
fn plain(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn places_of(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for place in raw.split(['\n', ';']).map(|p| p.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|p| !p.is_empty()) {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&place)) {
            out.push(place);
        }
    }
    out
}

/// Searches each place, from the last 30 days' searches when it can, then keeps the businesses that pass the filters, in
/// Google's order, each once.
pub async fn search(state: &AppState, org_id: Uuid, q: &SearchQuery) -> AppResult<Results> {
    let what = q.what.split_whitespace().collect::<Vec<_>>().join(" ");
    let wheres = places_of(&q.place);
    if what.is_empty() || wheres.is_empty() {
        return Err(AppError::BadRequest("Say what to look for and where, like dentists in Austin, TX".into()));
    }
    if wheres.len() > MAX_PLACES {
        return Err(AppError::BadRequest(format!("Split a search over at most {MAX_PLACES} places")));
    }
    let (google, cap) = client(state, org_id).await?;
    let db = &state.db;
    sqlx::query("delete from places_searches where org_id = $1 and searched_at < now() - interval '30 days'").bind(org_id).execute(db).await?;
    sqlx::query("delete from places where org_id = $1 and fetched_at < now() - interval '30 days'").bind(org_id).execute(db).await?;

    let (mut ids, mut calls, mut note): (Vec<String>, i64, Option<String>) = (Vec::new(), 0, None);
    for place in &wheres {
        let cached: Option<Vec<String>> = sqlx::query_scalar("select place_ids from places_searches where org_id = $1 and what = $2 and place = $3")
            .bind(org_id)
            .bind(plain(&what))
            .bind(plain(place))
            .fetch_optional(db)
            .await?;
        if let Some(cached) = cached {
            ids.extend(cached);
            continue;
        }
        let (mut found, mut token, mut complete): (Vec<Place>, Option<String>, bool) = (Vec::new(), None, false);
        for _ in 0..places::MAX_PAGES {
            if !spend(db, org_id, cap).await? {
                note = Some(capped(cap));
                break;
            }
            calls += 1;
            match google.search(&format!("{what} in {place}"), token.as_deref()).await {
                Ok(page) => {
                    found.extend(page.places);
                    token = page.next;
                    if token.is_none() {
                        complete = true;
                        break;
                    }
                }
                Err(e) => {
                    note = Some(e.to_string());
                    break;
                }
            }
        }
        // Google's paging ends at 60, so a third page with more after it is as complete as a search gets.
        complete |= note.is_none();
        save(db, org_id, &found).await?;
        let page_ids: Vec<String> = found.iter().map(|p| p.place_id.clone()).collect();
        if complete {
            sqlx::query(
                "insert into places_searches (org_id, what, place, place_ids) values ($1, $2, $3, $4)
                 on conflict (org_id, what, place) do update set place_ids = excluded.place_ids, searched_at = now()",
            )
            .bind(org_id)
            .bind(plain(&what))
            .bind(plain(place))
            .bind(&page_ids)
            .execute(db)
            .await?;
        }
        ids.extend(page_ids);
        if let Some(note) = &note {
            if ids.is_empty() {
                return Err(if note.starts_with("This month's") { AppError::TooManyRequests(note.clone()) } else { AppError::BadRequest(note.clone()) });
            }
            break;
        }
    }

    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(id.clone()));
    let all = businesses(db, org_id, q.project_id, &ids).await?;
    let found = all.len();
    let items = all
        .into_iter()
        .filter(|b| q.min_rating.is_none_or(|min| b.rating.is_some_and(|r| r >= min)))
        .filter(|b| q.min_reviews.is_none_or(|min| b.review_count >= min))
        .filter(|b| !q.with_website || !b.website.is_empty())
        .collect();
    Ok(Results { items, found, calls, usage: usage(db, org_id).await?, note })
}

async fn save(db: &PgPool, org_id: Uuid, found: &[Place]) -> sqlx::Result<()> {
    for p in found {
        sqlx::query(
            "insert into places (org_id, place_id, name, category, types, address, city, region, country_code, lat, lng, phone, website, domain,
               rating, review_count, maps_url)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
             on conflict (org_id, place_id) do update set name = excluded.name, category = excluded.category, types = excluded.types,
               address = excluded.address, city = excluded.city, region = excluded.region, country_code = excluded.country_code, lat = excluded.lat,
               lng = excluded.lng, phone = excluded.phone, website = excluded.website, domain = excluded.domain, rating = excluded.rating,
               review_count = excluded.review_count, maps_url = excluded.maps_url, fetched_at = now()",
        )
        .bind(org_id)
        .bind(&p.place_id)
        .bind(&p.name)
        .bind(&p.category)
        .bind(&p.types)
        .bind(&p.address)
        .bind(&p.city)
        .bind(&p.region)
        .bind(&p.country_code)
        .bind(p.lat)
        .bind(p.lng)
        .bind(&p.phone)
        .bind(&p.website)
        .bind(crate::routes::util_input::domain(&p.website))
        .bind(p.rating)
        .bind(p.review_count)
        .bind(&p.maps_url)
        .execute(db)
        .await?;
    }
    Ok(())
}

/// The organization's kept businesses with these ids, in their order, marked when `project_id` has them already: as a
/// company by domain or place, else as a lead at that domain or with that phone.
async fn businesses(db: &PgPool, org_id: Uuid, project_id: Option<Uuid>, ids: &[String]) -> sqlx::Result<Vec<Business>> {
    sqlx::query_as(
        "select p.place_id, p.name, p.category, p.types, p.address, p.city, p.region, p.country_code, p.lat, p.lng, p.phone, p.website,
           p.domain::text, p.rating, p.review_count, p.maps_url,
           case when exists (select 1 from companies c where c.project_id = $3 and (c.domain = p.domain or (c.place_id <> '' and c.place_id = p.place_id)))
                  then 'company'
                when exists (select 1 from leads l where l.project_id = $3 and (l.company_domain = p.domain or (p.phone like '+%' and l.phone = p.phone)))
                  then 'lead' end as known
         from places p where p.org_id = $1 and p.place_id = any($2)
         order by array_position($2, p.place_id)",
    )
    .bind(org_id)
    .bind(ids)
    .bind(project_id)
    .fetch_all(db)
    .await
}

/// A business that became a company or lead, and the address found for it, if any. `email_kind` is "person", "role" or "".
#[derive(Debug, Serialize, PartialEq)]
pub struct AddedBusiness {
    pub name: String,
    pub domain: Option<String>,
    pub phone: String,
    pub email: Option<String>,
    pub email_kind: &'static str,
    pub contact_name: String,
    pub note: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Skipped {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct AddOutcome {
    pub added: Vec<AddedBusiness>,
    pub skipped: Vec<Skipped>,
}

/// Adds the businesses to the campaign: those with a website as companies, and each as one lead with the best address on
/// its website. One without a website is a lead with only its phone, for calls and texts. The caller checks that the
/// campaign belongs to the project and the project to the organization.
pub async fn add(state: &AppState, org_id: Uuid, project_id: Uuid, segment_id: Uuid, place_ids: &[String]) -> AppResult<AddOutcome> {
    let mut ids: Vec<String> = Vec::new();
    for id in place_ids.iter().map(|id| id.trim()).filter(|id| !id.is_empty()) {
        if !ids.iter().any(|i| i == id) {
            ids.push(id.to_string());
        }
    }
    if ids.is_empty() {
        return Err(AppError::BadRequest("Pick at least one business".into()));
    }
    if ids.len() > MAX_ADD {
        return Err(AppError::BadRequest(format!("Add at most {MAX_ADD} businesses at a time")));
    }
    let db = &state.db;
    let listed = businesses(db, org_id, Some(project_id), &ids).await?;
    let suppressed: HashSet<String> =
        sqlx::query_scalar::<_, String>("select lower(value::text) from suppressions where org_id = $1").bind(org_id).fetch_all(db).await?.into_iter().collect();
    let mut skipped: Vec<Skipped> = ids
        .iter()
        .filter(|id| !listed.iter().any(|b| &b.place_id == *id))
        .map(|id| Skipped { name: id.clone(), reason: "Splee keeps a search for 30 days. Search again to add it.".into() })
        .collect();
    let mut picked: Vec<(Business, Option<Uuid>)> = Vec::new();
    for b in listed {
        let off = |v: &str| !v.is_empty() && suppressed.contains(&v.to_lowercase());
        let reason = if b.known.is_some() {
            Some("Already in this project")
        } else if b.domain.as_deref().is_some_and(off) || off(&b.phone) {
            Some("On the do-not-contact list")
        } else if b.domain.is_none() && !b.phone.starts_with('+') {
            Some("No website or phone number to reach them at")
        } else {
            None
        };
        match reason {
            Some(reason) => skipped.push(Skipped { name: b.name, reason: reason.into() }),
            None => picked.push((b, None)),
        }
    }

    for (b, company) in picked.iter_mut().filter(|(b, _)| b.domain.is_some()) {
        *company = sqlx::query_scalar(
            "insert into companies (project_id, segment_id, domain, name, description, city, country_code, fit_reason, source_url, phone, address,
               place_id, status)
             values ($1, $2, $3, $4, $5, $6, $7, 'Picked from Google Maps', $8, $9, $10, $11, 'people_found')
             on conflict (project_id, domain) do nothing returning id",
        )
        .bind(project_id)
        .bind(segment_id)
        .bind(&b.domain)
        .bind(&b.name)
        .bind(&b.category)
        .bind(&b.city)
        .bind(&b.country_code)
        .bind(&b.maps_url)
        .bind(&b.phone)
        .bind(&b.address)
        .bind(&b.place_id)
        .fetch_optional(db)
        .await?;
    }
    // Two businesses on one website, like a chain's branches, make one company.
    let (picked, taken): (Vec<_>, Vec<_>) = picked.into_iter().partition(|(b, company)| b.domain.is_none() || company.is_some());
    skipped.extend(taken.into_iter().map(|(b, _)| Skipped { name: b.name, reason: "Already in this project".into() }));

    let sites = Sites::new(&state.cfg.local_sites_url);
    let domains: Vec<String> = picked.iter().filter_map(|(b, _)| b.domain.clone()).collect();
    let read: HashMap<String, Found> = stream::iter(domains)
        .map(|domain| {
            let sites = &sites;
            async move { (domain.clone(), sites.find(&domain).await) }
        })
        .buffer_unordered(PARALLEL_SITES)
        .collect()
        .await;

    let mut added = Vec::new();
    let mut queued = false;
    for (b, company) in picked {
        let found = b.domain.as_ref().and_then(|d| read.get(d));
        let contact = match found {
            Some(found) => pick(state, project_id, &suppressed, found).await?,
            None => Contact::default(),
        };
        let (search, note) = match (&b.domain, found, &contact.address) {
            (_, _, Some((a, _))) if a.person => ("found", "Found on its website".to_string()),
            (_, _, Some(_)) => ("found", "Only a shared address on its website".to_string()),
            (Some(_), _, None) if !contact.name.is_empty() => {
                queued = true;
                ("queued", format!("Its website names {}. The email finders look for their address next.", contact.name))
            }
            (Some(domain), Some(f), None) if f.blocked => ("not_found", format!("{domain} asks robots not to read its pages")),
            (Some(domain), Some(f), None) if !f.read => ("not_found", format!("{domain} didn't answer")),
            (Some(domain), _, None) if contact.refused => ("not_found", format!("The addresses on {domain} didn't verify")),
            (Some(domain), _, None) => ("not_found", format!("{domain} lists no address of its own")),
            (None, _, _) => ("not_found", "No website. Call or text them instead".to_string()),
        };
        let (first, last) = contact.name.split_once(' ').unwrap_or((&contact.name, ""));
        let lead: Option<Uuid> = sqlx::query_scalar(
            "insert into leads (project_id, segment_id, company_id, full_name, first_name, last_name, email, email_status, email_source, email_search,
               email_search_note, email_searched_at, company_name, company_domain, city, country_code, phone, source)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, case when $10 = 'queued' then null else now() end, $12, $13, $14, $15, $16, 'google_maps')
             on conflict do nothing returning id",
        )
        .bind(project_id)
        .bind(segment_id)
        .bind(company)
        .bind(&contact.name)
        .bind(first)
        .bind(last)
        .bind(contact.address.as_ref().map(|(a, _)| &a.email))
        .bind(contact.address.as_ref().map_or("unknown", |(_, status)| *status))
        .bind(if contact.address.is_some() { "Website" } else { "" })
        .bind(search)
        .bind(&note)
        .bind(&b.name)
        .bind(&b.domain)
        .bind(&b.city)
        .bind(&b.country_code)
        .bind(if b.phone.starts_with('+') { b.phone.as_str() } else { "" })
        .fetch_optional(db)
        .await?;
        if lead.is_none() {
            skipped.push(Skipped { name: b.name, reason: "Its address is already another lead's".into() });
            continue;
        }
        let email_kind = match &contact.address {
            Some((a, _)) if a.person => "person",
            Some(_) => "role",
            None => "",
        };
        added.push(AddedBusiness {
            name: b.name,
            domain: b.domain,
            phone: b.phone,
            email: contact.address.map(|(a, _)| a.email),
            email_kind,
            contact_name: contact.name,
            note,
        });
    }
    if queued {
        crate::waterfall::wake();
    }
    Ok(AddOutcome { added, skipped })
}

/// The address a business's lead gets, with its email_status, and the name of the person it belongs to, if the site spells
/// one. `refused` says the verifier turned every address down.
#[derive(Default)]
struct Contact {
    address: Option<(Address, &'static str)>,
    name: String,
    refused: bool,
}

/// The best address on the site that isn't on the do-not-contact list, already a lead's, or turned down by the verifier.
async fn pick(state: &AppState, project_id: Uuid, suppressed: &HashSet<String>, found: &Found) -> sqlx::Result<Contact> {
    let mut contact = Contact::default();
    // The first person the site names, for the email finders when none of the addresses will do.
    let mut named = String::new();
    let mut checked = 0;
    for a in &found.addresses {
        let domain = a.email.rsplit('@').next().unwrap_or_default();
        if suppressed.contains(&a.email) || suppressed.contains(domain) {
            continue;
        }
        let taken: bool = sqlx::query_scalar("select exists (select 1 from leads where project_id = $1 and lower(email::text) = $2)")
            .bind(project_id)
            .bind(&a.email)
            .fetch_one(&state.db)
            .await?;
        if taken {
            continue;
        }
        if named.is_empty() {
            named = a.name.clone();
        }
        if checked == MAX_VERIFIED {
            break;
        }
        checked += 1;
        match verify::verify(state, project_id, &a.email, false).await {
            Some(Verdict::Invalid) => contact.refused = true,
            Some(Verdict::Valid) => {
                contact.name = a.name.clone();
                contact.address = Some((a.clone(), "verified"));
                break;
            }
            Some(Verdict::Unsure) | None => {
                contact.name = a.name.clone();
                contact.address = Some((a.clone(), "found"));
                break;
            }
        }
    }
    if contact.address.is_none() {
        contact.name = named;
    }
    Ok(contact)
}
