//! Steps 4 and 5 (companies and decision makers) plus the public Segments Explorer.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::explore::{self, EMPLOYEE_RANGES, Profile, RESEARCHER, is_directory, load_project};
use super::directory;
use super::sdk::Call;
use super::x;
use super::{Ctx, Outcome};
use crate::mail;
use crate::models::{Company, Project, Segment};
use crate::routes::util_input;

/// Project-wide ICP limits from `project_settings.targeting`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProjectTargeting {
    countries: Vec<String>,
    exclude_countries: Vec<String>,
    company_sizes: Vec<String>,
    exclude_keywords: Vec<String>,
}

impl ProjectTargeting {
    pub fn describe(&self) -> String {
        let mut rules = vec![];
        if !self.countries.is_empty() {
            rules.push(format!("Only companies in these countries: {}.", self.countries.join(", ")));
        }
        if !self.exclude_countries.is_empty() {
            rules.push(format!("Never companies in: {}.", self.exclude_countries.join(", ")));
        }
        if !self.company_sizes.is_empty() {
            rules.push(format!("Only companies with these headcounts: {}.", self.company_sizes.join(", ")));
        }
        if !self.exclude_keywords.is_empty() {
            rules.push(format!("Skip companies matching: {}.", self.exclude_keywords.join(", ")));
        }
        if rules.is_empty() { String::new() } else { format!("Account-wide rules: {}", rules.join(" ")) }
    }

    fn allows(&self, country_code: &str, text: &str) -> bool {
        let cc = country_code.to_uppercase();
        let in_list = |list: &[String]| list.iter().any(|c| c.eq_ignore_ascii_case(&cc));
        if !cc.is_empty() && (in_list(&self.exclude_countries) || (!self.countries.is_empty() && !in_list(&self.countries))) {
            return false;
        }
        let text = text.to_lowercase();
        !self.exclude_keywords.iter().any(|k| !k.trim().is_empty() && text.contains(&k.trim().to_lowercase()))
    }
}

pub async fn project_targeting(ctx: &Ctx, project_id: Uuid) -> anyhow::Result<ProjectTargeting> {
    let value: Option<Value> = sqlx::query_scalar("select targeting from project_settings where project_id = $1")
        .bind(project_id)
        .fetch_optional(ctx.db())
        .await?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default())
}

/// Suppressed emails and domains of the project's organization. Demo projects have none.
async fn suppressions(ctx: &Ctx, project: &Project) -> anyhow::Result<HashSet<String>> {
    let Some(org_id) = project.org_id else { return Ok(HashSet::new()) };
    let values: Vec<String> = sqlx::query_scalar("select lower(value::text) from suppressions where org_id = $1")
        .bind(org_id)
        .fetch_all(ctx.db())
        .await?;
    Ok(values.into_iter().collect())
}

fn segment_brief(segment: &Segment) -> String {
    let t = &segment.targeting;
    let join = |key: &str| {
        t[key].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default()
    };
    format!(
        "Segment \"{}\": {}\nIndustries: {}. Sizes: {}. Countries: {}. Search keywords: {}.\nTheir pain: {}",
        segment.name,
        segment.description,
        join("industries"),
        join("company_sizes"),
        join("countries"),
        join("keywords"),
        segment.pain_points,
    )
}

#[derive(Deserialize)]
struct CompanyList {
    companies: Vec<FoundCompany>,
}

#[derive(Deserialize)]
#[serde(default)]
#[derive(Default)]
struct FoundCompany {
    domain: String,
    name: String,
    description: String,
    city: String,
    country_code: String,
    employees: String,
    fit_score: i32,
    fit_reason: String,
    source_url: String,
}

fn company_schema(extra: Value) -> Value {
    let mut properties = json!({
        "domain": {"type": "string", "description": "the company's own website domain, no www"},
        "name": {"type": "string"},
        "description": {"type": "string", "description": "one sentence on what they do"},
        "city": {"type": "string"},
        "country_code": {"type": "string", "description": "ISO 3166-1 alpha-2"},
        "employees": {"type": "string", "enum": EMPLOYEE_RANGES}
    });
    let mut required = vec!["domain", "name", "description", "city", "country_code", "employees"];
    for (key, schema) in extra.as_object().into_iter().flatten() {
        properties[key] = schema.clone();
        required.push(key);
    }
    json!({
        "type": "object",
        "properties": {"companies": {"type": "array", "items": {"type": "object", "properties": properties, "required": required}}},
        "required": ["companies"]
    })
}

pub async fn find_companies_for_top_segment(ctx: &Ctx, project_id: Uuid, count: usize) -> anyhow::Result<()> {
    let segment = explore::top_segment(ctx, project_id).await?;
    let (project, profile) = load_project(ctx, project_id).await?;
    find_companies(ctx, &project, &profile, &segment, count).await?;
    Ok(())
}

async fn find_companies(ctx: &Ctx, project: &Project, profile: &Profile, segment: &Segment, count: usize) -> anyhow::Result<Vec<Company>> {
    let targeting = project_targeting(ctx, project.id).await?;
    let suppressed = suppressions(ctx, project).await?;
    let mut exclude: HashSet<String> = sqlx::query_scalar::<_, String>(
        "select domain::text from companies where project_id = $1 union select domain::text from competitors where project_id = $1",
    )
    .bind(project.id)
    .fetch_all(ctx.db())
    .await?
    .into_iter()
    .collect();
    exclude.extend(project.domain.clone());

    ctx.log(format!("building search queries from {}…", segment.name)).await?;
    let mut skip_list: Vec<&String> = exclude.iter().collect();
    skip_list.sort();
    skip_list.truncate(80);
    let prompt = format!(
        "We sell:\n{}\n\nFind {want} real businesses that fit this buyer segment:\n{}\n{}\n\n\
         Search the web the way a researcher builds a prospect list: business directories, \"best ... in <city>\" lists, association \
         member lists, maps-style searches like \"<keyword> near <city>\". Spread across several cities or regions. Open a list page when it \
         gives many matches at once. Only include businesses that have their own website. Don't include these domains: {}.\n\n\
         For each business: domain, name, description, city, country_code, employees (best estimate), fit_score (0-100, how well it \
         matches the segment and how likely it buys), fit_reason (one sentence specific to this business, e.g. what they do that makes our \
         offer useful), source_url (the page where you found it).",
        profile.brief(project.domain.as_deref()),
        segment_brief(segment),
        targeting.describe(),
        if skip_list.is_empty() { "none".to_string() } else { skip_list.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ") },
        want = count + 3,
    );
    let schema = company_schema(json!({
        "fit_score": {"type": "integer", "minimum": 0, "maximum": 100},
        "fit_reason": {"type": "string"},
        "source_url": {"type": "string"}
    }));
    let turns = if count > 12 { 20 } else { 14 };
    let mut call = Call::research(RESEARCHER, prompt, schema, turns);
    if directory::has_companies(ctx.db()).await? {
        call.directory = Some(project.id);
    }
    let list: CompanyList = ctx.ask(&call).await?;
    ctx.log(format!("scoring fit for {} domains…", list.companies.len())).await?;

    let mut candidates: Vec<(String, FoundCompany)> = list
        .companies
        .into_iter()
        .filter_map(|c| util_input::domain(&c.domain).map(|d| (d, c)))
        .filter(|(d, c)| {
            !exclude.contains(d)
                && !suppressed.contains(d)
                && !is_directory(d)
                && targeting.allows(&c.country_code, &format!("{} {}", c.name, c.description))
        })
        .collect();
    candidates.sort_by_key(|(_, c)| std::cmp::Reverse(c.fit_score));

    let mut saved = vec![];
    for (domain, c) in candidates {
        if saved.len() >= count {
            break;
        }
        let row = sqlx::query_as::<_, Company>(
            "insert into companies (project_id, segment_id, domain, name, description, city, country_code, employees, fit_score, fit_reason, source_url)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             on conflict (project_id, domain) do nothing returning *",
        )
        .bind(project.id)
        .bind(segment.id)
        .bind(&domain)
        .bind(c.name.trim())
        .bind(c.description.trim())
        .bind(c.city.trim())
        .bind(c.country_code.trim().to_uppercase())
        .bind(c.employees.trim())
        .bind(c.fit_score.clamp(0, 100))
        .bind(c.fit_reason.trim())
        .bind(c.source_url.trim())
        .fetch_optional(ctx.db())
        .await?;
        saved.extend(row);
    }
    anyhow::ensure!(!saved.is_empty(), "no new companies matched this campaign");
    ctx.log(format!("ranking the outreach list of {}…", saved.len())).await?;
    Ok(saved)
}

#[derive(Deserialize)]
struct FindCompaniesInput {
    segment_id: Uuid,
    count: Option<usize>,
}

pub async fn find_companies_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: FindCompaniesInput = ctx.input()?;
    let segment = explore::load_segment(ctx, input.segment_id).await?;
    let (project, profile) = load_project(ctx, segment.project_id).await?;
    let saved = find_companies(ctx, &project, &profile, &segment, input.count.unwrap_or(25).clamp(1, 50)).await?;
    Ok(Outcome::new(format!("Found {} companies", saved.len())))
}

#[derive(Deserialize)]
struct PeopleList {
    people: Vec<FoundPerson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FoundPerson {
    company_domain: String,
    full_name: String,
    first_name: String,
    last_name: String,
    title: String,
    email: String,
    email_source: String,
    linkedin_url: String,
    x_url: String,
    city: String,
    country_code: String,
    hook: String,
    facts: Vec<String>,
}

pub async fn find_people_for_top_segment(ctx: &Ctx, project_id: Uuid, cap: usize) -> anyhow::Result<()> {
    let segment = explore::top_segment(ctx, project_id).await?;
    let (project, profile) = load_project(ctx, project_id).await?;
    let companies = best_companies(ctx, &segment, cap).await?;
    let saved = find_people(ctx, &project, &profile, &segment, companies, 2, cap).await?;
    anyhow::ensure!(saved > 0, "no reachable decision makers found at these companies");
    Ok(())
}

/// Companies of the segment that haven't been searched for people yet, best fit first. Small shops often
/// have one findable person, so take one company per wanted lead. The search stops early once `cap` is reached.
async fn best_companies(ctx: &Ctx, segment: &Segment, cap: usize) -> anyhow::Result<Vec<Company>> {
    let limit = cap.max(3) as i64;
    Ok(sqlx::query_as::<_, Company>(
        "select * from companies where segment_id = $1 and status = 'new' order by fit_score desc, created_at limit $2",
    )
    .bind(segment.id)
    .bind(limit)
    .fetch_all(ctx.db())
    .await?)
}

/// Researches people at `companies` in batches and saves the reachable ones. Returns how many leads were saved.
async fn find_people(
    ctx: &Ctx,
    project: &Project,
    profile: &Profile,
    segment: &Segment,
    companies: Vec<Company>,
    max_per_company: usize,
    cap: usize,
) -> anyhow::Result<usize> {
    let suppressed = suppressions(ctx, project).await?;
    let resolver = mail::dns::resolver()?;
    let mut mx_cache: HashMap<String, bool> = HashMap::new();
    let roles = segment.targeting["roles"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "Owner, Founder, Managing Director".into());
    let schema = json!({
        "type": "object",
        "properties": {"people": {"type": "array", "items": {
            "type": "object",
            "properties": {
                "company_domain": {"type": "string", "description": "exactly as given in the company list"},
                "full_name": {"type": "string"},
                "first_name": {"type": "string"},
                "last_name": {"type": "string"},
                "title": {"type": "string"},
                "email": {"type": "string"},
                "email_source": {"type": "string", "enum": ["published", "pattern"]},
                "linkedin_url": {"type": "string"},
                "x_url": {"type": "string", "description": "their X (Twitter) profile, e.g. https://x.com/handle"},
                "city": {"type": "string"},
                "country_code": {"type": "string"},
                "hook": {"type": "string", "description": "one specific, verifiable fact about their business to open an email with"},
                "facts": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["company_domain", "full_name", "first_name", "last_name", "title", "email", "email_source", "linkedin_url", "city", "country_code", "hook", "facts"]
        }}},
        "required": ["people"]
    });

    let mut saved = 0;
    for batch in companies.chunks(3) {
        if saved >= cap {
            break;
        }
        let list = batch
            .iter()
            .map(|c| format!("- {} ({}, {}): {}", c.domain, c.name, if c.city.is_empty() { &c.country_code } else { &c.city }, c.description))
            .collect::<Vec<_>>()
            .join("\n");
        ctx.log(format!("looking for decision makers at {}…", batch.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", "))).await?;
        let prompt = format!(
            "We sell:\n{}\n\nFind the people to email at these prospects (segment \"{}\"):\n{list}\n\n\
             For each company find up to {max_per_company} decision makers for buying our offer. Preferred roles: {roles}. At small businesses \
             the owner or founder is usually right. Check the company's team, about and contact pages, and search for \
             \"<company name> owner\" or \"site:linkedin.com/in <company name>\". Use 2 to 3 tool calls per company.\n\n\
             For each person: their name, their job title only (e.g. \"Owner\", no notes or sources), linkedin_url and x_url (their X profile) if you saw them (else \"\"), city and country_code, a hook \
             (one specific fact about their business that makes a good first line, e.g. a recent project, a specialty, an award) and \
             2 to 4 short facts you saw on the web.\n\
             Email: if you saw this person's address published, use it with email_source \"published\". Otherwise guess it on the \
             company's domain with email_source \"pattern\": copy the pattern of any published address at the company (first@, \
             first.last@, f.last@...), else use first@domain for companies under 20 people and first.last@domain for larger ones.\n\
             Only include real people you found on the web. Skip a company if you find nobody.",
            profile.brief(project.domain.as_deref()),
            segment.name,
        );
        let found: PeopleList = ctx.ask(&Call::research(RESEARCHER, prompt, schema.clone(), 14)).await?;

        let mut per_company: HashMap<Uuid, usize> = HashMap::new();
        for person in found.people {
            if saved >= cap {
                break;
            }
            let Some(company) = util_input::domain(&person.company_domain).and_then(|d| batch.iter().find(|c| c.domain == d)) else {
                continue;
            };
            if per_company.get(&company.id).copied().unwrap_or(0) >= max_per_company {
                continue;
            }
            let Some(lead) = shape_lead(person, company) else { continue };
            let email_domain = lead.email.split('@').nth(1).unwrap_or_default().to_string();
            if suppressed.contains(&lead.email) || suppressed.contains(&email_domain) {
                continue;
            }
            let has_mx = match mx_cache.get(&email_domain) {
                Some(ok) => *ok,
                None => {
                    let ok = mail::dns::has_mx(&resolver, &email_domain).await;
                    mx_cache.insert(email_domain.clone(), ok);
                    ok
                }
            };
            if !has_mx {
                ctx.log(format!("skipping {email_domain}, it has no mail server…")).await?;
                continue;
            }
            let inserted = sqlx::query(
                "insert into leads (project_id, segment_id, company_id, full_name, first_name, last_name, title, email, email_status,
                                    linkedin_url, x_url, company_name, company_domain, city, country_code, personalization)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
                 on conflict (project_id, email) do nothing",
            )
            .bind(project.id)
            .bind(segment.id)
            .bind(company.id)
            .bind(&lead.full_name)
            .bind(&lead.first_name)
            .bind(&lead.last_name)
            .bind(&lead.title)
            .bind(&lead.email)
            .bind(lead.email_status)
            .bind(&lead.linkedin_url)
            .bind(&lead.x_url)
            .bind(&company.name)
            .bind(&company.domain)
            .bind(&lead.city)
            .bind(&lead.country_code)
            .bind(&lead.personalization)
            .execute(ctx.db())
            .await?;
            if inserted.rows_affected() > 0 {
                saved += 1;
                *per_company.entry(company.id).or_default() += 1;
            }
        }
        for company in batch {
            let status = if per_company.contains_key(&company.id) { "people_found" } else { "no_people" };
            sqlx::query("update companies set status = $2 where id = $1").bind(company.id).bind(status).execute(ctx.db()).await?;
        }
    }
    ctx.log(format!("found {saved} decision makers…")).await?;
    Ok(saved)
}

struct NewLead {
    full_name: String,
    first_name: String,
    last_name: String,
    title: String,
    email: String,
    email_status: &'static str,
    linkedin_url: String,
    x_url: String,
    city: String,
    country_code: String,
    personalization: Value,
}

fn shape_lead(p: FoundPerson, company: &Company) -> Option<NewLead> {
    let full_name = p.full_name.trim().to_string();
    if full_name.is_empty() {
        return None;
    }
    let mut words = full_name.split_whitespace();
    let first_name = if p.first_name.trim().is_empty() { words.next().unwrap_or_default().to_string() } else { p.first_name.trim().to_string() };
    let last_name =
        if p.last_name.trim().is_empty() { full_name.split_whitespace().skip(1).collect::<Vec<_>>().join(" ") } else { p.last_name.trim().to_string() };

    let email = p.email.trim().trim_start_matches("mailto:").to_lowercase();
    let published = p.email_source == "published" && util_input::email(&email).is_some();
    let email = if published || (util_input::email(&email).is_some() && email.ends_with(&format!("@{}", company.domain))) {
        email
    } else {
        // The model guessed an address on some other domain, or gave none. Fall back to the most common pattern.
        let first: String = first_name.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if first.is_empty() {
            return None;
        }
        format!("{first}@{}", company.domain)
    };

    Some(NewLead {
        full_name,
        first_name,
        last_name,
        title: p.title.trim().to_string(),
        email,
        email_status: if published { "found" } else { "guessed" },
        linkedin_url: p.linkedin_url.trim().to_string(),
        x_url: x::profile_url(&p.x_url),
        city: if p.city.trim().is_empty() { company.city.clone() } else { p.city.trim().to_string() },
        country_code: if p.country_code.trim().is_empty() { company.country_code.clone() } else { p.country_code.trim().to_uppercase() },
        personalization: json!({"hook": p.hook.trim(), "facts": p.facts}),
    })
}

#[derive(Deserialize)]
struct FindPeopleInput {
    segment_id: Uuid,
    company_ids: Option<Vec<Uuid>>,
    max_per_company: Option<usize>,
}

pub async fn find_people_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: FindPeopleInput = ctx.input()?;
    let segment = explore::load_segment(ctx, input.segment_id).await?;
    let (project, profile) = load_project(ctx, segment.project_id).await?;
    let max_per = input.max_per_company.unwrap_or(2).clamp(1, 5);
    let (companies, cap) = match input.company_ids {
        Some(ids) => {
            let companies = sqlx::query_as::<_, Company>("select * from companies where project_id = $1 and id = any($2) order by fit_score desc")
                .bind(project.id)
                .bind(&ids)
                .fetch_all(ctx.db())
                .await?;
            let cap = companies.len() * max_per;
            (companies, cap)
        }
        None => (best_companies(ctx, &segment, 20).await?, 20),
    };
    anyhow::ensure!(!companies.is_empty(), "there are no new companies in this campaign to search");
    let saved = find_people(ctx, &project, &profile, &segment, companies, max_per, cap).await?;
    Ok(Outcome::new(format!("Found {saved} decision makers")))
}

#[derive(Deserialize)]
struct ExplorerInput {
    query: String,
    country: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct ExplorerCompany {
    domain: String,
    name: String,
    description: String,
    city: String,
    country_code: String,
    employees: String,
    fit_reason: String,
}

#[derive(Deserialize)]
struct ExplorerList {
    companies: Vec<ExplorerCompany>,
}

pub async fn explorer_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: ExplorerInput = ctx.input()?;
    let place = input.country.as_deref().filter(|c| !c.trim().is_empty()).map(|c| format!(" in {}", c.trim())).unwrap_or_default();
    ctx.log(format!("building search queries for \"{}\"…", input.query.trim())).await?;
    let prompt = format!(
        "Find up to 20 real companies that match this description: \"{}\"{place}.\n\
         Search business directories, \"best ... in <city>\" lists, association member lists and maps-style searches. Only include \
         companies with their own website. For each: domain, name, description, city, country_code, employees (best estimate) and \
         fit_reason (one sentence on why it matches).",
        input.query.trim()
    );
    let schema = company_schema(json!({"fit_reason": {"type": "string"}}));
    let list: ExplorerList = ctx.ask(&Call::research(RESEARCHER, prompt, schema, 12)).await?;

    let mut seen = HashSet::new();
    let results: Vec<ExplorerCompany> = list
        .companies
        .into_iter()
        .filter_map(|mut c| {
            let domain = util_input::domain(&c.domain)?;
            (!is_directory(&domain) && seen.insert(domain.clone())).then(|| {
                c.domain = domain;
                c.country_code = c.country_code.trim().to_uppercase();
                c
            })
        })
        .collect();
    ctx.log(format!("found {} companies…", results.len())).await?;
    Ok(Outcome { message: format!("Found {} companies", results.len()), output: json!({"results": results}) })
}
