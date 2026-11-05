//! The explore run: the six-step pipeline from a website to written emails. Steps 1-3 live here,
//! steps 4-6 in `prospect` and `write`.

use std::collections::HashSet;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::prospect;
use crate::routes::util_input;
use super::sdk::Call;
use super::{Ctx, Outcome, write};
use crate::models::{Project, Segment};

pub const STEP_TITLES: [&str; 6] = [
    "Research your company",
    "Explore competitors",
    "Define campaigns",
    "Find potential customers",
    "Find decision makers",
    "Write personal emails",
];

pub const SEGMENT_ICONS: [&str; 20] = [
    "building", "store", "hotel", "heart", "palette", "home", "scissors", "briefcase", "graduation-cap", "stethoscope",
    "factory", "truck", "utensils", "code", "shopping-cart", "users", "landmark", "plane", "dumbbell", "camera",
];

pub const EMPLOYEE_RANGES: [&str; 7] = ["1-10", "11-50", "51-200", "201-500", "501-1000", "1001-5000", "5000+"];

pub const RESEARCHER: &str = "You are the research agent of Splee, an outbound sales tool. You research businesses on the public web \
so a company can find and email B2B customers. Report only what you found on the web or what the user told you. Never invent \
companies, domains, people, emails or numbers. When a value is unknown, use an empty string. Be fast: a few well-chosen searches \
and page fetches beat many. Reply in English unless asked otherwise.";

/// What step 1 learns about the user's business. Stored as `projects.company`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub description: String,
    pub employees: String,
    pub city: String,
    pub country_code: String,
    pub industry: String,
    pub products: Vec<String>,
    pub value_props: Vec<String>,
    pub language: String,
}

impl Profile {
    /// A compact block for prompts of later steps.
    pub fn brief(&self, domain: Option<&str>) -> String {
        let site = domain.map(|d| format!(" ({d})")).unwrap_or_default();
        format!(
            "{}{site}: {}\nIndustry: {}. Based in {}, {}. Size: {} employees.\nProducts: {}.\nWhy customers buy: {}.",
            self.name,
            self.description,
            self.industry,
            or_unknown(&self.city),
            or_unknown(&self.country_code),
            or_unknown(&self.employees),
            self.products.join("; "),
            self.value_props.join("; "),
        )
    }
}

fn or_unknown(s: &str) -> &str {
    if s.is_empty() { "unknown" } else { s }
}

#[derive(Deserialize)]
struct ExploreInput {
    #[serde(default)]
    mode: String,
    from_step: Option<i32>,
    note: Option<String>,
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: ExploreInput = ctx.input()?;
    let demo = input.mode == "demo";
    let project_id = ctx.project_id()?;
    let from = input.from_step.unwrap_or(1).clamp(1, 6);

    if from == 1 {
        sqlx::query("update projects set status = 'researching' where id = $1 and status in ('ready', 'failed')")
            .bind(project_id)
            .execute(ctx.db())
            .await?;
    }
    // A demo rerun ("Details look off?") replaces the whole preview. Emails cascade with their leads.
    if demo && from == 1 {
        let mut tx = ctx.db().begin().await?;
        for table in ["leads", "companies", "segments"] {
            sqlx::query(&format!("delete from {table} where project_id = $1")).bind(project_id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }

    for step in from..=6 {
        let title = STEP_TITLES[step as usize - 1];
        ctx.begin_step(step, title).await?;
        let result = match step {
            1 => research_company(ctx, project_id, input.note.as_deref()).await,
            2 => competitors(ctx, project_id).await,
            3 => segments(ctx, project_id).await,
            4 => prospect::find_companies_for_top_segment(ctx, project_id, if demo { 10 } else { 25 }).await,
            5 => prospect::find_people_for_top_segment(ctx, project_id, if demo { 6 } else { 20 }).await,
            _ => write::write_for_top_segment(ctx, project_id, if demo { Some(3) } else { None }).await,
        };
        if let Err(e) = result {
            if step == 1 {
                sqlx::query("update projects set status = 'failed' where id = $1").bind(project_id).execute(ctx.db()).await?;
            }
            return Err(e.context(format!("{title} failed")));
        }
        ctx.finish_step(step, title).await?;
    }

    sqlx::query("update projects set status = 'ready' where id = $1 and status in ('researching', 'failed')")
        .bind(project_id)
        .execute(ctx.db())
        .await?;
    Ok(Outcome::new("Your outreach plan is ready"))
}

pub async fn load_project(ctx: &Ctx, project_id: Uuid) -> anyhow::Result<(Project, Profile)> {
    let project = sqlx::query_as::<_, Project>("select * from projects where id = $1")
        .bind(project_id)
        .fetch_optional(ctx.db())
        .await?
        .context("project not found")?;
    let profile: Profile = serde_json::from_value(project.company.clone()).unwrap_or_default();
    Ok((project, profile))
}

fn profile_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "description": {"type": "string"},
            "employees": {"type": "string", "enum": EMPLOYEE_RANGES},
            "city": {"type": "string"},
            "country_code": {"type": "string", "description": "ISO 3166-1 alpha-2, uppercase, or empty"},
            "industry": {"type": "string"},
            "products": {"type": "array", "items": {"type": "string"}},
            "value_props": {"type": "array", "items": {"type": "string"}},
            "language": {"type": "string", "description": "ISO 639-1 code"}
        },
        "required": ["name", "description", "employees", "city", "country_code", "industry", "products", "value_props", "language"]
    })
}

const PROFILE_FIELDS: &str = "Return:
- name: the brand name as the company writes it.
- description: one sentence of 15 to 30 words saying what they sell, to whom, and what sets them apart. Example: \
\"Wholesale supplier of premium silk flowers and floral supplies for florists, shipping from NJ with durable, long-lasting artificial blooms.\"
- employees: headcount range. Use what the site, LinkedIn or a directory says, otherwise your best estimate from the size of the business.
- city and country_code: where the company is headquartered (ISO 3166-1 alpha-2 code like US, GB, DE).
- industry: a short label like \"Wholesale florist supplies\".
- products: 3 to 6 main products or services as short phrases.
- value_props: 3 to 5 concrete reasons customers buy from them, as short phrases.
- language: ISO 639-1 code of the main language they sell in.";

async fn research_company(ctx: &Ctx, project_id: Uuid, note: Option<&str>) -> anyhow::Result<()> {
    let (project, _) = load_project(ctx, project_id).await?;
    let correction = note
        .filter(|n| !n.trim().is_empty())
        .map(|n| format!("\n\nThe owner reviewed an earlier version of this profile and corrected it: \"{}\". Their correction is true and overrides anything the website or the web says.", n.trim()))
        .unwrap_or_default();

    let call = match (&project.domain, &project.business_description) {
        (Some(domain), _) => {
            ctx.log(format!("reading {domain}…")).await?;
            Call::research(
                RESEARCHER,
                format!(
                    "Research the business behind the website {domain}.\n\n\
                     1. Fetch https://{domain} and read it.\n\
                     2. Fetch one or two pages that explain the business best, such as about, pricing, products or services (follow links you saw on the homepage).\n\
                     3. Only if the site is thin or unreachable, run one web search for \"{domain}\".\n\n{PROFILE_FIELDS}{correction}"
                ),
                profile_schema(),
                8,
            )
        }
        (None, Some(description)) => Call::writing(
            RESEARCHER,
            format!(
                "A business owner without a website describes their business:\n\"{description}\"\n\n\
                 Build the profile from this description. If it doesn't name the business, use a short descriptive name like \"Lisbon Wedding Photography\".\n\n{PROFILE_FIELDS}{correction}"
            ),
            profile_schema(),
        ),
        (None, None) => anyhow::bail!("the project has neither a website nor a description"),
    };

    let mut profile: Profile = ctx.ask(&call).await?;
    profile.country_code = profile.country_code.trim().to_uppercase();
    anyhow::ensure!(!profile.name.trim().is_empty(), "could not work out what the company is called");

    sqlx::query("update projects set name = $2, company = $3 where id = $1")
        .bind(project_id)
        .bind(profile.name.trim())
        .bind(serde_json::to_value(&profile)?)
        .execute(ctx.db())
        .await?;
    ctx.log(format!("listing {} products and value props…", profile.products.len() + profile.value_props.len())).await?;
    Ok(())
}

/// Domains that list or sell other businesses. They show up in every "alternatives to" search but aren't competitors.
const NOT_COMPETITORS: [&str; 24] = [
    "amazon.", "ebay.", "etsy.", "alibaba.", "aliexpress.", "walmart.", "yelp.", "g2.com", "capterra.", "trustpilot.",
    "linkedin.", "facebook.", "instagram.", "pinterest.", "youtube.", "wikipedia.", "reddit.", "crunchbase.", "zoominfo.",
    "tripadvisor.", "yellowpages.", "bbb.org", "google.", "medium.com",
];

pub fn is_directory(domain: &str) -> bool {
    NOT_COMPETITORS.iter().any(|d| domain.contains(d))
}

#[derive(Deserialize)]
struct CompetitorList {
    competitors: Vec<CompetitorItem>,
}

#[derive(Deserialize)]
struct CompetitorItem {
    domain: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    note: String,
}

async fn competitors(ctx: &Ctx, project_id: Uuid) -> anyhow::Result<()> {
    let (project, profile) = load_project(ctx, project_id).await?;
    let schema = json!({
        "type": "object",
        "properties": {"competitors": {"type": "array", "maxItems": 12, "items": {
            "type": "object",
            "properties": {
                "domain": {"type": "string", "description": "the competitor's own website domain, e.g. afloral.com"},
                "name": {"type": "string"},
                "note": {"type": "string", "description": "max 12 words on how it overlaps"}
            },
            "required": ["domain", "name", "note"]
        }}},
        "required": ["competitors"]
    });
    let own = project.domain.clone().unwrap_or_default();
    let prompt = format!(
        "Find direct competitors of this business:\n{}\n\n\
         Run 3 to 5 web searches such as \"alternatives to {name}\", \"{name} competitors\" and category searches for what they sell \
         (for example \"{industry} {country}\"). Return up to 12 companies that sell a similar offer to similar buyers, closest first, \
         each with its own website domain. Leave out marketplaces, directories, review sites, social networks, media sites{own_rule}.",
        profile.brief(project.domain.as_deref()),
        name = profile.name,
        industry = profile.industry,
        country = profile.country_code,
        own_rule = if own.is_empty() { String::new() } else { format!(" and {own} itself") },
    );
    let list: CompetitorList = ctx.ask(&Call::research(RESEARCHER, prompt, schema, 10)).await?;
    ctx.log(format!("checking {} candidates…", list.competitors.len())).await?;

    let mut seen = HashSet::new();
    let kept: Vec<(String, CompetitorItem)> = list
        .competitors
        .into_iter()
        .filter_map(|c| util_input::domain(&c.domain).map(|d| (d, c)))
        .filter(|(d, _)| *d != own && !is_directory(d) && seen.insert(d.clone()))
        .take(12)
        .collect();

    let mut tx = ctx.db().begin().await?;
    sqlx::query("delete from competitors where project_id = $1").bind(project_id).execute(&mut *tx).await?;
    for (domain, c) in &kept {
        sqlx::query("insert into competitors (project_id, domain, name, note) values ($1, $2, $3, $4) on conflict do nothing")
            .bind(project_id)
            .bind(domain)
            .bind(c.name.trim())
            .bind(c.note.trim())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    ctx.log(format!("keeping the {} closest…", kept.len())).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct SegmentDraft {
    pub name: String,
    pub icon: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub est_size: i64,
    #[serde(default)]
    pub fit_score: i32,
    #[serde(default)]
    pub targeting: Value,
    #[serde(default)]
    pub pain_points: String,
    #[serde(default)]
    pub angle: String,
}

impl SegmentDraft {
    fn icon(&self) -> &str {
        if SEGMENT_ICONS.contains(&self.icon.as_str()) { &self.icon } else { "building" }
    }
}

fn segment_schema() -> Value {
    let list = json!({"type": "array", "items": {"type": "string"}});
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "2 to 4 words, plural, e.g. Wedding Planners"},
            "icon": {"type": "string", "enum": SEGMENT_ICONS},
            "description": {"type": "string", "description": "one sentence"},
            "est_size": {"type": "integer", "description": "estimated number of such businesses worldwide"},
            "fit_score": {"type": "integer", "minimum": 0, "maximum": 100},
            "targeting": {
                "type": "object",
                "properties": {
                    "industries": list,
                    "roles": list,
                    "company_sizes": {"type": "array", "items": {"type": "string", "enum": EMPLOYEE_RANGES}},
                    "countries": list,
                    "keywords": list
                },
                "required": ["industries", "roles", "company_sizes", "countries", "keywords"]
            },
            "pain_points": {"type": "string"},
            "angle": {"type": "string"}
        },
        "required": ["name", "icon", "description", "est_size", "fit_score", "targeting", "pain_points", "angle"]
    })
}

const SEGMENT_FIELDS: &str = "For each segment:
- name: 2 to 4 words, plural, like \"Wedding Planners\" or \"Boutique Hotels\".
- icon: the closest icon from the list.
- description: one sentence on who they are and why they buy.
- est_size: a realistic estimate of how many such businesses exist worldwide (an integer, the right order of magnitude matters more than precision).
- fit_score: 0 to 100, how likely they are to buy and how much they would spend.
- targeting: industries, roles (job titles of the person to email, e.g. Owner, Creative Director, Purchasing Manager), \
company_sizes (headcount ranges), countries (ISO codes where this business can serve them), keywords (search terms that find these businesses on the web).
- pain_points: one or two sentences on the problem they have that this business solves.
- angle: one sentence, the outreach angle that ties their pain to this business's offer.";

#[derive(Deserialize)]
struct SegmentList {
    segments: Vec<SegmentDraft>,
}

async fn segments(ctx: &Ctx, project_id: Uuid) -> anyhow::Result<()> {
    let (project, profile) = load_project(ctx, project_id).await?;
    let competitors: Vec<String> = sqlx::query_scalar("select domain::text from competitors where project_id = $1")
        .bind(project_id)
        .fetch_all(ctx.db())
        .await?;
    let kept: Vec<String> = sqlx::query_scalar(
        "select name from segments s where project_id = $1 and (status <> 'draft' or exists (select 1 from leads l where l.segment_id = s.id))",
    )
    .bind(project_id)
    .fetch_all(ctx.db())
    .await?;
    let targeting = prospect::project_targeting(ctx, project_id).await?;

    ctx.log("clustering use cases…").await?;
    let prompt = format!(
        "Business:\n{}\nCompetitors: {}\n{}{}\n\
         Define 4 to 6 distinct buyer segments (campaigns): types of businesses that would plausibly buy from this business. \
         Think about who uses the product in their own work, who resells it, and who buys in bulk. Each segment must be a kind of \
         business that can be found on the web and emailed. Segments must not overlap.\n\n{SEGMENT_FIELDS}\n\nOrder by fit_score, best first.",
        profile.brief(project.domain.as_deref()),
        if competitors.is_empty() { "none found".into() } else { competitors.join(", ") },
        targeting.describe(),
        if kept.is_empty() { String::new() } else { format!("\nAlready running campaigns (don't repeat them): {}.", kept.join(", ")) },
    );
    let schema = json!({
        "type": "object",
        "properties": {"segments": {"type": "array", "minItems": 4, "maxItems": 6, "items": segment_schema()}},
        "required": ["segments"]
    });
    let mut list: SegmentList = ctx.ask(&Call::writing(RESEARCHER, prompt, schema)).await?;
    ctx.log("matching pains to buyers…").await?;
    list.segments.sort_by_key(|s| std::cmp::Reverse(s.fit_score));

    let kept_names: HashSet<String> = kept.iter().map(|n| n.to_lowercase()).collect();
    let mut tx = ctx.db().begin().await?;
    sqlx::query(
        "delete from segments s where project_id = $1 and status = 'draft' and not exists (select 1 from leads l where l.segment_id = s.id)",
    )
    .bind(project_id)
    .execute(&mut *tx)
    .await?;
    let mut added = 0;
    for s in list.segments.iter().filter(|s| !s.name.trim().is_empty() && !kept_names.contains(&s.name.trim().to_lowercase())).take(6) {
        sqlx::query(
            "insert into segments (project_id, name, description, icon, est_size, fit_score, targeting, pain_points, angle)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(project_id)
        .bind(s.name.trim())
        .bind(s.description.trim())
        .bind(s.icon())
        .bind(s.est_size.max(0))
        .bind(s.fit_score.clamp(0, 100))
        .bind(&s.targeting)
        .bind(s.pain_points.trim())
        .bind(s.angle.trim())
        .execute(&mut *tx)
        .await?;
        added += 1;
    }
    sqlx::query(
        "update segments s set position = r.pos
         from (select id, (row_number() over (order by fit_score desc, created_at) - 1)::int as pos from segments where project_id = $1) r
         where s.id = r.id",
    )
    .bind(project_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    ctx.log(format!("scoring {added} campaign ideas…")).await?;
    Ok(())
}

/// The campaign the pipeline works on in steps 4-6.
pub async fn top_segment(ctx: &Ctx, project_id: Uuid) -> anyhow::Result<Segment> {
    sqlx::query_as::<_, Segment>("select * from segments where project_id = $1 order by fit_score desc, position limit 1")
        .bind(project_id)
        .fetch_optional(ctx.db())
        .await?
        .context("there are no campaigns yet, run the research first")
}

pub async fn load_segment(ctx: &Ctx, segment_id: Uuid) -> anyhow::Result<Segment> {
    sqlx::query_as::<_, Segment>("select * from segments where id = $1 and project_id = $2")
        .bind(segment_id)
        .bind(ctx.project_id()?)
        .fetch_optional(ctx.db())
        .await?
        .context("campaign not found")
}

#[derive(Deserialize)]
struct SegmentPromptInput {
    segment_id: Uuid,
    prompt: String,
}

pub async fn segment_from_prompt_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: SegmentPromptInput = ctx.input()?;
    let segment = load_segment(ctx, input.segment_id).await?;
    let (project, profile) = load_project(ctx, segment.project_id).await?;
    let targeting = prospect::project_targeting(ctx, project.id).await?;
    ctx.log("shaping the campaign…").await?;
    let prompt = format!(
        "Business:\n{}\n{}\n\nThe user wants a campaign for this buyer segment: \"{}\"\n\
         Turn it into one well-defined segment of businesses that would buy from this business.\n\n{SEGMENT_FIELDS}",
        profile.brief(project.domain.as_deref()),
        targeting.describe(),
        input.prompt.trim(),
    );
    let s: SegmentDraft = ctx.ask(&Call::writing(RESEARCHER, prompt, segment_schema())).await?;
    sqlx::query(
        "update segments set name = $2, description = $3, icon = $4, est_size = $5, fit_score = $6, targeting = $7, pain_points = $8, angle = $9
         where id = $1",
    )
    .bind(segment.id)
    .bind(s.name.trim())
    .bind(s.description.trim())
    .bind(s.icon())
    .bind(s.est_size.max(0))
    .bind(s.fit_score.clamp(0, 100))
    .bind(&s.targeting)
    .bind(s.pain_points.trim())
    .bind(s.angle.trim())
    .execute(ctx.db())
    .await?;
    Ok(Outcome::new(format!("Campaign \"{}\" is ready", s.name.trim())))
}
