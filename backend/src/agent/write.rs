//! Step 6 (first emails), follow-ups and LinkedIn sequences.

use std::collections::HashMap;

use futures::{StreamExt, TryStreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::explore::{self, Profile, load_project};
use super::sdk::Call;
use super::{Ctx, Outcome};
use crate::models::{EmailMessage, Lead, Project, Segment};

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
    pub email_language: String,
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
            email_language: "auto".into(),
            email_instructions: String::new(),
            follow_ups: json!([]),
            auto_reply_instructions: String::new(),
        }
    }
}

impl WriterSettings {
    pub async fn load(ctx: &Ctx, project_id: Uuid) -> anyhow::Result<Self> {
        let row = sqlx::query_as::<_, Self>(
            "select tone, max_words, cta, booking_link, sender_name, signature, email_language, email_instructions, follow_ups,
                    auto_reply_instructions
             from project_settings where project_id = $1",
        )
        .bind(project_id)
        .fetch_optional(ctx.db())
        .await?;
        Ok(row.unwrap_or_default())
    }

    pub fn language_rule(&self) -> String {
        if self.email_language.is_empty() || self.email_language == "auto" {
            "Write in the main language of the recipient's country (English when unsure).".into()
        } else {
            format!("Write in {}.", self.email_language)
        }
    }

    fn sender(&self, profile: &Profile) -> String {
        if self.sender_name.is_empty() { format!("the team at {}", profile.name) } else { format!("{} at {}", self.sender_name, profile.name) }
    }

    /// Appended by code so the user's exact signature is used.
    pub fn sign(&self, body: &str) -> String {
        let body = body.trim_end();
        let sig = if self.signature.trim().is_empty() { self.sender_name.trim() } else { self.signature.trim() };
        if sig.is_empty() { body.to_string() } else { format!("{body}\n\n{sig}") }
    }
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

fn lead_brief(lead: &Lead) -> Value {
    let location = [lead.city.as_str(), lead.country_code.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
    json!({
        "lead_id": lead.id,
        "name": lead.full_name,
        "first_name": lead.first_name,
        "title": lead.title,
        "company": lead.company_name,
        "domain": lead.company_domain,
        "location": location,
        "hook": lead.personalization["hook"],
        "facts": lead.personalization["facts"],
    })
}

fn has_placeholder(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("{{") || t.contains("[name") || t.contains("[first") || t.contains("[company") || t.contains("{first") || t.contains("{company")
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
        let list = serde_json::to_string_pretty(&missing.iter().map(|l| lead_brief(l)).collect::<Vec<_>>())?;
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

    let mut by_segment: HashMap<Option<Uuid>, Vec<Lead>> = HashMap::new();
    for lead in leads {
        by_segment.entry(lead.segment_id).or_default().push(lead.clone());
    }
    let campaigns = by_segment.into_iter().map(|(segment_id, group)| {
        let segment = segment_id.and_then(|id| segments.get(&id));
        let prompt = initial_prompt(&project, &profile, segment, &settings, &materials, instructions);
        write_campaign(ctx, segment, prompt, group, &settings)
    });
    let saved: usize = stream::iter(campaigns).buffer_unordered(CAMPAIGN_AGENTS).try_collect::<Vec<_>>().await?.into_iter().sum();
    ctx.log(format!("wrote {saved} personal emails…")).await?;
    Ok(saved)
}

async fn write_campaign(ctx: &Ctx, segment: Option<&Segment>, prompt: String, leads: Vec<Lead>, settings: &WriterSettings) -> anyhow::Result<usize> {
    let label = segment.map_or("No campaign", |s| s.name.as_str());
    let mut saved = 0;
    for batch in company_batches(leads) {
        ctx.log(format!("{label}: writing to {}…", batch.iter().map(|l| l.first_name.as_str()).collect::<Vec<_>>().join(", "))).await?;
        let drafts = ask_drafts(ctx, prompt.clone(), &batch, true).await?;
        for lead in &batch {
            let Some(d) = drafts.get(&lead.id) else { continue };
            save_initial(ctx, lead, segment, d.subject.trim(), &settings.sign(&d.body)).await?;
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

fn initial_prompt(project: &Project, profile: &Profile, segment: Option<&Segment>, s: &WriterSettings, materials: &str, instructions: &str) -> String {
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
         - Start with a greeting and their first name on its own line, in the language of the email (\"Hi Dana,\", \"Bonjour Robin,\").\n\
         - First sentence: one specific observation about their business from the lead's hook or facts. No flattery, no \"I hope this finds you well\", no \"I came across\".\n\
         - Then one or two sentences that tie that observation to a concrete problem and how we solve it.\n\
         - End with one short question as the call to action: {cta}.{booking}\n\
         - At most {max_words} words. Tone: {tone}. {language}\n\
         - Short paragraphs separated by a blank line. No sign-off or signature, it is added automatically.\n\
         - Leads at the same company may compare notes. Give each of them a different opening observation and an angle that fits their \
         own role, and never reuse a sentence between two emails in this list.\n\
         {extra_block}",
        sender = s.sender(profile),
        brief = profile.brief(project.domain.as_deref()),
        materials_block = if materials.is_empty() { String::new() } else { format!("Materials (use a fact only when it helps this lead):\n{materials}\n") },
        name = profile.name,
        max_words = s.max_words.clamp(30, 300),
        tone = s.tone,
        language = s.language_rule(),
        extra_block = if extra.is_empty() { String::new() } else { format!("- Also: {extra}\n") },
    )
}

async fn save_initial(ctx: &Ctx, lead: &Lead, segment: Option<&Segment>, subject: &str, body: &str) -> anyhow::Result<()> {
    let updated = sqlx::query(
        "update email_messages set subject = $2, body = $3
         where id = (select id from email_messages where lead_id = $1 and kind = 'initial' and status = any($4)
                     order by created_at desc limit 1)",
    )
    .bind(lead.id)
    .bind(subject)
    .bind(body)
    .bind(&UNSENT[..])
    .execute(ctx.db())
    .await?;
    if updated.rows_affected() > 0 {
        return Ok(());
    }
    // A campaign that is already sending takes new leads straight into the queue.
    let sending = segment.is_some_and(|s| s.status == "working" || s.status == "scaling");
    sqlx::query(
        "insert into email_messages (project_id, lead_id, segment_id, direction, kind, step, subject, body, status, scheduled_for, to_email)
         values ($1, $2, $3, 'outbound', 'initial', 0, $4, $5, $6, $7, $8)",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(lead.segment_id)
    .bind(subject)
    .bind(body)
    .bind(if sending { "approved" } else { "draft" })
    .bind(sending.then(chrono::Utc::now))
    .bind(&lead.email)
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
            sqlx::query_as::<_, Lead>("select * from leads where project_id = $1 and id = any($2) and email is not null")
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
}

/// Writes the next follow-up for each lead, threaded under the last email they got.
/// With `rewrite`, an unsent follow-up at that step gets new text instead of being skipped.
async fn write_follow_ups(ctx: &Ctx, project_id: Uuid, lead_ids: &[Uuid], rewrite: bool, instructions: &str) -> anyhow::Result<usize> {
    let (project, profile) = load_project(ctx, project_id).await?;
    let settings = WriterSettings::load(ctx, project_id).await?;
    let rules: Vec<FollowUpRule> = serde_json::from_value(settings.follow_ups.clone()).unwrap_or_default();
    let segments = segments_by_id(ctx, project_id).await?;
    let leads = sqlx::query_as::<_, Lead>("select * from leads where project_id = $1 and id = any($2) and status = 'contacted'")
        .bind(project_id)
        .bind(lead_ids)
        .fetch_all(ctx.db())
        .await?;

    let mut saved = 0;
    for lead in &leads {
        let thread = sqlx::query_as::<_, EmailMessage>(
            "select * from email_messages where lead_id = $1 and direction = 'outbound' and kind in ('initial', 'follow_up') and status = 'sent'
             order by step",
        )
        .bind(lead.id)
        .fetch_all(ctx.db())
        .await?;
        let (Some(first), Some(last)) = (thread.first(), thread.last()) else { continue };
        let step = last.step + 1;
        let Some(rule) = rules.get(step as usize - 1).filter(|r| r.enabled) else { continue };
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

        let earlier = thread.iter().map(|e| format!("--- email {} ---\n{}", e.step + 1, e.body)).collect::<Vec<_>>().join("\n");
        let max_words = (settings.max_words * 2 / 3).clamp(30, 150);
        let prompt = format!(
            "Write follow-up number {step} to the lead below, from {sender}. They got these emails and have not replied:\n{earlier}\n\n\
             About us:\n{brief}\n\n\
             Rules: it is sent as a reply in the same thread, so no subject and no \"just following up\" or guilt-tripping. Start with a \
             greeting and their first name, in the language of the earlier emails. Add one new angle, fact or offer that the earlier emails didn't use, then one short question. At most \
             {max_words} words. Tone: {tone}. {language} No sign-off.{extra}",
            sender = settings.sender(&profile),
            brief = profile.brief(project.domain.as_deref()),
            tone = settings.tone,
            language = settings.language_rule(),
            extra = {
                let extra = [settings.email_instructions.trim(), rule.instructions.trim(), instructions.trim()].iter().filter(|t| !t.is_empty()).cloned().collect::<Vec<_>>().join(" ");
                if extra.is_empty() { String::new() } else { format!(" Also: {extra}") }
            },
        );
        ctx.log(format!("writing follow-up {step} to {}…", lead.first_name)).await?;
        let drafts = ask_drafts(ctx, prompt, std::slice::from_ref(lead), false).await?;
        let Some(d) = drafts.get(&lead.id) else { continue };

        if let Some(id) = to_rewrite {
            // The sender may have sent it while the agent was writing. Leave sent emails alone.
            sqlx::query("update email_messages set body = $2 where id = $1 and status = any($3)")
                .bind(id)
                .bind(settings.sign(&d.body))
                .bind(&UNSENT[..])
                .execute(ctx.db())
                .await?;
            saved += 1;
            continue;
        }

        let subject = if first.subject.to_lowercase().starts_with("re:") { first.subject.clone() } else { format!("Re: {}", first.subject) };
        let sending = lead.segment_id.and_then(|id| segments.get(&id)).is_some_and(|s| s.status == "working" || s.status == "scaling");
        let due = last.sent_at.unwrap_or_else(chrono::Utc::now) + chrono::Duration::days(rule.delay_days.max(1));
        sqlx::query(
            "insert into email_messages (project_id, lead_id, segment_id, direction, kind, step, subject, body, status, scheduled_for,
                                         in_reply_to, to_email)
             values ($1, $2, $3, 'outbound', 'follow_up', $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(project_id)
        .bind(lead.id)
        .bind(lead.segment_id)
        .bind(step)
        .bind(subject)
        .bind(settings.sign(&d.body))
        .bind(if sending { "approved" } else { "draft" })
        .bind(due)
        .bind(&last.message_id)
        .bind(&lead.email)
        .execute(ctx.db())
        .await?;
        saved += 1;
    }
    Ok(saved)
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
         Tone: {tone}. {language} Start each with their first name.",
        sender = settings.sender(&profile),
        lead = serde_json::to_string_pretty(&lead_brief(&lead))?,
        brief = profile.brief(project.domain.as_deref()),
        campaign = segment.map(|s| format!("Campaign angle: {}", s.angle)).unwrap_or_default(),
        tone = settings.tone,
        language = settings.language_rule(),
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
    for (step, kind, body) in [(1, "connect", connect.as_str()), (2, "message", seq.message_1.trim()), (3, "message", seq.message_2.trim())] {
        sqlx::query("insert into linkedin_messages (lead_id, project_id, step, kind, body) values ($1, $2, $3, $4, $5)")
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
