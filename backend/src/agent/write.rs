//! Step 6 (first emails), follow-ups, LinkedIn sequences and text steps.

use std::collections::HashMap;

use futures::{StreamExt, TryStreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::explore::{self, Profile, load_project};
use super::sdk::Call;
use super::{Ctx, Outcome};
use crate::language::{self, Language};
use crate::models::{EmailMessage, Lead, Project, Segment, SegmentStep, Variant};
use crate::{hiring, tech};

const WRITER: &str = "You are the email writer of Splee, an outbound sales tool. You write short, specific, honest B2B cold emails \
that read like a thoughtful person wrote them by hand. Use only facts from the prompt. Never invent results, clients, numbers or \
offers. Never use placeholders such as [Name] or {company}. Plain text only.";

/// The subset of project_settings the writer uses. Demo projects have no settings row and get the defaults.
#[derive(sqlx::FromRow)]
pub struct WriterSettings {
    pub tone: String,
    pub max_words: i32,
    pub cta: String,
    pub booking_link: String,
    pub sender_name: String,
    pub signature: String,
    pub email_instructions: String,
    pub follow_ups: Value,
    pub auto_reply_instructions: String,
}

impl Default for WriterSettings {
    fn default() -> Self {
        Self {
            tone: "friendly".into(),
            max_words: 90,
            cta: String::new(),
            booking_link: String::new(),
            sender_name: String::new(),
            signature: String::new(),
            email_instructions: String::new(),
            follow_ups: json!([]),
            auto_reply_instructions: String::new(),
        }
    }
}

impl WriterSettings {
    pub async fn load(ctx: &Ctx, project_id: Uuid) -> anyhow::Result<Self> {
        let row = sqlx::query_as::<_, Self>(
            "select tone, max_words, cta, booking_link, sender_name, signature, email_instructions, follow_ups, auto_reply_instructions
             from project_settings where project_id = $1",
        )
        .bind(project_id)
        .fetch_optional(ctx.db())
        .await?;
        Ok(row.unwrap_or_default())
    }

    fn sender(&self, profile: &Profile) -> String {
        if self.sender_name.is_empty() { format!("the team at {}", profile.name) } else { format!("{} at {}", self.sender_name, profile.name) }
    }

    /// Appended by code so the user's exact signature is used.
    pub fn sign(&self, body: &str) -> String {
        let body = body.trim_end();
        let sig = signature(&self.signature, &self.sender_name);
        if sig.is_empty() { body.to_string() } else { format!("{body}\n\n{sig}") }
    }
}

/// What `sign` puts under an email: the project's signature, or its sender name when it has none.
pub fn signature<'a>(signature: &'a str, sender_name: &'a str) -> &'a str {
    if signature.trim().is_empty() { sender_name.trim() } else { signature.trim() }
}

async fn materials(ctx: &Ctx, project_id: Uuid) -> anyhow::Result<String> {
    let rows: Vec<(String, String, String, String)> =
        sqlx::query_as("select kind, title, content, url from materials where project_id = $1 order by created_at limit 12")
            .bind(project_id)
            .fetch_all(ctx.db())
            .await?;
    Ok(rows
        .into_iter()
        .map(|(kind, title, content, url)| {
            let content: String = content.chars().take(800).collect();
            format!("- {kind}: {title}{}\n  {content}", if url.is_empty() { String::new() } else { format!(" ({url})") })
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// What's known about each lead's company beyond the lead itself, by lowercase domain, and how we know the lead.
#[derive(Default)]
struct CompanySignals {
    /// Open roles that match the project's hiring signals. Two at most, newest first.
    posts: HashMap<String, Vec<Value>>,
    /// "Uses HubSpot and Intercom", naming only the project's tech picks.
    tech: HashMap<String, String>,
    /// By lead: the company they left and what they did with us there, for leads a job change made. See jobchanges.rs.
    history: HashMap<Uuid, Value>,
    /// The company's leading funding or news signal from the last 90 days. See signals/.
    news: HashMap<String, crate::signals::Brief>,
    /// The team's research questions set to be used in emails, with the company's answers. See research.rs.
    research: HashMap<String, Vec<Value>>,
}

async fn company_signals(ctx: &Ctx, leads: &[&Lead]) -> anyhow::Result<CompanySignals> {
    let Some(project_id) = leads.first().map(|l| l.project_id) else { return Ok(CompanySignals::default()) };
    let domains: Vec<String> = leads.iter().filter_map(|l| l.company_domain.clone()).collect();
    let mut posts: HashMap<String, Vec<Value>> = HashMap::new();
    for post in hiring::open_at(ctx.db(), project_id, &domains).await? {
        let found = posts.entry(post.domain.to_lowercase()).or_default();
        if found.len() < 2 {
            found.push(json!({"role": post.title, "url": post.url, "excerpt": post.excerpt}));
        }
    }
    let ids: Vec<Uuid> = leads.iter().map(|l| l.id).collect();
    let (tech, history) = (tech::fact_lines(ctx.db(), project_id, &domains).await?, crate::jobchanges::histories(ctx.db(), &ids).await?);
    let news = crate::signals::leading(ctx.db(), project_id, &domains).await?;
    let research = super::research::briefs(ctx.db(), project_id, &domains).await?;
    Ok(CompanySignals { posts, tech, history, news, research })
}

fn lead_brief(lead: &Lead, signals: &CompanySignals) -> Value {
    let location = [lead.city.as_str(), lead.country_code.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
    let mut brief = json!({
        "lead_id": lead.id,
        "name": lead.full_name,
        "first_name": lead.first_name,
        "title": lead.title,
        "company": lead.company_name,
        "domain": lead.company_domain,
        "location": location,
        "hook": lead.personalization["hook"],
        "facts": lead.personalization["facts"],
    });
    let domain = lead.company_domain.as_ref().map(|d| d.to_lowercase()).unwrap_or_default();
    if let Some(posts) = signals.posts.get(&domain) {
        brief["hiring"] = json!(posts);
    }
    if let Some(line) = signals.tech.get(&domain) {
        brief["tech"] = json!(line);
    }
    if let Some(news) = signals.news.get(&domain) {
        let source = if news.kind == "funding_filing" { "an SEC Form D filing" } else { "their own blog" };
        brief["news"] = json!({"what": news.headline, "title": news.title, "date": news.happened_on.to_string(), "source": source});
    }
    if let Some(answers) = signals.research.get(&domain) {
        brief["research"] = json!(answers);
    }
    let reasons = crate::score::opener_reasons(&lead.score_reasons);
    if !reasons.is_empty() {
        brief["why_now"] = json!(reasons);
    }
    if let Some(history) = signals.history.get(&lead.id) {
        brief["history"] = history.clone();
    }
    brief
}

fn has_placeholder(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("{{") || t.contains("[name") || t.contains("[first") || t.contains("[company") || has_field(&t)
}

/// A field in single braces, like {last_name} or {sender_name}, that a template left unfilled.
fn has_field(text: &str) -> bool {
    text.split('{').skip(1).any(|rest| rest.split_once('}').is_some_and(|(inner, _)| !inner.is_empty() && inner.chars().all(|c| c.is_alphanumeric() || c == '_')))
}

#[derive(Deserialize)]
struct Drafts {
    emails: Vec<Draft>,
}

#[derive(Deserialize)]
struct Draft {
    lead_id: Uuid,
    #[serde(default)]
    subject: String,
    body: String,
}

fn drafts_schema(with_subject: bool) -> Value {
    let mut properties = json!({"lead_id": {"type": "string"}, "body": {"type": "string"}});
    let mut required = vec!["lead_id", "body"];
    if with_subject {
        properties["subject"] = json!({"type": "string"});
        required.push("subject");
    }
    json!({
        "type": "object",
        "properties": {"emails": {"type": "array", "items": {"type": "object", "properties": properties, "required": required}}},
        "required": ["emails"]
    })
}

/// Asks for drafts and keeps the ones that belong to `leads` and contain no placeholders. One retry when some are missing.
async fn ask_drafts(ctx: &Ctx, prompt: String, leads: &[Lead], with_subject: bool) -> anyhow::Result<HashMap<Uuid, Draft>> {
    let mut result = HashMap::new();
    for attempt in 0..2 {
        let missing: Vec<&Lead> = leads.iter().filter(|l| !result.contains_key(&l.id)).collect();
        if missing.is_empty() {
            break;
        }
        let signals = company_signals(ctx, &missing).await?;
        let list = serde_json::to_string_pretty(&missing.iter().map(|l| lead_brief(l, &signals)).collect::<Vec<_>>())?;
        let retry = if attempt > 0 { "\nWrite every email in full, with real names, and no placeholders." } else { "" };
        let drafts: Drafts =
            ctx.ask(&Call::writing(WRITER, format!("{prompt}\n\nLeads (write exactly one email per lead_id):\n{list}{retry}"), drafts_schema(with_subject))).await?;
        for d in drafts.emails {
            let known = missing.iter().any(|l| l.id == d.lead_id);
            if known && !d.body.trim().is_empty() && !has_placeholder(&d.body) && !has_placeholder(&d.subject) {
                result.insert(d.lead_id, d);
            }
        }
    }
    Ok(result)
}

pub async fn write_for_top_segment(ctx: &Ctx, project_id: Uuid, limit: Option<usize>) -> anyhow::Result<()> {
    let segment = explore::top_segment(ctx, project_id).await?;
    let mut leads = leads_without_initial(ctx.db(), project_id, Some(segment.id)).await?;
    if let Some(limit) = limit {
        leads.truncate(limit);
    }
    let written = write_initial(ctx, project_id, &leads, "").await?;
    anyhow::ensure!(written > 0 || leads.is_empty(), "no emails could be written");
    Ok(())
}

pub async fn leads_without_initial(db: &sqlx::PgPool, project_id: Uuid, segment_id: Option<Uuid>) -> sqlx::Result<Vec<Lead>> {
    sqlx::query_as::<_, Lead>(
        "select l.* from leads l
         where l.project_id = $1 and ($2::uuid is null or l.segment_id = $2) and l.email is not null
           and l.status in ('new', 'queued')
           and not exists (select 1 from email_messages e where e.lead_id = l.id and e.kind = 'initial' and e.status <> 'cancelled')
         order by l.created_at",
    )
    .bind(project_id)
    .bind(segment_id)
    .fetch_all(db)
    .await
}

/// Campaign writers that run at the same time. Each one is a separate `claude` process, so keep this modest.
const CAMPAIGN_AGENTS: usize = 4;
/// Leads per writing call. Coworkers stay in one call even if that makes it bigger.
const BATCH: usize = 5;

/// Writes (or rewrites) the first email for each lead. Returns how many were saved.
/// Every campaign gets its own writer, running side by side, whose prompts only carry that campaign.
async fn write_initial(ctx: &Ctx, project_id: Uuid, leads: &[Lead], instructions: &str) -> anyhow::Result<usize> {
    if leads.is_empty() {
        return Ok(0);
    }
    let (project, profile) = load_project(ctx, project_id).await?;
    let settings = WriterSettings::load(ctx, project_id).await?;
    let materials = materials(ctx, project_id).await?;
    let segments = segments_by_id(ctx, project_id).await?;
    let mut leads = leads.to_vec();
    crate::ab_test::assign(ctx.db(), &mut leads).await?;
    let variants = variants_by_id(ctx, project_id).await?;
    let imported = imported_steps(ctx, project_id).await?;
    let languages = language::for_leads(ctx.db(), &leads.iter().map(|l| l.id).collect::<Vec<_>>()).await?;

    // Each variant of a campaign gets its own prompt, and so does each language its leads are written in. A campaign without
    // variants in one language is one group, as before.
    // Keyed by the language's code as a String: a &'static str here makes the stream below not Send.
    let mut groups: HashMap<(Option<Uuid>, Option<Uuid>, String), Vec<Lead>> = HashMap::new();
    let mut filled = 0;
    for lead in leads {
        let lang = languages.get(&lead.id).copied().unwrap_or(language::ENGLISH);
        let own = lead.segment_id.and_then(|id| imported.get(&(id, 0)));
        if let Some((subject, body)) = own.and_then(|s| Some((fill(&s.subject, &lead, &settings)?, fill(&s.body, &lead, &settings)?))) {
            save_initial(ctx, &lead, lead.segment_id.and_then(|id| segments.get(&id)), subject.trim(), &settings.sign(&body), lang).await?;
            filled += 1;
            continue;
        }
        groups.entry((lead.segment_id, lead.variant_id, lang.code.to_string())).or_default().push(lead);
    }
    let campaigns = groups.into_iter().map(|((segment_id, variant_id, code), group)| {
        let segment = segment_id.and_then(|id| segments.get(&id));
        let variant = variant_id.and_then(|id| variants.get(&id));
        let own = segment_id.and_then(|id| imported.get(&(id, 0)));
        let lang = language::or_english(&code);
        // The campaign's own email goes last, so it wins over the default subject, opening and length in the rules.
        let prompt =
            initial_prompt(&project, &profile, segment, variant, &settings, &materials, instructions, lang) + &own.map(own_email_rule).unwrap_or_default();
        write_campaign(ctx, segment, variant, prompt, group, &settings, lang)
    });
    let saved: usize = filled + stream::iter(campaigns).buffer_unordered(CAMPAIGN_AGENTS).try_collect::<Vec<_>>().await?.into_iter().sum::<usize>();
    ctx.log(format!("wrote {saved} personal emails…")).await?;
    Ok(saved)
}

async fn write_campaign(
    ctx: &Ctx,
    segment: Option<&Segment>,
    variant: Option<&Variant>,
    prompt: String,
    leads: Vec<Lead>,
    settings: &WriterSettings,
    lang: &Language,
) -> anyhow::Result<usize> {
    let campaign = segment.map_or("No campaign", |s| s.name.as_str());
    let label = variant.map_or_else(|| campaign.to_string(), |v| format!("{campaign} ({})", v.name));
    let label = if lang.code == "en" { label } else { format!("{label}, in {}", lang.name) };
    let mut saved = 0;
    for batch in company_batches(leads) {
        ctx.log(format!("{label}: writing to {}…", batch.iter().map(|l| l.first_name.as_str()).collect::<Vec<_>>().join(", "))).await?;
        let drafts = ask_drafts(ctx, prompt.clone(), &batch, true).await?;
        for lead in &batch {
            let Some(d) = drafts.get(&lead.id) else { continue };
            save_initial(ctx, lead, segment, d.subject.trim(), &settings.sign(&d.body), lang).await?;
            saved += 1;
        }
    }
    Ok(saved)
}

/// Packs leads into batches of about BATCH, keeping coworkers together so the writer can tell their emails apart.
fn company_batches(leads: Vec<Lead>) -> Vec<Vec<Lead>> {
    let mut companies: Vec<Vec<Lead>> = Vec::new();
    let mut index: HashMap<Uuid, usize> = HashMap::new();
    for lead in leads {
        // Imported leads may have no company row. They are their own group.
        let key = lead.company_id.unwrap_or(lead.id);
        match index.get(&key) {
            Some(&i) => companies[i].push(lead),
            None => {
                index.insert(key, companies.len());
                companies.push(vec![lead]);
            }
        }
    }
    let mut batches: Vec<Vec<Lead>> = Vec::new();
    for company in companies {
        match batches.last_mut() {
            Some(batch) if batch.len() + company.len() <= BATCH => batch.extend(company),
            _ => batches.push(company),
        }
    }
    batches
}

async fn segments_by_id(ctx: &Ctx, project_id: Uuid) -> anyhow::Result<HashMap<Uuid, Segment>> {
    let rows = sqlx::query_as::<_, Segment>("select * from segments where project_id = $1").bind(project_id).fetch_all(ctx.db()).await?;
    Ok(rows.into_iter().map(|s| (s.id, s)).collect())
}

/// The emails of campaigns brought over from another tool, by campaign and position (0 is the first email).
async fn imported_steps(ctx: &Ctx, project_id: Uuid) -> anyhow::Result<HashMap<(Uuid, i32), SegmentStep>> {
    let rows: Vec<(Uuid, i32, i32, String, String)> = sqlx::query_as(
        "select t.segment_id, t.position, t.delay_days, t.subject, t.body from segment_steps t join segments s on s.id = t.segment_id
         where s.project_id = $1",
    )
    .bind(project_id)
    .fetch_all(ctx.db())
    .await?;
    Ok(rows.into_iter().map(|(segment, position, delay_days, subject, body)| ((segment, position), SegmentStep { position, delay_days, subject, body })).collect())
}

async fn variants_by_id(ctx: &Ctx, project_id: Uuid) -> anyhow::Result<HashMap<Uuid, Variant>> {
    let rows = sqlx::query_as::<_, Variant>(
        "select v.* from segment_variants v join segments s on s.id = v.segment_id where s.project_id = $1",
    )
    .bind(project_id)
    .fetch_all(ctx.db())
    .await?;
    Ok(rows.into_iter().map(|v| (v.id, v)).collect())
}

#[allow(clippy::too_many_arguments)]
fn initial_prompt(
    project: &Project,
    profile: &Profile,
    segment: Option<&Segment>,
    variant: Option<&Variant>,
    s: &WriterSettings,
    materials: &str,
    instructions: &str,
    lang: &Language,
) -> String {
    let campaign = segment
        .map(|seg| format!("Campaign: {}. Their pain: {} Outreach angle: {}", seg.name, seg.pain_points, seg.angle))
        .unwrap_or_default();
    let cta = if s.cta.trim().is_empty() {
        "a low-friction yes/no question, such as offering a sample, a short call or a quick look at something".to_string()
    } else {
        s.cta.trim().to_string()
    };
    let booking = if s.booking_link.trim().is_empty() {
        String::new()
    } else {
        format!(" If the CTA asks for a call, you may add the booking link {} on its own line.", s.booking_link.trim())
    };
    let extra = [s.email_instructions.trim(), instructions.trim()].iter().filter(|t| !t.is_empty()).cloned().collect::<Vec<_>>().join(" ");
    format!(
        "Write a first cold email to each lead below, from {sender}.\n\n\
         About us:\n{brief}\n{campaign}\n{materials_block}\n\
         Rules:\n\
         - Subject: \"{name} x <their company name>\", e.g. \"Larksilk x Tupelo Honey Flower\".\n\
         - Start with a greeting and their first name on its own line, like \"Hi Dana,\".\n\
         - First sentence: one specific observation about their business from the lead's hook or facts. No flattery, no \"I hope this finds you well\", no \"I came across\".\n\
         - A lead's \"hiring\" lists roles their company has open right now, from its job board. When one is a role our offer helps \
         with, it usually makes the best first observation: name the role in plain words and tie it to what we do. Never paste the link.\n\
         - A lead's \"tech\" names tools their company uses that matter to our offer, like \"Uses HubSpot\". When no job post fits, it can \
         make the first observation: name the tool plainly and tie it to what we do. Never say how we know.\n\
         - A lead's \"news\" is something their company did in the last 90 days, as its \"source\" reported it: money raised, a \
         launch, an acquisition, a new office or a new leader. When no job post fits, it can make the first observation. Say it \
         plainly and only as \"what\" and \"title\" put it: keep the amount as given, call a filing raising money, never name a round \
         or an investor it doesn't name, and never say how we know. Never paste the link.\n\
         - A lead's \"research\" holds questions our team asked about their company with the answers we found. They're facts you \
         can build the first observation or the problem on. Never quote the question or say we researched them.\n\
         - A lead's \"why_now\" holds the two strongest reasons the lead ranks high, as the user sees them on the lead. The first \
         sentence may build on one that's about their company, like a hire or their size. Never mention a visit to our website, a page \
         they read, their reply or a deal.\n\
         - A lead's \"history\" means they already know us: at their previous company they did what it says, and they just started \
         their new role. Open with that plainly instead of an observation, like \"You worked with us at Acme, so I wanted to say \
         congrats on the new role.\" Then say how we could help at the new company. Never say how we noticed the move.\n\
         - Then one or two sentences that tie that observation to a concrete problem and how we solve it.\n\
         - End with one short question as the call to action: {cta}.{booking}\n\
         - At most {max_words} words. Tone: {tone}.\n\
         - {language}\n\
         - Short paragraphs separated by a blank line. No sign-off or signature, it is added automatically.\n\
         - Leads at the same company may compare notes. Give each of them a different opening observation and an angle that fits their \
         own role, and never reuse a sentence between two emails in this list.\n\
         {extra_block}{variant_block}",
        sender = s.sender(profile),
        brief = profile.brief(project.domain.as_deref()),
        materials_block = if materials.is_empty() { String::new() } else { format!("Materials (use a fact only when it helps this lead):\n{materials}\n") },
        name = profile.name,
        max_words = s.max_words.clamp(30, 300),
        tone = s.tone,
        language = language::instruction(lang),
        extra_block = if extra.is_empty() { String::new() } else { format!("- Also: {extra}\n") },
        variant_block = variant.map(variant_rule).unwrap_or_default(),
    )
}

/// An imported email with the lead's fields filled in, exactly as the user wrote it. None when it still has a field Splee can't
/// fill, like {{pain}} or spintax, or the lead lacks one it uses, so the writer fills those in instead of a lead seeing braces.
fn fill(text: &str, lead: &Lead, settings: &WriterSettings) -> Option<String> {
    let fields = [
        ("{first_name}", lead.first_name.as_str()),
        ("{last_name}", &lead.last_name),
        ("{full_name}", &lead.full_name),
        ("{company}", &lead.company_name),
        ("{title}", &lead.title),
        ("{city}", &lead.city),
        ("{sender_name}", &settings.sender_name),
    ];
    fill_fields(text, &fields)
}

fn fill_fields(text: &str, fields: &[(&str, &str)]) -> Option<String> {
    let mut out = text.to_string();
    for &(field, value) in fields {
        if out.contains(field) {
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            out = out.replace(field, value);
        }
    }
    (!out.contains('{') && !out.contains('}')).then_some(out)
}

/// A campaign brought over from another tool keeps the emails the user wrote there.
fn own_email_rule(step: &SegmentStep) -> String {
    let subject = step.subject.trim();
    let subject = if subject.is_empty() { String::new() } else { format!("Subject: {subject}\n") };
    format!(
        "- The user wrote this campaign's emails themselves, before Splee. Send this one as written, with {{first_name}}, {{company}} and \
         the other fields in single braces filled in from the lead, and anything in double braces replaced by what it asks for, written \
         for the lead. Where this differs from the rules above, follow this.\n{subject}{}\n",
        step.body.trim()
    )
}

/// The A/B test angle goes last so it wins over the default subject, opening and call to action above it.
fn variant_rule(v: &Variant) -> String {
    let instructions = v.instructions.trim();
    if instructions.is_empty() {
        return String::new();
    }
    format!(
        "- These emails test the angle \"{}\": {instructions} Where this differs from the rules above, including the subject, follow this.\n",
        v.name.trim()
    )
}

async fn save_initial(ctx: &Ctx, lead: &Lead, segment: Option<&Segment>, subject: &str, body: &str, lang: &Language) -> anyhow::Result<()> {
    let updated = sqlx::query(
        "update email_messages set subject = $2, body = $3, variant_id = $5, language = $6
         where id = (select id from email_messages where lead_id = $1 and kind = 'initial' and status = any($4)
                     order by created_at desc limit 1)",
    )
    .bind(lead.id)
    .bind(subject)
    .bind(body)
    .bind(&UNSENT[..])
    .bind(lead.variant_id)
    .bind(lang.code)
    .execute(ctx.db())
    .await?;
    if updated.rows_affected() > 0 {
        return Ok(());
    }
    // A campaign that is already sending takes new leads straight into the queue, unless autopilot wrote them and the project
    // approves autopilot's emails. Read now, not when the run was queued, so turning auto-send off holds what's still writing.
    let drafts = ctx.run.input["autopilot"].as_bool().unwrap_or(false)
        && !sqlx::query_scalar::<_, bool>("select autopilot_auto_send from project_settings where project_id = $1")
            .bind(lead.project_id)
            .fetch_optional(ctx.db())
            .await?
            .unwrap_or(false);
    let sending = !drafts && segment.is_some_and(|s| s.status == "working" || s.status == "scaling");
    sqlx::query(
        "insert into email_messages (project_id, lead_id, segment_id, direction, kind, step, subject, body, status, scheduled_for, to_email,
                                     variant_id, language)
         values ($1, $2, $3, 'outbound', 'initial', 0, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(lead.segment_id)
    .bind(subject)
    .bind(body)
    .bind(if sending { "approved" } else { "draft" })
    .bind(sending.then(chrono::Utc::now))
    .bind(&lead.email)
    .bind(lead.variant_id)
    .bind(lang.code)
    .execute(ctx.db())
    .await?;
    if sending {
        sqlx::query("update leads set status = 'queued' where id = $1 and status = 'new'").bind(lead.id).execute(ctx.db()).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct WriteEmailsInput {
    segment_id: Option<Uuid>,
    lead_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    instructions: String,
    /// Set by the mail sender: write the next follow-up for each lead instead of a first email.
    #[serde(default)]
    follow_up: bool,
    /// Rewrite the lead's unsent email (first email or pending follow-up) with the current settings.
    #[serde(default)]
    rewrite: bool,
}

pub async fn write_emails_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: WriteEmailsInput = ctx.input()?;
    let project_id = ctx.project_id()?;
    if input.follow_up {
        let written = write_follow_ups(ctx, project_id, &input.lead_ids.unwrap_or_default(), false, "").await?;
        return Ok(Outcome::new(format!("Wrote {written} follow-ups")));
    }
    if input.rewrite {
        return rewrite_unsent(ctx, project_id, &input.lead_ids.unwrap_or_default(), &input.instructions).await;
    }
    let leads = match input.lead_ids {
        Some(ids) => {
            // A lead whose first email went out never gets a second one, e.g. when a half-finished run runs again.
            sqlx::query_as::<_, Lead>(
                "select * from leads l where project_id = $1 and id = any($2) and email is not null
                   and not exists (select 1 from email_messages e where e.lead_id = l.id and e.kind = 'initial' and e.status in ('sending', 'sent'))",
            )
                .bind(project_id)
                .bind(&ids)
                .fetch_all(ctx.db())
                .await?
        }
        None => leads_without_initial(ctx.db(), project_id, input.segment_id).await?,
    };
    let written = write_initial(ctx, project_id, &leads, &input.instructions).await?;
    anyhow::ensure!(written > 0 || leads.is_empty(), "no emails could be written");
    Ok(Outcome::new(format!("Wrote {written} emails")))
}

/// Statuses of an outbound email that hasn't gone out yet and may still be rewritten.
const UNSENT: [&str; 3] = ["draft", "approved", "scheduled"];

async fn rewrite_unsent(ctx: &Ctx, project_id: Uuid, lead_ids: &[Uuid], instructions: &str) -> anyhow::Result<Outcome> {
    let with_unsent = |kind: &'static str| {
        sqlx::query_as::<_, Lead>(
            "select l.* from leads l where l.project_id = $1 and l.id = any($2) and l.email is not null
               and exists (select 1 from email_messages e where e.lead_id = l.id and e.direction = 'outbound'
                           and e.kind = $3 and e.status = any($4))
             order by l.created_at",
        )
        .bind(project_id)
        .bind(lead_ids)
        .bind(kind)
        .bind(&UNSENT[..])
        .fetch_all(ctx.db())
    };
    let firsts = with_unsent("initial").await?;
    let follow_ups = with_unsent("follow_up").await?;
    let total = firsts.len() + follow_ups.len();

    let mut written = write_initial(ctx, project_id, &firsts, instructions).await?;
    let ids: Vec<Uuid> = follow_ups.iter().map(|l| l.id).collect();
    written += write_follow_ups(ctx, project_id, &ids, true, instructions).await?;
    anyhow::ensure!(written > 0 || total == 0, "no emails could be rewritten");
    Ok(Outcome::new(format!("Rewrote {written} of {total} unsent emails")))
}

#[derive(Deserialize)]
struct FollowUpRule {
    delay_days: i64,
    enabled: bool,
    #[serde(default)]
    instructions: String,
    /// "email", or "call" for a call step, which calls.rs opens instead.
    #[serde(default)]
    kind: Option<String>,
}

/// Writes the next follow-up for each lead, threaded under the last email they got.
/// With `rewrite`, an unsent follow-up at that step gets new text instead of being skipped.
async fn write_follow_ups(ctx: &Ctx, project_id: Uuid, lead_ids: &[Uuid], rewrite: bool, instructions: &str) -> anyhow::Result<usize> {
    let (project, profile) = load_project(ctx, project_id).await?;
    let settings = WriterSettings::load(ctx, project_id).await?;
    let rules: Vec<FollowUpRule> = serde_json::from_value(settings.follow_ups.clone()).unwrap_or_default();
    let segments = segments_by_id(ctx, project_id).await?;
    let variants = variants_by_id(ctx, project_id).await?;
    let imported = imported_steps(ctx, project_id).await?;
    let leads = sqlx::query_as::<_, Lead>("select * from leads where project_id = $1 and id = any($2) and status = 'contacted'")
        .bind(project_id)
        .bind(lead_ids)
        .fetch_all(ctx.db())
        .await?;
    let languages = language::for_leads(ctx.db(), lead_ids).await?;

    let mut saved = 0;
    for lead in &leads {
        let lang = languages.get(&lead.id).copied().unwrap_or(language::ENGLISH);
        let thread = sqlx::query_as::<_, EmailMessage>(
            "select * from email_messages where lead_id = $1 and direction = 'outbound' and kind in ('initial', 'follow_up') and status = 'sent'
             order by step",
        )
        .bind(lead.id)
        .fetch_all(ctx.db())
        .await?;
        let (Some(first), Some(last)) = (thread.first(), thread.last()) else { continue };
        // A call step done or skipped since the last email moves the sequence on, and the next email counts from it.
        let call: Option<(i32, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
            "select step, closed_at from calls where lead_id = $1 and kind = 'sequence' and status in ('done', 'skipped') and step > $2
             order by step desc limit 1",
        )
        .bind(lead.id)
        .bind(last.step)
        .fetch_optional(ctx.db())
        .await?;
        let (step, anchor) = match call {
            Some((step, at)) => (step + 1, Some(at)),
            None => (last.step + 1, last.sent_at),
        };
        // A campaign brought over from another tool has its own follow-ups, on its own schedule.
        // It only sends the steps it came with, never the project's follow-ups after them.
        let own = lead.segment_id.and_then(|id| imported.get(&(id, step)));
        let is_imported = lead.segment_id.is_some_and(|id| imported.contains_key(&(id, 0)));
        let rule = if is_imported { None } else { rules.get(step as usize - 1).filter(|r| r.enabled && r.kind.as_deref() != Some("call")) };
        if rule.is_none() && own.is_none() {
            continue;
        }
        let delay_days = own.map_or_else(|| rule.map_or(3, |r| r.delay_days), |s| s.delay_days as i64);
        let existing: Option<(Uuid, String)> =
            sqlx::query_as("select id, status from email_messages where lead_id = $1 and kind = 'follow_up' and step = $2")
                .bind(lead.id)
                .bind(step)
                .fetch_optional(ctx.db())
                .await?;
        let to_rewrite = match existing {
            None if !rewrite => None,
            Some((id, status)) if rewrite && UNSENT.contains(&status.as_str()) => Some(id),
            _ => continue,
        };

        // Follow-ups keep the first email's variant, so a reply to one counts toward the angle that started the thread.
        let variant = first.variant_id.and_then(|id| variants.get(&id));
        let angle = variant
            .map(|v| v.instructions.trim())
            .filter(|i| !i.is_empty())
            .map(|i| format!("The first email followed this angle, so keep to it: {i}"));
        let earlier = thread.iter().map(|e| format!("--- email {} ---\n{}", e.step + 1, e.body)).collect::<Vec<_>>().join("\n");
        let max_words = (settings.max_words * 2 / 3).clamp(30, 150);
        let prompt = format!(
            "Write follow-up number {step} to the lead below, from {sender}. They got these emails and have not replied:\n{earlier}\n\n\
             About us:\n{brief}\n\n\
             Rules: it is sent as a reply in the same thread, so no subject and no \"just following up\" or guilt-tripping. Start with a \
             greeting and their first name. Add one new angle, fact or offer that the earlier emails didn't use, then one short question. \
             At most {max_words} words. Tone: {tone}. No sign-off.\n{language}{extra}{own}",
            sender = settings.sender(&profile),
            brief = profile.brief(project.domain.as_deref()),
            tone = settings.tone,
            language = language::instruction(lang),
            extra = {
                let rule = rule.map_or("", |r| r.instructions.trim());
                let extra = [settings.email_instructions.trim(), rule, instructions.trim(), angle.as_deref().unwrap_or_default()]
                    .iter()
                    .filter(|t| !t.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");
                if extra.is_empty() { String::new() } else { format!(" Also: {extra}") }
            },
            own = own.map(|s| format!("\n{}", own_email_rule(s))).unwrap_or_default(),
        );
        let body = match own.and_then(|s| fill(&s.body, lead, &settings)) {
            Some(body) => body,
            None => {
                ctx.log(format!("writing follow-up {step} to {}…", lead.first_name)).await?;
                let mut drafts = ask_drafts(ctx, prompt, std::slice::from_ref(lead), false).await?;
                let Some(d) = drafts.remove(&lead.id) else { continue };
                d.body
            }
        };

        if let Some(id) = to_rewrite {
            // The sender may have sent it while the agent was writing. Leave sent emails alone.
            sqlx::query("update email_messages set body = $2, language = $4 where id = $1 and status = any($3)")
                .bind(id)
                .bind(settings.sign(&body))
                .bind(&UNSENT[..])
                .bind(lang.code)
                .execute(ctx.db())
                .await?;
            saved += 1;
            continue;
        }

        let sending = lead.segment_id.and_then(|id| segments.get(&id)).is_some_and(|s| s.status == "working" || s.status == "scaling");
        let due = anchor.unwrap_or_else(chrono::Utc::now) + chrono::Duration::days(delay_days.max(1));
        insert_follow_up(ctx.db(), lead, &thread, step, &settings.sign(&body), sending, due, lang).await?;
        saved += 1;
    }
    Ok(saved)
}

/// Threads the follow-up under the last email sent, on the first email's variant even if the lead's has changed since.
#[allow(clippy::too_many_arguments)]
async fn insert_follow_up(
    db: &sqlx::PgPool,
    lead: &Lead,
    thread: &[EmailMessage],
    step: i32,
    body: &str,
    sending: bool,
    due: chrono::DateTime<chrono::Utc>,
    lang: &Language,
) -> sqlx::Result<()> {
    let (Some(first), Some(last)) = (thread.first(), thread.last()) else { return Ok(()) };
    let subject = if first.subject.to_lowercase().starts_with("re:") { first.subject.clone() } else { format!("Re: {}", first.subject) };
    sqlx::query(
        "insert into email_messages (project_id, lead_id, segment_id, direction, kind, step, subject, body, status, scheduled_for,
                                     in_reply_to, to_email, variant_id, language)
         values ($1, $2, $3, 'outbound', 'follow_up', $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(lead.segment_id)
    .bind(step)
    .bind(subject)
    .bind(body)
    .bind(if sending { "approved" } else { "draft" })
    .bind(due)
    .bind(&last.message_id)
    .bind(&lead.email)
    .bind(first.variant_id)
    .bind(lang.code)
    .execute(db)
    .await?;
    Ok(())
}

#[derive(Deserialize)]
struct LinkedinInput {
    lead_id: Uuid,
}

#[derive(Deserialize)]
struct LinkedinSequence {
    connect_note: String,
    message_1: String,
    message_2: String,
}

pub async fn linkedin_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: LinkedinInput = ctx.input()?;
    let project_id = ctx.project_id()?;
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1 and project_id = $2")
        .bind(input.lead_id)
        .bind(project_id)
        .fetch_optional(ctx.db())
        .await?
        .ok_or_else(|| anyhow::anyhow!("lead not found"))?;
    let (project, profile) = load_project(ctx, project_id).await?;
    let settings = WriterSettings::load(ctx, project_id).await?;
    let segment = match lead.segment_id {
        Some(id) => segments_by_id(ctx, project_id).await?.remove(&id),
        None => None,
    };

    ctx.log(format!("writing a LinkedIn sequence for {}…", lead.full_name)).await?;
    let prompt = format!(
        "Write a LinkedIn outreach sequence from {sender} to this person:\n{lead}\n\nAbout us:\n{brief}\n{campaign}\n\n\
         - connect_note: the connection request note, at most 280 characters, no pitch, one specific reason to connect.\n\
         - message_1: sent after they accept, at most 500 characters: one observation about their business, how we help, a soft question.\n\
         - message_2: sent a few days later if no answer, at most 350 characters, a different angle and an easy yes/no question.\n\
         Tone: {tone}. Start each with their first name.\n{language}",
        sender = settings.sender(&profile),
        lead = serde_json::to_string_pretty(&lead_brief(&lead, &company_signals(ctx, &[&lead]).await?))?,
        brief = profile.brief(project.domain.as_deref()),
        campaign = segment.map(|s| format!("Campaign angle: {}", s.angle)).unwrap_or_default(),
        tone = settings.tone,
        language = language::instruction(language::for_lead(ctx.db(), lead.id).await?),
    );
    let schema = json!({
        "type": "object",
        "properties": {"connect_note": {"type": "string"}, "message_1": {"type": "string"}, "message_2": {"type": "string"}},
        "required": ["connect_note", "message_1", "message_2"]
    });
    let seq: LinkedinSequence = ctx.ask(&Call::writing(WRITER, prompt, schema)).await?;
    let connect: String = seq.connect_note.trim().chars().take(300).collect();

    let mut tx = ctx.db().begin().await?;
    sqlx::query("delete from linkedin_messages where lead_id = $1 and status = 'todo'").bind(lead.id).execute(&mut *tx).await?;
    // The connect note is due now. Each message comes due once the step before it is sent (routes::linkedin::update).
    for (step, kind, body) in [(1, "connect", connect.as_str()), (2, "message", seq.message_1.trim()), (3, "message", seq.message_2.trim())] {
        sqlx::query(
            "insert into linkedin_messages (lead_id, project_id, step, kind, body, due_at)
             values ($1, $2, $3, $4, $5, case when $3 = 1 then now() end)",
        )
            .bind(lead.id)
            .bind(project_id)
            .bind(step)
            .bind(kind)
            .bind(body)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Outcome::new("LinkedIn sequence ready"))
}

#[derive(Deserialize)]
struct TextsInput {
    lead_ids: Vec<Uuid>,
}

#[derive(Deserialize)]
struct WrittenText {
    text: String,
}

/// Writes each lead's next SMS step that's due, from the sender loop (twilio::schedule). The code adds the sender's name and the
/// STOP line where they're missing, and keeps it to twilio::SMS_MAX_CHARS.
pub async fn texts_run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: TextsInput = ctx.input()?;
    let project_id = ctx.project_id()?;
    let (project, profile) = load_project(ctx, project_id).await?;
    let settings = WriterSettings::load(ctx, project_id).await?;
    let mut written = 0;
    for step in crate::twilio::due_steps(ctx.db(), Some(&input.lead_ids)).await? {
        if step.project_id != project_id || step.channel != "sms" {
            continue;
        }
        let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(step.lead_id).fetch_one(ctx.db()).await?;
        let emails: Vec<String> = sqlx::query_scalar(
            "select body from email_messages where lead_id = $1 and direction = 'outbound' and kind in ('initial', 'follow_up') and status = 'sent'
             order by step",
        )
        .bind(lead.id)
        .fetch_all(ctx.db())
        .await?;
        let texts: Vec<String> =
            sqlx::query_scalar("select body from text_messages where lead_id = $1 and status in ('sent', 'delivered') order by sent_at")
                .bind(lead.id)
                .fetch_all(ctx.db())
                .await?;
        let earlier = emails
            .iter()
            .enumerate()
            .map(|(i, b)| format!("--- email {} ---\n{b}", i + 1))
            .chain(texts.iter().enumerate().map(|(i, b)| format!("--- text {} ---\n{b}", i + 1)))
            .collect::<Vec<_>>()
            .join("\n");
        let who = if settings.sender_name.trim().is_empty() { format!("the team at {}", profile.name) } else { settings.sender_name.trim().to_string() };
        let extra = [settings.email_instructions.trim(), step.instructions.trim()].iter().filter(|t| !t.is_empty()).cloned().collect::<Vec<_>>().join(" ");
        let prompt = format!(
            "Write text message number {n} to the lead below, from {sender}. They got these and have not replied:\n{earlier}\n\n\
             Lead:\n{lead}\n\nAbout us:\n{brief}\n\n\
             Rules: a text, not an email, so at most 260 characters, plain words, no subject, no links, no emoji and no sign-off. Start with \
             their first name{intro}. One short reason to talk, then one easy question. Tone: {tone}.{extra}\n{language}",
            n = step.step,
            sender = settings.sender(&profile),
            lead = serde_json::to_string_pretty(&lead_brief(&lead, &company_signals(ctx, &[&lead]).await?))?,
            brief = profile.brief(project.domain.as_deref()),
            intro = if step.step == 1 { format!(", then say it's {who}, since this is the first text they get from us") } else { String::new() },
            tone = settings.tone,
            language = language::instruction(language::for_lead(ctx.db(), lead.id).await?),
            extra = if extra.is_empty() { String::new() } else { format!(" Also: {extra}") },
        );
        ctx.log(format!("writing text {} to {}…", step.step, lead.first_name)).await?;
        let schema = json!({ "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] });
        let draft: WrittenText = ctx.ask(&Call::writing(WRITER, prompt, schema)).await?;
        if draft.text.trim().is_empty() || has_placeholder(&draft.text) {
            continue;
        }
        let body = crate::twilio::finish_sms(&draft.text, &settings.sender_name, &profile.name, &lead.phone, step.first);
        if crate::twilio::insert_step(ctx.db(), &step, &lead, &body, None).await? {
            written += 1;
        }
    }
    Ok(Outcome::new(format!("Wrote {written} texts")))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use sqlx::PgPool;
    use uuid::Uuid;

    use serde_json::{Value, json};

    use super::{Ctx, company_signals, insert_follow_up, lead_brief, own_email_rule, save_initial, variant_rule};
    use crate::ab_test::{self, tests::{add_leads, seed}};
    use crate::agent::fake;
    use crate::agent::sdk::Call;
    use crate::language::ENGLISH;
    use crate::models::{AgentRun, EmailMessage, Lead, Variant};

    async fn ctx(db: &PgPool, project: Uuid) -> Ctx {
        run_ctx(db, project, "write_emails", json!({})).await
    }

    async fn run_ctx(db: &PgPool, project: Uuid, kind: &str, input: Value) -> Ctx {
        let run = sqlx::query_as::<_, AgentRun>("insert into agent_runs (project_id, kind, input) values ($1, $2, $3) returning *")
            .bind(project)
            .bind(kind)
            .bind(input)
            .fetch_one(db)
            .await
            .unwrap();
        Ctx { state: crate::autopilot::tests::state(db.clone()), run, step: AtomicI32::new(0) }
    }

    /// The fake model's answer to each kind of writing call: an email for every lead_id in the prompt, a LinkedIn sequence or a text.
    fn written(call: &Call) -> Value {
        let props = &call.schema["properties"];
        if props.get("emails").is_some() {
            let ids: Vec<&str> = call.prompt.split("\"lead_id\": \"").skip(1).map(|rest| &rest[..36]).collect();
            json!({ "emails": ids.iter().map(|id| json!({ "lead_id": id, "subject": "Betreff", "body": "Guten Tag" })).collect::<Vec<_>>() })
        } else if props.get("connect_note").is_some() {
            json!({ "connect_note": "Note", "message_1": "One", "message_2": "Two" })
        } else {
            json!({ "text": "Kurze Frage?" })
        }
    }

    /// The prompt that asked about this lead.
    fn prompt_for(prompts: &[String], lead: &Lead) -> String {
        let found: Vec<&String> = prompts.iter().filter(|p| p.contains(&lead.id.to_string())).collect();
        assert_eq!(found.len(), 1, "{prompts:#?}");
        found[0].clone()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn every_message_to_a_lead_is_written_in_their_language(db: PgPool) {
        let c = seed(&db, 0).await;
        sqlx::query("update segments set language = 'lead' where id = $1").bind(c.segment).execute(&db).await.unwrap();
        sqlx::query("insert into project_settings (project_id, paused, follow_ups, text_steps) values ($1, false, '[{\"delay_days\": 3, \"enabled\": true}]', '[{\"delay_days\": 1}]')")
            .bind(c.project)
            .execute(&db)
            .await
            .unwrap();
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(c.project).fetch_one(&db).await.unwrap();
        sqlx::query("insert into twilio_accounts (org_id, account_sid, api_key_sid, api_secret_enc, auth_token_enc, sms_from) values ($1, 'AC1', 'SK1', 'x', 'x', '+15550100')")
            .bind(org)
            .execute(&db)
            .await
            .unwrap();
        let leads = add_leads(&db, &c, 3).await;
        for (lead, country, phone) in leads.iter().zip([("DE", "+4915112345678"), ("FR", "+33612345678"), ("US", "+12025550143")]).map(|(l, (c, p))| (l, c, p)) {
            sqlx::query("update leads set country_code = $2, phone = $3 where id = $1").bind(lead.id).bind(country).bind(phone).execute(&db).await.unwrap();
        }
        let leads: Vec<Lead> = sqlx::query_as("select * from leads where project_id = $1 order by created_at").bind(c.project).fetch_all(&db).await.unwrap();
        let (anna, camille, dana) = (&leads[0], &leads[1], &leads[2]);

        // First emails: one prompt per language, each with its own instruction, and the emails keep their language.
        let ctx = ctx(&db, c.project).await;
        fake::answer_with(ctx.run.id, written);
        assert_eq!(super::write_initial(&ctx, c.project, &leads, "").await.unwrap(), 3);
        let prompts = fake::prompts(ctx.run.id);
        assert_eq!(prompts.len(), 3);
        let german = prompt_for(&prompts, anna);
        assert!(german.contains("write in German, the subject line too") && german.contains("\"Sie\"") && german.contains("not as a translation"), "{german}");
        assert!(!german.contains(&camille.id.to_string()) && !german.contains(&dana.id.to_string()));
        let french = prompt_for(&prompts, camille);
        assert!(french.contains("write in French") && french.contains("\"vous\""), "{french}");
        assert!(prompt_for(&prompts, dana).contains("write in English"));
        let saved: Vec<(Uuid, String, String)> =
            sqlx::query_as("select lead_id, language, subject from email_messages where kind = 'initial' order by lead_id").fetch_all(&db).await.unwrap();
        let language_of = |lead: &Lead| saved.iter().find(|(id, ..)| *id == lead.id).map(|(_, l, s)| (l.clone(), s.clone())).unwrap();
        assert_eq!(language_of(anna), ("de".to_string(), "Betreff".to_string()));
        assert_eq!((language_of(camille).0.as_str(), language_of(dana).0.as_str()), ("fr", "en"));

        // They went out, so each gets a follow-up and a text, in the same language.
        sqlx::query("update email_messages set status = 'sent', sent_at = now() - interval '4 days', message_id = gen_random_uuid()::text where kind = 'initial'")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("update leads set status = 'contacted' where project_id = $1").bind(c.project).execute(&db).await.unwrap();
        let ids: Vec<Uuid> = leads.iter().map(|l| l.id).collect();
        let ctx = run_ctx(&db, c.project, "write_emails", json!({})).await;
        fake::answer_with(ctx.run.id, written);
        assert_eq!(super::write_follow_ups(&ctx, c.project, &ids, false, "").await.unwrap(), 3);
        let prompts = fake::prompts(ctx.run.id);
        let german = prompt_for(&prompts, anna);
        assert!(german.contains("Write follow-up number 1") && german.contains("write in German") && german.contains("\"Sie\""), "{german}");
        assert!(prompt_for(&prompts, camille).contains("write in French"));
        let follow_up: String = sqlx::query_scalar("select language from email_messages where lead_id = $1 and kind = 'follow_up'")
            .bind(anna.id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(follow_up, "de");

        let ctx = run_ctx(&db, c.project, "write_texts", json!({ "lead_ids": [anna.id] })).await;
        fake::answer_with(ctx.run.id, written);
        super::texts_run(&ctx).await.unwrap();
        let prompts = fake::prompts(ctx.run.id);
        assert!(prompts.len() == 1 && prompts[0].contains("Write text message number 1") && prompts[0].contains("write in German"), "{prompts:#?}");

        let ctx = run_ctx(&db, c.project, "linkedin_sequence", json!({ "lead_id": camille.id })).await;
        fake::answer_with(ctx.run.id, written);
        super::linkedin_run(&ctx).await.unwrap();
        let prompts = fake::prompts(ctx.run.id);
        assert!(prompts[0].contains("LinkedIn outreach sequence") && prompts[0].contains("write in French") && prompts[0].contains("\"vous\""), "{prompts:#?}");

        // A campaign set to one language writes everyone in it, whatever their country.
        sqlx::query("update segments set language = 'es' where id = $1").bind(c.segment).execute(&db).await.unwrap();
        let ctx = run_ctx(&db, c.project, "linkedin_sequence", json!({ "lead_id": anna.id })).await;
        fake::answer_with(ctx.run.id, written);
        super::linkedin_run(&ctx).await.unwrap();
        let prompt = &fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("write in Spanish") && prompt.contains("\"usted\""), "{prompt}");
    }

    #[test]
    fn the_variant_rule_comes_with_its_instructions_only() {
        let mut v = Variant {
            id: Uuid::nil(),
            segment_id: Uuid::nil(),
            name: "Cost".into(),
            instructions: "Lead with what they spend on agencies.".into(),
            position: 0,
            paused_at: None,
            created_at: chrono::Utc::now(),
        };
        let rule = variant_rule(&v);
        assert!(rule.contains("\"Cost\": Lead with what they spend on agencies.") && rule.contains("including the subject"), "{rule}");
        v.instructions = "  ".into();
        assert_eq!(variant_rule(&v), "");
    }

    #[test]
    fn an_imported_email_goes_out_as_written_when_every_field_fills() {
        let fields = [("{first_name}", "Ana"), ("{company}", "Delta"), ("{city}", ""), ("{sender_name}", "Maya")];
        assert_eq!(super::fill_fields("Hi {first_name},\n\nHow does {company} plan visits?\n{sender_name}", &fields).as_deref(),
            Some("Hi Ana,\n\nHow does Delta plan visits?\nMaya"));
        // A field Splee can't fill, or one the lead lacks, goes to the writer instead.
        assert_eq!(super::fill_fields("Hi {first_name}, {{pain}}", &fields), None);
        assert_eq!(super::fill_fields("Hi {first_name} at {city}", &fields), None);
        // And what the writer sends back is dropped if it kept one.
        assert!(super::has_placeholder("Hi Ana, from {sender_name}") && super::has_placeholder("{Last_Name}") && !super::has_placeholder("Hi Ana { not a field }"));
    }

    #[test]
    fn a_campaign_brought_over_keeps_its_own_email() {
        let step = crate::models::SegmentStep { position: 0, delay_days: 0, subject: "{company} x Fieldnote".into(), body: "Hi {first_name},\n\n{{icebreaker}}".into() };
        let rule = own_email_rule(&step);
        assert!(rule.contains("Send this one as written") && rule.ends_with("Subject: {company} x Fieldnote\nHi {first_name},\n\n{{icebreaker}}\n"), "{rule}");
        let follow_up = crate::models::SegmentStep { subject: String::new(), ..step };
        assert!(!own_email_rule(&follow_up).contains("Subject:"));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn follow_ups_keep_the_first_emails_variant(db: PgPool) {
        let c = seed(&db, 2).await;
        let ctx = ctx(&db, c.project).await;
        let mut leads = add_leads(&db, &c, 2).await;
        ab_test::assign(&db, &mut leads).await.unwrap();
        let segment = sqlx::query_as("select * from segments where id = $1").bind(c.segment).fetch_one(&db).await.unwrap();
        for lead in &leads {
            save_initial(&ctx, lead, Some(&segment), "Hi", "Hello", ENGLISH).await.unwrap();
        }
        let first: Vec<(Uuid, Option<Uuid>)> =
            sqlx::query_as("select lead_id, variant_id from email_messages where kind = 'initial' order by created_at").fetch_all(&db).await.unwrap();
        assert_eq!(first, vec![(leads[0].id, Some(c.variants[0])), (leads[1].id, Some(c.variants[1]))]);

        // Even after auto-pick pauses the first lead's variant and the lead somehow gets another, its follow-up stays on A.
        sqlx::query("update email_messages set status = 'sent', sent_at = now(), message_id = 'm1' where lead_id = $1")
            .bind(leads[0].id)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("update segment_variants set paused_at = now() where id = $1").bind(c.variants[0]).execute(&db).await.unwrap();
        let mut lead = leads[0].clone();
        lead.variant_id = Some(c.variants[1]);
        let thread = sqlx::query_as::<_, EmailMessage>("select * from email_messages where lead_id = $1").bind(lead.id).fetch_all(&db).await.unwrap();
        insert_follow_up(&db, &lead, &thread, 1, "Following on", true, chrono::Utc::now(), ENGLISH).await.unwrap();
        let (step, subject, variant): (i32, String, Option<Uuid>) =
            sqlx::query_as("select step, subject, variant_id from email_messages where lead_id = $1 and kind = 'follow_up'")
                .bind(lead.id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!((step, subject.as_str(), variant), (1, "Re: Hi", Some(c.variants[0])));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn campaigns_without_variants_write_untagged_emails(db: PgPool) {
        let c = seed(&db, 0).await;
        let ctx = ctx(&db, c.project).await;
        let mut leads = add_leads(&db, &c, 1).await;
        ab_test::assign(&db, &mut leads).await.unwrap();
        let segment = sqlx::query_as("select * from segments where id = $1").bind(c.segment).fetch_one(&db).await.unwrap();
        save_initial(&ctx, &leads[0], Some(&segment), "Hi", "Hello", ENGLISH).await.unwrap();
        let (status, variant): (String, Option<Uuid>) =
            sqlx::query_as("select status, variant_id from email_messages where lead_id = $1").bind(leads[0].id).fetch_one(&db).await.unwrap();
        assert_eq!((status.as_str(), variant), ("approved", None));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn briefs_say_which_picked_tools_the_company_uses(db: PgPool) {
        let c = seed(&db, 0).await;
        let ctx = ctx(&db, c.project).await;
        let leads = add_leads(&db, &c, 2).await;
        for (lead, domain) in leads.iter().zip(["Acme.com", "plain.com"]) {
            sqlx::query("update leads set company_domain = $2 where id = $1").bind(lead.id).bind(domain).execute(&db).await.unwrap();
        }
        sqlx::query("insert into tech_scans (domain, techs, scanned_at) values ('acme.com', '{HubSpot,Intercom,Cloudflare}', now()), ('plain.com', '{Cloudflare}', now())")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into project_tech_picks (project_id, tech, kind, position) values ($1, 'HubSpot', 'competitor', 0), ($1, 'Intercom', 'uses', 1)")
            .bind(c.project)
            .execute(&db)
            .await
            .unwrap();
        let leads: Vec<Lead> =
            sqlx::query_as("select * from leads where id = any($1) order by company_domain").bind(leads.iter().map(|l| l.id).collect::<Vec<_>>()).fetch_all(&db).await.unwrap();
        let signals = company_signals(&ctx, &leads.iter().collect::<Vec<_>>()).await.unwrap();
        assert_eq!(lead_brief(&leads[0], &signals)["tech"], "Uses HubSpot and Intercom");
        assert!(lead_brief(&leads[1], &signals).get("tech").is_none());
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn briefs_carry_the_companys_recent_funding_or_news(db: PgPool) {
        let c = seed(&db, 0).await;
        let ctx = ctx(&db, c.project).await;
        let leads = add_leads(&db, &c, 3).await;
        for (lead, domain) in leads.iter().zip(["acme.example", "quiet.example", "old.example"]) {
            sqlx::query("update leads set company_domain = $2 where id = $1").bind(lead.id).bind(domain).execute(&db).await.unwrap();
        }
        sqlx::query(
            "insert into company_signals (project_id, domain, kind, source, key, happened_on, title, amount, offered, url) values
               ($1, 'acme.example', 'launch', 'feed', 'a', current_date - 2, 'Introducing Acme Flow', null, null, 'https://acme.example/flow'),
               ($1, 'acme.example', 'funding_filing', 'sec', 'b', current_date - 41, 'Acme Robotics, Inc.', 12000000, 15000000, 'https://www.sec.gov/b'),
               ($1, 'old.example', 'funding_post', 'feed', 'c', current_date - 91, 'We raised our seed round', null, null, 'https://old.example/seed')",
        )
        .bind(c.project)
        .execute(&db)
        .await
        .unwrap();
        let leads: Vec<Lead> =
            sqlx::query_as("select * from leads where id = any($1) order by company_domain").bind(leads.iter().map(|l| l.id).collect::<Vec<_>>()).fetch_all(&db).await.unwrap();
        let signals = company_signals(&ctx, &leads.iter().collect::<Vec<_>>()).await.unwrap();
        let today = chrono::Utc::now().date_naive();
        let filed = today - chrono::Duration::days(41);
        let when = crate::signals::short_date(filed, today);
        // The filing leads over the newer launch, since money is the stronger opener.
        assert_eq!(
            lead_brief(&leads[0], &signals)["news"],
            serde_json::json!({"what": format!("Raised $12M (SEC filing, {when})"), "title": "Acme Robotics, Inc.", "date": filed.to_string(),
                               "source": "an SEC Form D filing"})
        );
        assert!(lead_brief(&leads[1], &signals).get("news").is_none(), "old.example's post is past 90 days");
        assert!(lead_brief(&leads[2], &signals).get("news").is_none());
        let profile = crate::agent::explore::Profile::default();
        let project = sqlx::query_as("select * from projects where id = $1").bind(c.project).fetch_one(&db).await.unwrap();
        let prompt = super::initial_prompt(&project, &profile, None, None, &super::WriterSettings::default(), "", "", ENGLISH);
        assert!(prompt.contains("A lead's \"news\"") && prompt.contains("keep the amount as given"), "{prompt}");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_lead_from_a_job_change_brings_its_history(db: PgPool) {
        let c = seed(&db, 0).await;
        let ctx = ctx(&db, c.project).await;
        let leads = add_leads(&db, &c, 2).await;
        let watched: Uuid = sqlx::query_scalar(
            "insert into watched_people (project_id, relationship, full_name, linkedin_key, company_name) values ($1, 'booked', 'Dana', 'dana', 'Acme')
             returning id",
        )
        .bind(c.project)
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query("insert into job_changes (project_id, watched_id, old_company, new_company, move_number, new_title, new_lead_id) values ($1, $2, 'Acme', 'Brightline', 1, 'VP Sales', $3)")
            .bind(c.project)
            .bind(watched)
            .bind(leads[0].id)
            .execute(&db)
            .await
            .unwrap();
        let signals = company_signals(&ctx, &leads.iter().collect::<Vec<_>>()).await.unwrap();
        let history = serde_json::json!({ "previous_company": "Acme", "what_they_did_there": "booked a meeting with us", "new_role": "VP Sales" });
        assert_eq!(lead_brief(&leads[0], &signals)["history"], history);
        assert!(lead_brief(&leads[1], &signals).get("history").is_none());
        // The writer is told to open with it plainly.
        let profile = crate::agent::explore::Profile::default();
        let project = sqlx::query_as("select * from projects where id = $1").bind(c.project).fetch_one(&db).await.unwrap();
        let prompt = super::initial_prompt(&project, &profile, None, None, &super::WriterSettings::default(), "", "", ENGLISH);
        assert!(prompt.contains("A lead's \"history\" means they already know us") && prompt.contains("Never say how we noticed the move"), "{prompt}");
    }

    #[test]
    fn briefs_carry_the_two_strongest_reasons_for_the_lead() {
        let mut lead = crate::twilio::tests::test_lead();
        let reason = |text: &str, kind: &str| crate::score::Reason { text: text.into(), kind: kind.into() };
        lead.score_reasons = sqlx::types::Json(vec![
            reason("Hiring 2 SDRs", "intent"),
            reason("51-200 employees", "fit"),
            reason("Based in GB", "fit"),
            reason("Uses a competitor: Outreach", "against"),
        ]);
        let brief = lead_brief(&lead, &Default::default());
        assert_eq!(brief["why_now"], serde_json::json!(["Hiring 2 SDRs", "51-200 employees"]));
        lead.score_reasons = sqlx::types::Json(vec![reason("Uses a competitor: Outreach", "against")]);
        assert!(lead_brief(&lead, &Default::default()).get("why_now").is_none());
    }
}
