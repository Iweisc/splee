//! Companies like your best customers. The seeds are the companies of the project's won deals, plus or minus domains picked
//! by hand. The profile is what at least a third of them share, and every company in the database is scored against it in
//! one query that starts from the directory's indexes, so it stays fast on millions of rows. No Claude.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::{fingerprints, hiring, jobs, score};

/// One part of the profile. A company's score is the points it earns over the points of the parts that are on and have
/// something in common, so a profile of only an industry and a country still scores out of 100.
pub struct Part {
    pub key: &'static str,
    pub label: &'static str,
    pub weight: f64,
}

// Company size waits for a headcount source: Overture's places have none, so it could never score.
pub const CATEGORY: Part = Part { key: "category", label: "Industry", weight: 30.0 };
pub const TECH: Part = Part { key: "tech", label: "Tools they use", weight: 25.0 };
pub const KEYWORDS: Part = Part { key: "keywords", label: "Words at least two of them use", weight: 15.0 };
pub const HIRING: Part = Part { key: "hiring", label: "Hiring for roles you sell to", weight: 15.0 };
pub const COUNTRY: Part = Part { key: "country", label: "Country", weight: 10.0 };
pub const REGION: Part = Part { key: "region", label: "Region", weight: 5.0 };
pub const PARTS: [&Part; 6] = [&CATEGORY, &TECH, &KEYWORDS, &HIRING, &COUNTRY, &REGION];

/// Matching this many of the profile's tools or words earns the part's full points.
const FULL_AT: usize = 3;
/// Values kept per part, the most shared first.
const MAX_VALUES: usize = 5;
const MAX_WORDS: usize = 8;
pub const LIMIT: i64 = 200;
const MAX_REASONS: usize = 4;
pub const MAX_ADDED_SEEDS: usize = 100;
/// Companies with open posts for the project's roles, at most, so a common role can't make the query huge.
const MAX_HIRING: i64 = 5000;

/// Words that say nothing about what a company does.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "from", "that", "this", "our", "your", "their", "they", "them", "its", "are", "was", "were", "has", "have", "had",
    "who", "what", "which", "into", "over", "more", "most", "all", "any", "each", "one", "two", "also", "than", "then", "but", "not", "can", "will",
    "help", "helps", "helping", "make", "makes", "offer", "offers", "provide", "provides", "providing", "based", "company", "companies", "business",
    "businesses", "service", "services", "solution", "solutions", "team", "teams", "people", "customers", "clients", "since", "about", "across",
    "around", "best", "leading", "small", "large", "local", "new", "world", "global", "inc", "ltd", "llc", "gmbh", "www", "com",
];

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize, FromRow)]
pub struct Settings {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub off: Vec<String>,
}

pub async fn settings(db: &PgPool, project_id: Uuid) -> sqlx::Result<Settings> {
    let row = sqlx::query_as("select added, removed, off from lookalike_settings where project_id = $1").bind(project_id).fetch_optional(db).await?;
    Ok(row.unwrap_or_default())
}

/// The caller checked the domains and keys.
pub async fn save_settings(db: &PgPool, project_id: Uuid, s: &Settings) -> sqlx::Result<()> {
    sqlx::query(
        "insert into lookalike_settings (project_id, added, removed, off) values ($1, $2, $3, $4)
         on conflict (project_id) do update set added = excluded.added, removed = excluded.removed, off = excluded.off, updated_at = now()",
    )
    .bind(project_id)
    .bind(&s.added)
    .bind(&s.removed)
    .bind(&s.off)
    .execute(db)
    .await?;
    Ok(())
}

/// The domains of the companies the project won deals with, by the lead's company or else its company domain.
async fn won_domains(db: &PgPool, project_id: Uuid) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "select domain from (
           select lower(coalesce(c.domain::text, l.company_domain::text)) as domain, min(d.closed_at) as first_won
           from deals d join leads l on l.id = d.lead_id left join companies c on c.id = l.company_id
           where d.project_id = $1 and d.status = 'won' and coalesce(c.domain::text, l.company_domain::text) is not null
           group by 1
         ) won order by first_won nulls last, domain",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
}

/// The seeds' domains, each with whether it came from a won deal: those, then the ones added by hand, less the removed ones.
/// `chosen` replaces them all, like the API's seeds parameter.
async fn seed_domains(db: &PgPool, project_id: Uuid, s: &Settings, chosen: Option<&[String]>) -> sqlx::Result<Vec<(String, bool)>> {
    let won = won_domains(db, project_id).await?;
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut push = |domain: &str| {
        if !out.iter().any(|(d, _)| d == domain) {
            out.push((domain.to_string(), won.iter().any(|w| w == domain)));
        }
    };
    match chosen {
        Some(chosen) => chosen.iter().for_each(|d| push(d)),
        None => won.iter().chain(&s.added).filter(|d| !s.removed.contains(d)).for_each(|d| push(d)),
    }
    Ok(out)
}

pub async fn has_seeds(db: &PgPool, project_id: Uuid) -> sqlx::Result<bool> {
    Ok(!seed_domains(db, project_id, &settings(db, project_id).await?, None).await?.is_empty())
}

/// What Splee knows about a seed, from the company database, the project's company row, its tech scan and its job posts.
#[derive(Debug, Default, Clone)]
pub struct Facts {
    pub description: String,
    pub country: String,
    pub region: String,
    /// The category and its parents from the company database, the broadest first.
    pub categories: Vec<String>,
    /// None until the site has been scanned.
    pub techs: Option<Vec<String>>,
    /// The project's watched roles it has posted for, open or not. None until the hiring worker has read its job board.
    pub hiring: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Seed {
    pub domain: String,
    pub name: String,
    /// From a won deal, rather than added by hand.
    pub won: bool,
    pub in_database: bool,
    /// Splee has a company database row, a company in the project or a tech scan for it.
    pub known: bool,
    #[serde(skip)]
    pub facts: Facts,
}

#[derive(FromRow)]
struct SeedRow {
    domain: String,
    name: String,
    description: String,
    country: String,
    region: String,
    category: Option<String>,
    categories: Vec<String>,
    techs: Option<Vec<String>>,
    hiring: Option<Vec<String>>,
    in_database: bool,
    in_project: bool,
}

async fn seeds(db: &PgPool, project_id: Uuid, domains: &[(String, bool)]) -> sqlx::Result<Vec<Seed>> {
    let names: Vec<String> = domains.iter().map(|(d, _)| d.clone()).collect();
    let rows: Vec<SeedRow> = sqlx::query_as(
        "select s.domain, coalesce(nullif(c.name, ''), d.name, '') as name, coalesce(c.description, '') as description,
                coalesce(nullif(c.country_code, ''), d.country_code, '') as country, coalesce(d.region, '') as region,
                d.category, coalesce(d.categories, '{}') as categories, t.techs,
                case when exists (select 1 from hiring_checks hc join job_boards b on b.domain = hc.domain and b.provider <> ''
                                  where hc.project_id = $1 and hc.domain = s.domain::citext)
                     then array(select distinct j.role from job_openings j where j.project_id = $1 and j.domain = s.domain::citext and j.role <> '')
                end as hiring,
                d.domain is not null as in_database, c.id is not null as in_project
         from unnest($2::text[]) s(domain)
         left join directory_companies d on d.domain = s.domain
         left join companies c on c.project_id = $1 and c.domain = s.domain::citext
         left join tech_scans t on t.domain = s.domain::citext and t.scanned_at is not null",
    )
    .bind(project_id)
    .bind(&names)
    .fetch_all(db)
    .await?;
    let mut by_domain: HashMap<String, SeedRow> = rows.into_iter().map(|r| (r.domain.clone(), r)).collect();
    Ok(domains
        .iter()
        .filter_map(|(domain, won)| {
            let r = by_domain.remove(domain)?;
            let mut categories = r.categories;
            if let Some(primary) = r.category.filter(|c| !categories.contains(c)) {
                categories.push(primary);
            }
            Some(Seed {
                domain: r.domain,
                name: r.name,
                won: *won,
                in_database: r.in_database,
                known: r.in_database || r.in_project || r.techs.is_some(),
                facts: Facts {
                    description: r.description,
                    country: r.country.to_uppercase(),
                    region: r.region,
                    categories,
                    techs: r.techs,
                    hiring: r.hiring,
                },
            })
        })
        .collect())
}

/// One value in the profile, like a category, and how many seeds have it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Trait {
    pub value: String,
    pub label: String,
    pub seeds: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfilePart {
    pub key: &'static str,
    pub label: &'static str,
    /// Switched on and has values. A part with nothing in common can't be on.
    pub on: bool,
    /// The seeds Splee has this kind of data for. Shares are of these.
    pub known: usize,
    pub values: Vec<Trait>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub seeds: Vec<Seed>,
    pub parts: Vec<ProfilePart>,
}

impl Profile {
    fn part(&self, p: &Part) -> Option<&ProfilePart> {
        self.parts.iter().find(|x| x.key == p.key && x.on)
    }

    fn values(&self, p: &Part) -> Vec<String> {
        self.part(p).map(|x| x.values.iter().map(|v| v.value.clone()).collect()).unwrap_or_default()
    }

    fn seeds_with(&self, p: &Part, value: &str) -> usize {
        self.part(p).and_then(|x| x.values.iter().find(|v| v.value == value)).map_or(0, |v| v.seeds)
    }

    /// The points of every part that's on.
    fn possible(&self) -> f64 {
        PARTS.iter().filter(|p| self.part(p).is_some()).map(|p| p.weight).sum()
    }
}

/// The values that at least a third of the seeds with this kind of data share, and at least `min` of them, the most shared
/// first, and how many seeds have the data.
fn common(per_seed: impl Iterator<Item = Option<Vec<String>>>, min: usize, max: usize) -> (usize, Vec<(String, usize)>) {
    let mut known = 0;
    let mut counts: Vec<(String, usize)> = Vec::new();
    for values in per_seed.flatten() {
        known += 1;
        let mut seen = HashSet::new();
        for v in values.into_iter().filter(|v| !v.is_empty()) {
            if !seen.insert(v.clone()) {
                continue;
            }
            match counts.iter_mut().find(|(c, _)| *c == v) {
                Some((_, n)) => *n += 1,
                None => counts.push((v, 1)),
            }
        }
    }
    // Stable, so a tie keeps the order the seeds first showed the values in.
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    counts.retain(|(_, n)| *n * 3 >= known && *n >= min);
    counts.truncate(max);
    (known, counts)
}

/// The words of a description worth matching on: lowercase, at least 3 letters, not a number or a stopword.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3 && !w.chars().all(|c| c.is_ascii_digit()) && !STOPWORDS.contains(w))
        .map(String::from)
        .collect()
}

/// "dental_clinic" reads as "Dental clinic".
pub fn category_words(category: &str) -> String {
    let words = category.replace('_', " ");
    let mut chars = words.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// What the seeds have in common, part by part. `roles` are the roles the project sells to, from its hiring settings.
pub fn build(seeds: Vec<Seed>, roles: &[String], off: &[String]) -> Profile {
    let mut parts = Vec::new();
    let mut add = |p: &Part, known: usize, values: Vec<Trait>| {
        let on = !values.is_empty() && !off.iter().any(|k| k == p.key);
        parts.push(ProfilePart { key: p.key, label: p.label, on, known, values });
    };
    let traits = |counts: Vec<(String, usize)>, label: &dyn Fn(&str) -> String| {
        counts.into_iter().map(|(value, seeds)| Trait { label: label(&value), value, seeds }).collect::<Vec<_>>()
    };

    // The broadest category, like "services", says nothing, so it's left out unless it's all a seed has.
    let hierarchy = |s: &Seed| -> Option<Vec<String>> {
        let c = &s.facts.categories;
        (!c.is_empty()).then(|| if c.len() > 1 { c[1..].to_vec() } else { c.clone() })
    };
    let (known, mut counts) = common(seeds.iter().map(hierarchy), 1, usize::MAX);
    // A parent of another shared category only widens the search, so the most specific ones are kept.
    let parents: HashSet<(&str, &str)> = seeds
        .iter()
        .flat_map(|s| {
            let c = &s.facts.categories;
            (0..c.len()).flat_map(move |i| (i + 1..c.len()).map(move |j| (c[i].as_str(), c[j].as_str())))
        })
        .collect();
    let kept: Vec<String> = counts.iter().map(|(c, _)| c.clone()).collect();
    counts.retain(|(c, _)| !kept.iter().any(|k| parents.contains(&(c.as_str(), k.as_str()))));
    counts.truncate(MAX_VALUES);
    add(&CATEGORY, known, traits(counts, &|c| category_words(c)));

    let (known, counts) = common(seeds.iter().map(|s| s.facts.techs.clone()), 1, MAX_WORDS);
    add(&TECH, known, traits(counts, &|t| t.to_string()));

    // Words need two seeds, not just a third: with two or three descriptions a third is one seed, and then every word of a
    // single description would be in the profile.
    let described = |s: &Seed| Some(words(&s.facts.description)).filter(|w| !w.is_empty());
    let (known, counts) = common(seeds.iter().map(described), 2, MAX_WORDS);
    add(&KEYWORDS, known, traits(counts, &|w| w.to_string()));

    // Only the roles the project sells to, out of the seeds whose job boards Splee reads.
    let posted = |s: &Seed| {
        let hiring = s.facts.hiring.as_ref()?;
        Some(roles.iter().filter(|role| hiring.iter().any(|r| r.eq_ignore_ascii_case(role))).cloned().collect())
    };
    let (known, counts) = common(seeds.iter().map(posted), 1, MAX_VALUES);
    add(&HIRING, known, traits(counts, &|r| r.to_string()));

    let one = |v: &str| Some(vec![v.to_string()]).filter(|_| !v.is_empty());
    let (known, counts) = common(seeds.iter().map(|s| one(&s.facts.country)), 1, MAX_VALUES);
    add(&COUNTRY, known, traits(counts, &|c| c.to_string()));

    // Regions are kept as "GB|west yorkshire", since a region's name alone can be in two countries.
    let region = |s: &Seed| one(&s.facts.region).filter(|_| !s.facts.country.is_empty()).map(|_| vec![region_key(&s.facts.country, &s.facts.region)]);
    let (known, counts) = common(seeds.iter().map(region), 1, MAX_VALUES);
    let shown: HashMap<String, String> = seeds
        .iter()
        .filter(|s| !s.facts.region.is_empty())
        .map(|s| (region_key(&s.facts.country, &s.facts.region), format!("{}, {}", s.facts.region, s.facts.country)))
        .collect();
    add(&REGION, known, traits(counts, &|k| shown.get(k).cloned().unwrap_or_default()));

    Profile { seeds, parts }
}

fn region_key(country: &str, region: &str) -> String {
    format!("{}|{}", country.to_uppercase(), region.trim().to_lowercase())
}

/// The companies with open posts for the project's roles, from any project's job board checks, since a job post is public
/// like a tech scan. Each with the role its newest post is for.
async fn hiring_now(db: &PgPool, roles: &[String]) -> sqlx::Result<HashMap<String, String>> {
    if roles.is_empty() {
        return Ok(HashMap::new());
    }
    let patterns: Vec<String> = roles.iter().map(|r| format!("%{}%", r.trim().to_lowercase().replace(['%', '_'], ""))).collect();
    let posts: Vec<(String, String)> = sqlx::query_as(
        "select lower(domain::text), title from job_openings where closed_at is null and lower(title) like any($1)
         order by coalesce(posted_at, first_seen) desc limit $2",
    )
    .bind(&patterns)
    .bind(MAX_HIRING)
    .fetch_all(db)
    .await?;
    let mut out = HashMap::new();
    for (domain, title) in posts {
        if let Some(role) = jobs::matching_roles(&title, roles).first() {
            out.entry(domain).or_insert_with(|| role.to_string());
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct Match {
    pub domain: String,
    pub name: String,
    pub website: String,
    pub city: Option<String>,
    pub region: Option<String>,
    pub country_code: String,
    pub category: Option<String>,
    /// 0-100, the share of the profile's points it earns.
    pub score: i16,
    /// What earned the most points, at most four.
    pub reasons: Vec<String>,
}

#[derive(FromRow)]
struct Row {
    domain: String,
    name: String,
    website: String,
    city: Option<String>,
    region: Option<String>,
    country_code: String,
    category: Option<String>,
    /// The first of the profile's categories it's in, from 1.
    category_at: Option<i64>,
    /// The profile's words its name or category has, from 1.
    keyword_hits: Vec<i32>,
    tech_hits: Vec<String>,
    hiring_role: Option<String>,
    country_hit: bool,
    region_hit: bool,
}

/// Points for matching `hits` of a part's `total` values.
fn share(hits: usize, total: usize) -> f64 {
    hits.min(FULL_AT) as f64 / total.clamp(1, FULL_AT) as f64
}

/// The ranking query for a profile and what it binds, in the order of its parameters. See `bind!`.
pub(crate) struct Ranking {
    pub sql: String,
    project_id: Uuid,
    org_id: Option<Uuid>,
    known: Vec<String>,
    cats: Vec<String>,
    words: Vec<String>,
    tools: Vec<String>,
    hiring_domains: Vec<String>,
    hiring_roles: Vec<String>,
    countries: Vec<String>,
    region_countries: Vec<String>,
    region_names: Vec<String>,
    limit: i64,
    /// The words as one tsquery that matches any of them. Only letters and digits get into words, so it can't hold syntax.
    /// Whole words, not prefixes: Postgres can't estimate how many names a prefix finds and then reads the whole table.
    any_word: String,
}

/// Binds a Ranking's values to a query of its SQL. A macro, since query_as and query_scalar have no common bind trait.
macro_rules! bind {
    ($query:expr, $r:expr) => {
        $query
            .bind($r.project_id)
            .bind($r.org_id)
            .bind(&$r.known)
            .bind(&$r.cats)
            .bind(&$r.words)
            .bind(&$r.tools)
            .bind(&$r.hiring_domains)
            .bind(&$r.hiring_roles)
            .bind(&$r.countries)
            .bind(&$r.region_countries)
            .bind(&$r.region_names)
            .bind($r.limit)
            .bind(&$r.any_word)
    };
}

/// The best `limit` companies in the database for the profile. Seeds, the project's own companies, domain and competitors,
/// and anything on the organization's do-not-contact list are left out.
pub async fn rank(db: &PgPool, project_id: Uuid, org_id: Option<Uuid>, profile: &Profile, limit: i64) -> sqlx::Result<Vec<Match>> {
    let Some(r) = ranking(db, project_id, org_id, profile, limit).await? else { return Ok(Vec::new()) };
    let rows: Vec<Row> = bind!(sqlx::query_as(&r.sql), r).fetch_all(db).await?;
    let possible = profile.possible();
    Ok(rows
        .into_iter()
        .map(|row| {
            let (points, reasons) = explain(profile, &row, &r.cats, &r.words);
            Match {
                score: if possible > 0.0 { (points / possible * 100.0).round() as i16 } else { 0 },
                reasons,
                domain: row.domain,
                name: row.name,
                website: row.website,
                city: row.city,
                region: row.region,
                country_code: row.country_code,
                category: row.category,
            }
        })
        .collect())
}

/// Seeds, the project's companies, domain and competitors, and the organization's do-not-contact list.
const EXCLUDED: &str = "d.domain <> all($3)
             and not exists (select 1 from companies c where c.project_id = $1 and c.domain = d.domain::citext)
             and not exists (select 1 from suppressions s where s.org_id = $2 and s.value in (d.domain, d.email, split_part(d.email, '@', 2)))";

/// None when no part is on.
pub(crate) async fn ranking(db: &PgPool, project_id: Uuid, org_id: Option<Uuid>, profile: &Profile, limit: i64) -> sqlx::Result<Option<Ranking>> {
    let (cats, words, tools, countries) = (profile.values(&CATEGORY), profile.values(&KEYWORDS), profile.values(&TECH), profile.values(&COUNTRY));
    let (region_countries, region_names): (Vec<String>, Vec<String>) = profile
        .values(&REGION)
        .iter()
        .filter_map(|k| k.split_once('|').map(|(c, r)| (c.to_string(), r.to_string())))
        .unzip();
    let hiring = match profile.part(&HIRING) {
        Some(part) => hiring_now(db, &part.values.iter().map(|v| v.value.clone()).collect::<Vec<_>>()).await?,
        None => HashMap::new(),
    };
    let (hiring_domains, hiring_roles): (Vec<String>, Vec<String>) = hiring.into_iter().unzip();

    // Each part that's on picks companies through an index, and Postgres ORs their bitmaps: the categories' and the names'
    // GIN indexes, and the primary key for the domains the tech scans' GIN index, the hiring list or the places found.
    let mut picks: Vec<String> = Vec::new();
    if !cats.is_empty() {
        picks.push("d.categories && $4".into());
    }
    if !words.is_empty() {
        picks.push("d.search @@ to_tsquery('simple', $13)".into());
    }
    if !tools.is_empty() {
        picks.push("d.domain = any(array(select lower(t.domain::text) from tech_scans t where t.techs && $6 and t.scanned_at is not null))".into());
    }
    if !hiring_domains.is_empty() {
        picks.push("d.domain = any($7)".into());
    }
    // A whole country can be millions of companies, so only each country's and region's best `limit` come in, in the final
    // tie order. That loses nothing: a company that only shares the place scores what the place's companies above it score,
    // or less, and `limit` of them already rank above it.
    let mut places: Vec<String> = Vec::new();
    let best_in = |place: &str| {
        format!(
            "cross join lateral (select d.domain from directory_companies d where {place} and {EXCLUDED}
                                 order by d.confidence desc, d.domain limit $12) x"
        )
    };
    if !countries.is_empty() {
        places.push(format!("select x.domain from unnest($9::text[]) g(country) {}", best_in("d.country_code = g.country")));
    }
    if !region_names.is_empty() {
        let in_region = best_in("d.country_code = g.country and lower(d.region) = g.region");
        places.push(format!("select x.domain from unnest($10::text[], $11::text[]) g(country, region) {in_region}"));
    }
    if !places.is_empty() {
        picks.push(format!("d.domain = any(array({}))", places.join(" union all ")));
    }
    if picks.is_empty() {
        return Ok(None);
    }

    // Every picked company is scored with plain expressions, one per value, which cost far less per row than subqueries.
    // Which values matched, for the reasons, is only worked out for the best `limit`.
    let sum = |n: usize, term: &dyn Fn(usize) -> String| (1..=n).map(term).collect::<Vec<_>>().join(" + ");
    let mut points: Vec<String> = Vec::new();
    let mut weigh = |p: &Part, on: bool, expr: String| {
        if on {
            points.push(format!("{} * ({expr})", p.weight));
        }
    };
    weigh(&CATEGORY, !cats.is_empty(), "(d.categories && $4)::int".into());
    let words_hit = sum(words.len(), &|i| format!("(d.search @@ kw.q[{i}])::int"));
    weigh(&KEYWORDS, !words.is_empty(), format!("least({words_hit}, {FULL_AT})::float8 / {}", words.len().clamp(1, FULL_AT)));
    let tools_hit = sum(tools.len(), &|i| format!("coalesce(t.techs @> array[$6[{i}]], false)::int"));
    weigh(&TECH, !tools.is_empty(), format!("least({tools_hit}, {FULL_AT})::float8 / {}", tools.len().clamp(1, FULL_AT)));
    weigh(&HIRING, profile.part(&HIRING).is_some(), "(h.role is not null)::int".into());
    weigh(&COUNTRY, !countries.is_empty(), "(d.country_code = any($9))::int".into());
    let in_region = (1..=region_names.len()).map(|i| format!("(d.country_code = $10[{i}] and lower(d.region) = $11[{i}])")).collect::<Vec<_>>();
    weigh(&REGION, !region_names.is_empty(), format!("coalesce({}, false)::int", in_region.join(" or ")));

    let sql = format!(
        "with kw as (select coalesce(array_agg(to_tsquery('simple', k) order by n), '{{}}') as q from unnest($5::text[]) with ordinality u(k, n)),
         best as (
           select d.domain, d.name, d.website, d.city, d.region, d.country_code, d.category, d.categories, d.search, d.confidence,
                  t.techs, h.role as hiring_role, {points} as points
           from directory_companies d
           cross join kw
           left join tech_scans t on t.domain = d.domain::citext and t.scanned_at is not null
           left join unnest($7::text[], $8::text[]) h(domain, role) on h.domain = d.domain
           where ({picks})
             and {EXCLUDED}
           order by points desc, d.confidence desc, d.domain
           limit $12
         )
         select b.domain, b.name, b.website, b.city, b.region, b.country_code, b.category, b.hiring_role,
                (select min(c.n) from unnest($4::text[]) with ordinality c(v, n) where c.v = any(b.categories)) as category_at,
                array(select i::int from generate_subscripts(kw.q, 1) i where b.search @@ kw.q[i] order by i) as keyword_hits,
                array(select x.name from unnest($6::text[]) with ordinality x(name, n) where x.name = any(b.techs) order by x.n) as tech_hits,
                b.country_code = any($9) as country_hit,
                exists (select 1 from unnest($10::text[], $11::text[]) g(c, r) where g.c = b.country_code and g.r = lower(b.region)) as region_hit
         from best b cross join kw
         order by b.points desc, b.confidence desc, b.domain",
        picks = picks.join(" or "),
        points = if points.is_empty() { "0".to_string() } else { points.join(" + ") },
    );
    let known = known_domains(db, project_id, profile).await?;
    let any_word = words.join(" | ");
    Ok(Some(Ranking { sql, project_id, org_id, known, cats, words, tools, hiring_domains, hiring_roles, countries, region_countries, region_names, limit, any_word }))
}

/// The seeds, the project's companies are left out in the query, and its own domain and competitors.
async fn known_domains(db: &PgPool, project_id: Uuid, profile: &Profile) -> sqlx::Result<Vec<String>> {
    let mut known: Vec<String> = sqlx::query_scalar(
        "select lower(domain::text) from projects where id = $1 and domain is not null
         union select lower(domain::text) from competitors where project_id = $1",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    known.extend(profile.seeds.iter().map(|s| s.domain.clone()));
    Ok(known)
}

/// "like 3 of your 4 customers"
fn like(n: usize, of: usize) -> String {
    match (n, of) {
        (_, 1) => "like your customer".into(),
        (2, 2) => "like both of your customers".into(),
        (n, of) if n == of => format!("like all {of} of your customers"),
        (n, of) => format!("like {n} of your {of} customers"),
    }
}

/// The row's points and the reasons that earned the most, in the same terms as the query's ORDER BY.
fn explain(profile: &Profile, r: &Row, cats: &[String], words: &[String]) -> (f64, Vec<String>) {
    let total = profile.seeds.len();
    let mut found: Vec<(f64, String)> = Vec::new();
    if let Some(cat) = r.category_at.and_then(|i| cats.get(i as usize - 1)) {
        found.push((CATEGORY.weight, format!("{}, {}", category_words(cat), like(profile.seeds_with(&CATEGORY, cat), total))));
    }
    if profile.part(&TECH).is_some() && !r.tech_hits.is_empty() {
        let tools = profile.values(&TECH);
        let uses = |s: &&Seed, t: &String| s.facts.techs.as_ref().is_some_and(|ts| ts.contains(t));
        let all = profile.seeds.iter().filter(|s| r.tech_hits.iter().all(|t| uses(s, t))).count();
        let seeds = if all > 0 { all } else { r.tech_hits.iter().map(|t| profile.seeds_with(&TECH, t)).max().unwrap_or(0) };
        let line = fingerprints::fact_line(&r.tech_hits).unwrap_or_default();
        found.push((TECH.weight * share(r.tech_hits.len(), tools.len()), format!("{line}, {}", like(seeds, total))));
    }
    if !r.keyword_hits.is_empty() {
        let hits: Vec<String> = r.keyword_hits.iter().filter_map(|i| words.get(*i as usize - 1)).map(|w| format!("“{w}”")).collect();
        let list = match hits.as_slice() {
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
            [] => String::new(),
        };
        let text = format!("Its name or category says {list}, like your customers' descriptions");
        found.push((KEYWORDS.weight * share(hits.len(), words.len()), text));
    }
    if let Some(role) = &r.hiring_role {
        found.push((HIRING.weight, format!("Hiring {} {role}", score::article(role))));
    }
    let country = if r.country_hit && profile.part(&COUNTRY).is_some() { COUNTRY.weight } else { 0.0 };
    match r.region.as_deref().filter(|_| r.region_hit && profile.part(&REGION).is_some()) {
        // The region says the country too, so they're one reason.
        Some(region) => {
            let seeds = profile.seeds_with(&REGION, &region_key(&r.country_code, region));
            found.push((country + REGION.weight, format!("In {region}, {}, {}", r.country_code, like(seeds, total))));
        }
        None if country > 0.0 => {
            found.push((country, format!("Based in {}, {}", r.country_code, like(profile.seeds_with(&COUNTRY, &r.country_code), total))));
        }
        None => {}
    }
    let points = found.iter().map(|(p, _)| p).sum();
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    (points, found.into_iter().take(MAX_REASONS).map(|(_, text)| text).collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct Lookalikes {
    pub profile: Profile,
    pub results: Vec<Match>,
    /// The company database has no companies, so nothing can match yet.
    pub database_empty: bool,
    pub settings: Settings,
}

/// The profile and the best matches for the project, from its saved seeds and toggles, or from `chosen` seeds instead.
pub async fn find(db: &PgPool, project_id: Uuid, chosen: Option<&[String]>) -> sqlx::Result<Lookalikes> {
    let s = settings(db, project_id).await?;
    let domains = seed_domains(db, project_id, &s, chosen).await?;
    let roles = hiring::roles(db, project_id).await?;
    let profile = build(seeds(db, project_id, &domains).await?, &roles, &s.off);
    let database_empty: bool = sqlx::query_scalar("select not exists (select 1 from directory_companies)").fetch_one(db).await?;
    let org_id: Option<Uuid> = sqlx::query_scalar("select org_id from projects where id = $1").bind(project_id).fetch_one(db).await?;
    let results = if database_empty || profile.seeds.is_empty() { Vec::new() } else { rank(db, project_id, org_id, &profile, LIMIT).await? };
    Ok(Lookalikes { profile, results, database_empty, settings: s })
}

/// The matches as companies of the campaign, with their score and reasons as the fit. Returns how many were new.
pub async fn add(db: &PgPool, org_id: Uuid, project_id: Uuid, segment_id: Uuid, matches: &[&Match]) -> sqlx::Result<u64> {
    let picks: Vec<crate::directory::Pick> = matches
        .iter()
        .map(|m| {
            let fit_reason = format!("Like your customers: {}", m.reasons.join(". "));
            crate::directory::Pick { domain: m.domain.clone(), fit_score: m.score, fit_reason }
        })
        .collect();
    crate::directory::add(db, org_id, project_id, segment_id, &picks).await
}

/// Autopilot's first try at finding companies for a campaign: the best matches it hasn't added yet that score at least
/// AUTOPILOT_SCORE and that the project's targeting allows. Returns how many it added, 0 without seeds or matches.
pub async fn autopilot_add(db: &PgPool, org_id: Uuid, project_id: Uuid, segment_id: Uuid, count: i64) -> anyhow::Result<u64> {
    let found = find(db, project_id, None).await?;
    let targeting = crate::agent::project_targeting(db, project_id).await?;
    let picks: Vec<&Match> = found
        .results
        .iter()
        .filter(|m| m.score >= AUTOPILOT_SCORE)
        .filter(|m| targeting.allows(&m.country_code, &format!("{} {}", m.name, m.category.as_deref().unwrap_or_default())))
        .take(count.max(0) as usize)
        .collect();
    if picks.is_empty() {
        return Ok(0);
    }
    Ok(add(db, org_id, project_id, segment_id, &picks).await?)
}

/// Below this a match shares too little with the customers for autopilot to add it unasked.
pub const AUTOPILOT_SCORE: i16 = 50;

#[cfg(test)]
pub(crate) mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;

    fn seed(domain: &str, facts: Facts) -> Seed {
        Seed { domain: domain.into(), name: domain.into(), won: true, in_database: true, known: true, facts }
    }

    fn values(p: &Profile, key: &str) -> Vec<(String, usize)> {
        p.parts.iter().find(|x| x.key == key).unwrap().values.iter().map(|v| (v.value.clone(), v.seeds)).collect()
    }

    fn known(p: &Profile, key: &str) -> usize {
        p.parts.iter().find(|x| x.key == key).unwrap().known
    }

    #[test]
    fn the_profile_keeps_what_a_third_of_the_seeds_with_data_share() {
        let tech = |t: &[&str]| Some(t.iter().map(|s| s.to_string()).collect());
        let cats = |c: &[&str]| c.iter().map(|s| s.to_string()).collect();
        let seeds = vec![
            seed("a.example", Facts { techs: tech(&["HubSpot", "Intercom"]), categories: cats(&["services", "accounting_firm"]),
                                      description: "Payroll and bookkeeping for dentists".into(), country: "GB".into(), ..Facts::default() }),
            seed("b.example", Facts { techs: tech(&["HubSpot", "Stripe"]), categories: cats(&["services", "accounting_firm"]),
                                      description: "Bookkeeping, payroll and tax".into(), country: "GB".into(), ..Facts::default() }),
            seed("c.example", Facts { techs: tech(&["HubSpot"]), categories: cats(&["services", "tax_advisor"]), country: "DE".into(), ..Facts::default() }),
            seed("d.example", Facts { techs: tech(&["Shopify"]), country: "GB".into(), ..Facts::default() }),
            // Nothing known: it counts toward no part's share.
            seed("e.example", Facts::default()),
        ];
        let p = build(seeds, &["Head of Finance".to_string()], &[]);

        // 4 seeds were scanned, so a tool needs 2 of them. Stripe and Shopify have 1 each.
        assert_eq!(known(&p, "tech"), 4);
        assert_eq!(values(&p, "tech"), [("HubSpot".to_string(), 3)]);
        // 3 have a category: each of the two is at least a third. "services" is the root, left out.
        assert_eq!(values(&p, "category"), [("accounting_firm".to_string(), 2), ("tax_advisor".to_string(), 1)]);
        assert_eq!(p.parts[0].values[0].label, "Accounting firm");
        // Words need 2 seeds once 2 have descriptions, so "dentists" and "tax" are out.
        assert_eq!(known(&p, "keywords"), 2);
        assert_eq!(values(&p, "keywords"), [("payroll".to_string(), 2), ("bookkeeping".to_string(), 2)]);
        assert_eq!(values(&p, "country"), [("GB".to_string(), 3)]);
        // No seed has a region or a job board Splee reads, so those parts have nothing in common and can't be on.
        for key in ["region", "hiring"] {
            let part = p.parts.iter().find(|x| x.key == key).unwrap();
            assert!(part.values.is_empty() && !part.on && part.known == 0, "{key}");
        }
        assert!(p.parts.iter().filter(|x| x.known > 0).all(|x| x.on));

        // A part switched off stays in the profile, off.
        let off = build(p.seeds.clone(), &[], &["tech".to_string()]);
        let tech = off.parts.iter().find(|x| x.key == "tech").unwrap();
        assert!(!tech.on && !tech.values.is_empty());

        // One description alone gives no words, so its words don't become the whole profile.
        let one = build(vec![seed("a.example", Facts { description: "Payroll and bookkeeping".into(), ..Facts::default() })], &[], &[]);
        assert_eq!((known(&one, "keywords"), values(&one, "keywords")), (1, vec![]));
    }

    #[test]
    fn hiring_keeps_the_roles_a_third_of_the_seeds_with_job_boards_posted_for() {
        let posted = |roles: &[&str]| Facts { hiring: Some(roles.iter().map(|s| s.to_string()).collect()), ..Facts::default() };
        let roles = ["Head of Finance".to_string(), "Account Executive".to_string()];
        let seeds = vec![
            seed("a.example", posted(&["Head of Finance"])),
            seed("b.example", posted(&["head of finance", "Office Manager"])),
            seed("c.example", posted(&[])),
            // Its job board was never read, so it isn't counted.
            seed("d.example", Facts::default()),
        ];
        // 2 of the 3 read posted for Head of Finance. No seed posted for Account Executive, and Office Manager isn't a role
        // the project sells to.
        let p = build(seeds.clone(), &roles, &[]);
        assert_eq!((known(&p, "hiring"), values(&p, "hiring")), (3, vec![("Head of Finance".to_string(), 2)]));

        // 1 of 4 isn't a third, so nothing is shared and hiring is off.
        let mut few = seeds;
        few[1] = seed("b.example", posted(&[]));
        few[3] = seed("d.example", posted(&["Office Manager"]));
        let p = build(few, &roles, &[]);
        let hiring = p.parts.iter().find(|x| x.key == "hiring").unwrap();
        assert!(hiring.values.is_empty() && !hiring.on && hiring.known == 4);
    }

    #[test]
    fn a_parent_category_gives_way_to_the_specific_ones() {
        let cats = |c: &[&str]| Facts { categories: c.iter().map(|s| s.to_string()).collect(), ..Facts::default() };
        let seeds = vec![
            seed("a.example", cats(&["health", "dental_care", "dentist"])),
            seed("b.example", cats(&["health", "dental_care", "dentist"])),
            seed("c.example", cats(&["health", "dental_care", "orthodontist"])),
        ];
        let p = build(seeds, &[], &[]);
        assert_eq!(values(&p, "category"), [("dentist".to_string(), 2), ("orthodontist".to_string(), 1)]);
    }

    #[test]
    fn reasons_say_how_many_customers_share_it() {
        assert_eq!(like(1, 1), "like your customer");
        assert_eq!(like(2, 2), "like both of your customers");
        assert_eq!(like(4, 4), "like all 4 of your customers");
        assert_eq!(like(3, 4), "like 3 of your 4 customers");
    }

    pub(crate) async fn org_and_project(db: &PgPool, name: &str) -> (Uuid, Uuid) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $1::text) returning id")
            .bind(org)
            .fetch_one(db)
            .await
            .unwrap();
        (org, project)
    }

    pub(crate) async fn directory(db: &PgPool, domain: &str, name: &str, category: &str, country: &str, region: Option<&str>) {
        sqlx::query(
            "insert into directory_companies (domain, name, website, email, region, country_code, category, categories, confidence, source_id, release)
             values ($1, $2, 'https://' || $1, 'hi@' || $1, $5, $4, $3, array['services', $3], 0.9, 'x', 'r')",
        )
        .bind(domain)
        .bind(name)
        .bind(category)
        .bind(country)
        .bind(region)
        .execute(db)
        .await
        .unwrap();
    }

    async fn scan(db: &PgPool, domain: &str, techs: &[&str]) {
        sqlx::query("insert into tech_scans (domain, techs, scanned_at) values ($1, $2, now())").bind(domain).bind(techs).execute(db).await.unwrap();
    }

    async fn deal(db: &PgPool, org: Uuid, project: Uuid, domain: &str, status: &str) {
        let stage: Uuid = sqlx::query_scalar(
            "insert into deal_stages (org_id, name, kind, position) values ($1, $2, case $2 when 'won' then 'won' when 'lost' then 'lost' else 'open' end, 0)
             returning id",
        )
        .bind(org)
        .bind(status)
        .fetch_one(db)
        .await
        .unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, email, company_domain) values ($1, 'ceo@' || $2, $2) returning id")
            .bind(project)
            .bind(domain)
            .fetch_one(db)
            .await
            .unwrap();
        sqlx::query("insert into deals (org_id, project_id, lead_id, stage_id, status, closed_at) values ($1, $2, $3, $4, $5, now())")
            .bind(org)
            .bind(project)
            .bind(lead)
            .bind(stage)
            .bind(status)
            .execute(db)
            .await
            .unwrap();
    }

    /// A project that won deals with three accounting firms and added a fourth customer by hand, a company database of
    /// matches and near misses, and a second organization whose data mustn't change the first one's matches.
    pub(crate) async fn customers(db: &PgPool, org: Uuid, project: Uuid) {
        for (domain, category, region) in [
            ("alpha.example", "accounting_firm", "West Yorkshire"),
            ("beta.example", "accounting_firm", "West Yorkshire"),
            ("gamma.example", "tax_advisor", "Greater London"),
        ] {
            directory(db, domain, domain, category, "GB", Some(region)).await;
            deal(db, org, project, domain, "won").await;
            // The hiring worker read each one's job board. Only Alpha posted for a role the project sells to.
            sqlx::query("insert into job_boards (domain, provider, slug) values ($1, 'lever', $1)").bind(domain).execute(db).await.unwrap();
            sqlx::query("insert into hiring_checks (project_id, domain) values ($1, $2)").bind(project).bind(domain).execute(db).await.unwrap();
        }
        sqlx::query(
            "insert into job_openings (project_id, domain, provider, title, role, url)
             values ($1, 'alpha.example', 'lever', 'Head of Finance', 'Head of Finance', 'https://jobs.example/alpha')",
        )
        .bind(project)
        .execute(db)
        .await
        .unwrap();
        deal(db, org, project, "open.example", "open").await;
        deal(db, org, project, "lost.example", "lost").await;
        scan(db, "alpha.example", &["HubSpot", "Intercom"]).await;
        scan(db, "beta.example", &["HubSpot", "Intercom", "Stripe"]).await;
        scan(db, "gamma.example", &["HubSpot"]).await;
        // Not in the company database, but the project knows it: a company row and a tech scan.
        scan(db, "delta.example", &["Shopify"]).await;
        sqlx::query(
            "insert into companies (project_id, domain, name, description, country_code) values
               ($1, 'alpha.example', 'Alpha', 'Bookkeeping and payroll for dental practices', 'GB'),
               ($1, 'beta.example', 'Beta', 'Payroll and bookkeeping for small firms', 'GB'),
               ($1, 'delta.example', 'Delta Books', 'Payroll software for accountants', 'GB')",
        )
        .bind(project)
        .execute(db)
        .await
        .unwrap();
        save_settings(db, project, &Settings { added: vec!["delta.example".into()], ..Settings::default() }).await.unwrap();
        sqlx::query(
            "insert into project_settings (project_id, targeting) values ($1, '{\"hiring_roles\": [\"Head of Finance\", \"Account Executive\"]}')
             on conflict (project_id) do update set targeting = excluded.targeting",
        )
            .bind(project)
            .execute(db)
            .await
            .unwrap();

        directory(db, "ledger.example", "Ledger Payroll", "accounting_firm", "GB", Some("West Yorkshire")).await;
        scan(db, "ledger.example", &["HubSpot", "Intercom"]).await;
        directory(db, "tally.example", "Tally", "tax_advisor", "GB", Some("Greater London")).await;
        directory(db, "berlin.example", "Berlin Steuer", "accounting_firm", "DE", None).await;
        directory(db, "bakery.example", "Bakery", "bakery", "GB", Some("West Yorkshire")).await;
        scan(db, "bakery.example", &["HubSpot"]).await;
        directory(db, "noise.example", "Noise", "restaurant", "FR", None).await;
        for domain in ["known.example", "blocked.example", "mailblocked.example", "rival.example"] {
            directory(db, domain, domain, "accounting_firm", "GB", None).await;
        }
        sqlx::query("insert into companies (project_id, domain) values ($1, 'Known.example')").bind(project).execute(db).await.unwrap();
        sqlx::query("insert into competitors (project_id, domain) values ($1, 'rival.example')").bind(project).execute(db).await.unwrap();
        for value in ["blocked.example", "hi@mailblocked.example"] {
            let kind = if value.contains('@') { "email" } else { "domain" };
            sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, $2, $3, 'manual')")
                .bind(org)
                .bind(value)
                .bind(kind)
                .execute(db)
                .await
                .unwrap();
        }

        // Another organization suppressed Tally and has Berlin as a company. Its job board checks found Ledger hiring for a
        // role the customers posted for, and Berlin for one they didn't.
        let (other_org, other_project) = org_and_project(db, "Other").await;
        sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, 'tally.example', 'domain', 'manual')")
            .bind(other_org)
            .execute(db)
            .await
            .unwrap();
        sqlx::query("insert into companies (project_id, domain) values ($1, 'berlin.example')").bind(other_project).execute(db).await.unwrap();
        sqlx::query(
            "insert into job_openings (project_id, domain, provider, title, role, url)
             values ($1, 'ledger.example', 'lever', 'Head of Finance (UK)', 'Finance lead', 'https://jobs.example/1'),
                    ($1, 'berlin.example', 'lever', 'Account Executive, DACH', 'Account Executive', 'https://jobs.example/2')",
        )
        .bind(other_project)
        .execute(db)
        .await
        .unwrap();
    }

    fn ranked(found: &Lookalikes) -> Vec<(&str, i16)> {
        found.results.iter().map(|m| (m.domain.as_str(), m.score)).collect()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn matches_rank_by_what_they_share_with_the_customers(db: PgPool) {
        let (org, project) = org_and_project(&db, "Acme").await;
        customers(&db, org, project).await;
        let found = find(&db, project, None).await.unwrap();

        let seeds: Vec<(&str, bool, bool, bool)> = found.profile.seeds.iter().map(|s| (s.domain.as_str(), s.won, s.in_database, s.known)).collect();
        assert_eq!(seeds, [
            ("alpha.example", true, true, true),
            ("beta.example", true, true, true),
            ("gamma.example", true, true, true),
            ("delta.example", false, false, true),
        ]);
        let p = &found.profile;
        assert_eq!(values(p, "category"), [("accounting_firm".to_string(), 2), ("tax_advisor".to_string(), 1)]);
        assert_eq!(values(p, "tech"), [("HubSpot".to_string(), 3), ("Intercom".to_string(), 2)]);
        assert_eq!(values(p, "keywords"), [("payroll".to_string(), 3), ("bookkeeping".to_string(), 2)]);
        assert_eq!((known(p, "hiring"), values(p, "hiring")), (3, vec![("Head of Finance".to_string(), 1)]));
        assert_eq!(values(p, "region"), [("GB|west yorkshire".to_string(), 2), ("GB|greater london".to_string(), 1)]);
        assert_eq!(p.parts.iter().find(|x| x.key == "region").unwrap().values[0].label, "West Yorkshire, GB");

        // Ledger shares the most. Seeds, the project's companies, its competitor and suppressed domains are left out, but
        // another organization's suppressions and companies change nothing. Noise shares nothing, and Berlin's post isn't
        // for a role the customers hire for. Ledger: industry 30, both tools 25, half the words 7.5, hiring 15, country 10 and
        // region 5, of 100.
        assert_eq!(ranked(&found), [("ledger.example", 93), ("tally.example", 45), ("berlin.example", 30), ("bakery.example", 28)]);
        assert_eq!(found.results[0].reasons, [
            "Accounting firm, like 2 of your 4 customers",
            "Uses HubSpot and Intercom, like 2 of your 4 customers",
            "Hiring a Head of Finance",
            "In West Yorkshire, GB, like 2 of your 4 customers",
        ]);
        assert_eq!(found.results[1].reasons, ["Tax advisor, like 1 of your 4 customers", "In Greater London, GB, like 1 of your 4 customers"]);
        assert_eq!(found.results[3].reasons, ["In West Yorkshire, GB, like 2 of your 4 customers", "Uses HubSpot, like 3 of your 4 customers"]);
        assert!(!found.database_empty);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn switching_parts_off_or_removing_a_seed_changes_the_matches(db: PgPool) {
        let (org, project) = org_and_project(&db, "Acme").await;
        customers(&db, org, project).await;
        let added = vec!["delta.example".to_string()];

        // Without the industry, Berlin shares nothing, Tally only its place, and the rest score out of 70.
        save_settings(&db, project, &Settings { added: added.clone(), off: vec!["category".into()], ..Settings::default() }).await.unwrap();
        let found = find(&db, project, None).await.unwrap();
        assert!(!found.profile.parts.iter().find(|x| x.key == "category").unwrap().on);
        assert_eq!(ranked(&found), [("ledger.example", 89), ("bakery.example", 39), ("tally.example", 21)]);

        // Without Gamma, tax advisors and Greater London aren't in the profile. Gamma itself, which uses HubSpot, is a match,
        // and Tally is one only for its country.
        save_settings(&db, project, &Settings { added, removed: vec!["gamma.example".into()], ..Settings::default() }).await.unwrap();
        let found = find(&db, project, None).await.unwrap();
        assert_eq!(found.profile.seeds.iter().map(|s| s.domain.as_str()).collect::<Vec<_>>(), ["alpha.example", "beta.example", "delta.example"]);
        assert_eq!(values(&found.profile, "category"), [("accounting_firm".to_string(), 2)]);
        assert!(found.results.iter().any(|m| m.domain == "gamma.example"));
        let tally = found.results.iter().find(|m| m.domain == "tally.example").unwrap();
        assert_eq!(tally.reasons, ["Based in GB, like all 3 of your customers"]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_profile_of_only_a_region_finds_the_companies_there(db: PgPool) {
        let (org, project) = org_and_project(&db, "Acme").await;
        customers(&db, org, project).await;
        let off = ["category", "tech", "keywords", "hiring", "country"].map(String::from).to_vec();
        save_settings(&db, project, &Settings { added: vec!["delta.example".into()], off, ..Settings::default() }).await.unwrap();
        let found = find(&db, project, None).await.unwrap();
        assert_eq!(found.profile.parts.iter().filter(|x| x.on).map(|x| x.key).collect::<Vec<_>>(), ["region"]);
        // Each scores all of the region's points. Ties go by confidence, then domain.
        assert_eq!(ranked(&found), [("bakery.example", 100), ("ledger.example", 100), ("tally.example", 100)]);
        assert_eq!(found.results[0].reasons, ["In West Yorkshire, GB, like 2 of your 4 customers"]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn sharing_the_place_outranks_a_weak_match(db: PgPool) {
        let (org, project) = org_and_project(&db, "Acme").await;
        customers(&db, org, project).await;
        // The industry scores all or nothing, so a weak match is one of the two tools or one of the two words.
        directory(&db, "local.example", "Local Plumbing", "plumber", "GB", Some("West Yorkshire")).await;
        directory(&db, "intercom-user.example", "Café Lumière", "cafe", "FR", None).await;
        scan(&db, "intercom-user.example", &["Intercom"]).await;
        directory(&db, "paris.example", "Paris Payroll", "cafe", "FR", None).await;
        let found = find(&db, project, None).await.unwrap();
        // Local: country 10 and region 5. Café: half the tools, 12.5. Paris: half the words, 7.5.
        assert_eq!(ranked(&found)[4..], [("local.example", 15), ("intercom-user.example", 13), ("paris.example", 8)]);
        assert_eq!(found.results[4].reasons, ["In West Yorkshire, GB, like 2 of your 4 customers"]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn won_deals_are_the_seeds_unless_others_are_given(db: PgPool) {
        let (org, project) = org_and_project(&db, "Acme").await;
        directory(&db, "solo.example", "Solo", "accounting_firm", "GB", None).await;
        // No won deals and no seeds: an empty profile and no matches.
        let found = find(&db, project, None).await.unwrap();
        assert!(found.profile.seeds.is_empty() && found.results.is_empty() && !found.database_empty);
        assert!(!has_seeds(&db, project).await.unwrap());

        customers(&db, org, project).await;
        let domains = |f: &Lookalikes| f.profile.seeds.iter().map(|s| (s.domain.clone(), s.won, s.known)).collect::<Vec<_>>();
        save_settings(&db, project, &Settings::default()).await.unwrap();
        let won = find(&db, project, None).await.unwrap();
        assert_eq!(domains(&won), [
            ("alpha.example".to_string(), true, true),
            ("beta.example".to_string(), true, true),
            ("gamma.example".to_string(), true, true),
        ]);
        assert!(has_seeds(&db, project).await.unwrap());

        // Given seeds replace the saved ones. One Splee knows nothing about still counts as a customer.
        let chosen = ["beta.example".to_string(), "new.example".to_string()];
        let found = find(&db, project, Some(&chosen)).await.unwrap();
        assert_eq!(domains(&found), [("beta.example".to_string(), true, true), ("new.example".to_string(), false, false)]);
        assert_eq!(found.results[0].reasons[0], "Accounting firm, like 1 of your 2 customers");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn added_matches_join_the_campaign_with_their_reasons(db: PgPool) {
        let (org, project) = org_and_project(&db, "Acme").await;
        customers(&db, org, project).await;
        let segment: Uuid =
            sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Firms') returning id").bind(project).fetch_one(&db).await.unwrap();
        let found = find(&db, project, None).await.unwrap();
        let picked: Vec<&Match> = found.results.iter().take(2).collect();
        assert_eq!(add(&db, org, project, segment, &picked).await.unwrap(), 2);
        assert_eq!(add(&db, org, project, segment, &picked).await.unwrap(), 0, "already in the project");

        let (segment_id, fit_score, fit_reason, status): (Option<Uuid>, i32, String, String) =
            sqlx::query_as("select segment_id, fit_score, fit_reason, status from companies where domain = 'ledger.example'").fetch_one(&db).await.unwrap();
        assert_eq!((segment_id, fit_score, status.as_str()), (Some(segment), 93, "new"));
        assert!(fit_reason.starts_with("Like your customers: Accounting firm, like 2 of your 4 customers. Uses HubSpot"), "{fit_reason}");
        // Now they're the project's, so they're no longer matches.
        assert_eq!(ranked(&find(&db, project, None).await.unwrap()), [("berlin.example", 30), ("bakery.example", 28)]);
    }

    /// Every way the ranking picks companies out goes through an index, which Postgres ORs, so it never has to read every
    /// company. On a table this small Postgres rightly reads it whole instead, so sequential scans are turned off for the
    /// EXPLAIN. On 500,000 rows it picks the bitmap scans by itself.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_ranking_query_picks_companies_through_the_indexes(db: PgPool) {
        sqlx::query(
            "insert into directory_companies (domain, name, website, country_code, category, categories, confidence, source_id, release)
             select 'c' || i || '.example', case when i % 500 = 0 then 'Payroll ' else 'Cafe ' end || i, 'https://c' || i || '.example',
                    case when i % 3 = 0 then 'GB' else 'FR' end, c, array['services', c], 0.5, 'x', 'r'
             from generate_series(1, 5000) i, lateral (select case when i % 400 = 0 then 'accounting_firm' else 'cafe' end as c) cat",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("analyze directory_companies").execute(&db).await.unwrap();
        let (org, project) = org_and_project(&db, "Acme").await;
        for domain in ["c400.example", "c800.example"] {
            deal(&db, org, project, domain, "won").await;
            sqlx::query("insert into companies (project_id, domain, description) values ($1, $2, 'Payroll for firms')")
                .bind(project)
                .bind(domain)
                .execute(&db)
                .await
                .unwrap();
        }
        let s = settings(&db, project).await.unwrap();
        let profile = build(seeds(&db, project, &seed_domains(&db, project, &s, None).await.unwrap()).await.unwrap(), &[], &[]);
        assert_eq!(values(&profile, "category"), [("accounting_firm".to_string(), 2)]);
        assert_eq!(values(&profile, "keywords"), [("payroll".to_string(), 2), ("firms".to_string(), 2)]);

        let r = ranking(&db, project, Some(org), &profile, LIMIT).await.unwrap().unwrap();
        let mut tx = db.begin().await.unwrap();
        sqlx::query("set local enable_seqscan = off").execute(&mut *tx).await.unwrap();
        let plan: Vec<String> = bind!(sqlx::query_scalar(&format!("explain {}", r.sql)), r).fetch_all(&mut *tx).await.unwrap();
        tx.rollback().await.unwrap();
        let plan = plan.join("\n");
        assert!(plan.contains("Bitmap Index Scan on directory_companies_categories"), "{plan}");
        assert!(plan.contains("Bitmap Index Scan on directory_companies_search"), "{plan}");
        assert!(plan.contains("BitmapOr"), "{plan}");
        // Both seeds are in FR, so its best companies come in through the country's index too.
        assert!(plan.contains("Index Scan using directory_companies_country_rank"), "{plan}");
        assert!(!plan.contains("Seq Scan on directory_companies"), "{plan}");
        let found = rank(&db, project, Some(org), &profile, LIMIT).await.unwrap();
        assert_eq!(found.len(), LIMIT as usize, "the other accounting firms, the payroll names and the best of FR");
    }
}
