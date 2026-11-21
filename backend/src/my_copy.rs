//! My copy: a first email the user writes themselves. Each lead gets it with their details filled in, one option of every
//! {a|b} picked for them, and each {{ai: …}} line written for them by the writer (agent/write.rs). While a campaign sends
//! it, one lead in ten gets the AI email instead, so the two can be compared. Follow-ups are written as before.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::Lead;

pub const VARIABLES: [&str; 7] = ["first_name", "company", "job_title", "company_city", "sender_first_name", "sender_name", "sender_company"];
pub const MAX_SUBJECT: usize = 300;
pub const MAX_BODY: usize = 3000;
pub const MAX_AI_LINES: usize = 5;
/// One lead in this many gets the AI email while My copy is on.
const AI_ONE_IN: u64 = 10;
/// What an empty subject goes out as, the same as the writer's default.
const DEFAULT_SUBJECT: &str = "{{sender_company}} x {{company}}";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Instructions,
    MyCopy,
}

impl Mode {
    pub fn from_db(s: &str) -> Self {
        if s == "my_copy" { Mode::MyCopy } else { Mode::Instructions }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Instructions => "instructions",
            Mode::MyCopy => "my_copy",
        }
    }
}

/// A first email's two texts. Both are kept whatever the mode, so switching loses nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FirstEmail {
    pub mode: Mode,
    pub subject: String,
    pub body: String,
}

impl FirstEmail {
    pub fn from_db(mode: &str, subject: String, body: String) -> Self {
        Self { mode: Mode::from_db(mode), subject, body }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Piece {
    Text(String),
    Var { name: &'static str, fallback: Option<String> },
    Spin(Vec<String>),
    Ai(String),
}

fn braces(name: &str) -> String {
    format!("{{{{{name}}}}}")
}

fn near(text: &str) -> String {
    let shown: String = text.chars().take(24).collect();
    if shown.len() < text.len() { format!("{shown}…") } else { shown }
}

fn parse(text: &str) -> Result<Vec<Piece>, String> {
    let mut pieces = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(['{', '}']) {
        plain.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with('}') {
            return Err(format!("A }} has no {{ before it, near \"{}\"", near(tail)));
        }
        let double = tail.starts_with("{{");
        let (open, close) = if double { (2, "}}") } else { (1, "}") };
        let after = &tail[open..];
        let end = after.find(close).ok_or_else(|| format!("A {} is never closed, near \"{}\"", &tail[..open], near(tail)))?;
        let inner = &after[..end];
        if inner.contains(['{', '}']) {
            return Err(format!("Braces can't go inside braces, near \"{}\"", near(tail)));
        }
        let piece = if double { double_piece(inner)? } else { spin_piece(inner)? };
        if !plain.is_empty() {
            pieces.push(Piece::Text(std::mem::take(&mut plain)));
        }
        pieces.push(piece);
        rest = &after[end + close.len()..];
    }
    plain.push_str(rest);
    if !plain.is_empty() {
        pieces.push(Piece::Text(plain));
    }
    Ok(pieces)
}

fn double_piece(inner: &str) -> Result<Piece, String> {
    let inner = inner.trim();
    if inner.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("ai:")) {
        let instruction = inner[3..].trim();
        if instruction.is_empty() {
            return Err("{{ai:}} needs an instruction, like {{ai: one line about their latest project}}".into());
        }
        return Ok(Piece::Ai(instruction.to_string()));
    }
    let (name, fallback) = inner.split_once('|').map_or((inner, None), |(n, f)| (n.trim(), Some(f.trim())));
    let Some(name) = VARIABLES.iter().find(|v| **v == name).copied() else {
        let known = VARIABLES.iter().map(|v| braces(v)).collect::<Vec<_>>().join(", ");
        return Err(format!("{} isn't a variable. Use one of {known}", braces(inner)));
    };
    Ok(Piece::Var { name, fallback: fallback.filter(|f| !f.is_empty()).map(str::to_string) })
}

fn spin_piece(inner: &str) -> Result<Piece, String> {
    if !inner.contains('|') {
        return Err(format!(
            "{{{inner}}} needs a | between its options, like {{Hi|Hello}}. Variables take double braces, like {{{{first_name}}}}"
        ));
    }
    Ok(Piece::Spin(inner.split('|').map(|o| o.trim().to_string()).collect()))
}

/// FNV-1a with a final mix, so it's the same on every machine and Rust version: a lead's split and picks never change.
fn hash(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for b in *part {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^ (h >> 33)
}

/// Whether the lead is in the tenth that gets the AI email while My copy is on.
pub fn gets_ai_email(lead_id: Uuid) -> bool {
    hash(&[lead_id.as_bytes().as_slice(), b"my_copy".as_slice()]).is_multiple_of(AI_ONE_IN)
}

pub struct Sender {
    pub name: String,
    pub company: String,
    pub signature: String,
}

impl Sender {
    pub async fn load(db: &PgPool, project_id: Uuid) -> sqlx::Result<Self> {
        let (name, company, signature): (String, String, String) = sqlx::query_as(
            "select coalesce(s.sender_name, ''), coalesce(nullif(p.company->>'name', ''), p.name), coalesce(s.signature, '')
             from projects p left join project_settings s on s.project_id = p.id where p.id = $1",
        )
        .bind(project_id)
        .fetch_one(db)
        .await?;
        Ok(Self { name, company, signature })
    }

    fn first_name(&self) -> &str {
        self.name.split_whitespace().next().unwrap_or_default()
    }
}

/// What the variables are filled from for one lead.
pub struct Fields<'a> {
    pub lead: &'a Lead,
    pub company_city: &'a str,
    pub sender: &'a Sender,
}

impl Fields<'_> {
    fn value(&self, name: &str) -> &str {
        match name {
            "first_name" => &self.lead.first_name,
            "company" => &self.lead.company_name,
            "job_title" => &self.lead.title,
            "company_city" => self.company_city,
            "sender_first_name" => self.sender.first_name(),
            "sender_name" => &self.sender.name,
            "sender_company" => &self.sender.company,
            _ => "",
        }
    }
}

/// The cities of these leads' companies, by company id.
pub async fn company_cities(db: &PgPool, leads: &[Lead]) -> sqlx::Result<HashMap<Uuid, String>> {
    let ids: Vec<Uuid> = leads.iter().filter_map(|l| l.company_id).collect();
    let rows: Vec<(Uuid, String)> = sqlx::query_as("select id, city from companies where id = any($1) and city <> ''").bind(&ids).fetch_all(db).await?;
    Ok(rows.into_iter().collect())
}

/// The lead's company's city, else the lead's own.
pub fn company_city<'a>(lead: &'a Lead, cities: &'a HashMap<Uuid, String>) -> &'a str {
    lead.company_id.and_then(|id| cities.get(&id)).map_or(lead.city.as_str(), String::as_str)
}

/// A first email filled in for one lead, but for its {{ai: …}} lines.
#[derive(Debug)]
pub struct Filled {
    pub subject: String,
    /// The text around the AI lines: one more than there are lines.
    parts: Vec<String>,
}

impl Filled {
    /// The body with `lines` written in, one for each {{ai: …}} line.
    pub fn body(&self, lines: &[String]) -> String {
        let mut out = self.parts[0].clone();
        for (line, part) in lines.iter().zip(&self.parts[1..]) {
            out.push_str(line);
            out.push_str(part);
        }
        out
    }
}

#[derive(Debug)]
pub struct Template {
    subject: Vec<Piece>,
    body: Vec<Piece>,
    /// The text ends with the sender's name, so it goes out without the signature under it.
    signs_itself: bool,
}

impl Template {
    pub fn new(subject: &str, body: &str) -> Result<Self, String> {
        let subject = subject.trim();
        let subject = parse(if subject.is_empty() { DEFAULT_SUBJECT } else { subject }).map_err(|e| format!("Subject: {e}"))?;
        if subject.iter().any(|p| matches!(p, Piece::Ai(_))) {
            return Err("Subject: {{ai: …}} lines go in the body only".into());
        }
        let body = body.trim();
        let pieces = parse(body).map_err(|e| format!("Body: {e}"))?;
        let last = body.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default();
        let signs_itself = parse(last).is_ok_and(|line| {
            line.iter().any(|p| matches!(p, Piece::Var { .. }))
                && line.iter().all(|p| match p {
                    Piece::Var { name, .. } => name.starts_with("sender_"),
                    Piece::Text(t) => t.chars().all(|c| c.is_whitespace() || ",.-|&".contains(c)),
                    _ => false,
                })
        });
        Ok(Self { subject, body: pieces, signs_itself })
    }

    pub fn ai_lines(&self) -> Vec<&str> {
        self.body.iter().filter_map(|p| if let Piece::Ai(i) = p { Some(i.as_str()) } else { None }).collect()
    }

    /// The body as the writer sees it: variables and options as typed, and the lines it writes as [AI 1], [AI 2]…
    pub fn outline(&self) -> String {
        let mut n = 0;
        self.body
            .iter()
            .map(|p| match p {
                Piece::Text(t) => t.clone(),
                Piece::Var { name, .. } => braces(name),
                Piece::Spin(options) => format!("{{{}}}", options.join("|")),
                Piece::Ai(_) => {
                    n += 1;
                    format!("[AI {n}]")
                }
            })
            .collect()
    }

    /// Err with the variable this lead has no value for, when it has no fallback either.
    pub fn render(&self, fields: &Fields) -> Result<Filled, &'static str> {
        let subject = fill(&self.subject, b"subject", fields)?.concat();
        Ok(Filled { subject, parts: fill(&self.body, b"body", fields)? })
    }

    /// Signed like the AI email, unless the text already ends with the sender's name.
    pub fn sign(&self, body: &str, sender: &Sender) -> String {
        let body = body.trim_end();
        let sig = crate::agent::write::signature(&sender.signature, &sender.name);
        if self.signs_itself || sig.is_empty() { body.to_string() } else { format!("{body}\n\n{sig}") }
    }

    fn uses_sender_name_without_fallback(&self) -> Option<&'static str> {
        self.subject.iter().chain(&self.body).find_map(|p| match p {
            Piece::Var { name, fallback: None } if matches!(*name, "sender_name" | "sender_first_name") => Some(*name),
            _ => None,
        })
    }
}

fn fill(pieces: &[Piece], field: &[u8], fields: &Fields) -> Result<Vec<String>, &'static str> {
    let mut parts = vec![String::new()];
    for (i, piece) in pieces.iter().enumerate() {
        let last = parts.last_mut().expect("starts with one");
        match piece {
            Piece::Text(t) => last.push_str(t),
            Piece::Var { name, fallback } => {
                // A value with braces in it would read as a placeholder the template left unfilled, so it counts as missing.
                let value = fields.value(name).trim();
                let missing = value.is_empty() || value.contains(['{', '}']);
                last.push_str(if missing { fallback.as_deref().ok_or(*name)? } else { value });
            }
            Piece::Spin(options) => {
                let pick = hash(&[fields.lead.id.as_bytes().as_slice(), field, (i as u64).to_le_bytes().as_slice()]) % options.len() as u64;
                last.push_str(&options[pick as usize]);
            }
            Piece::Ai(_) => parts.push(String::new()),
        }
    }
    Ok(parts)
}

/// The checks a text passes before it's saved. `sender_name` is the project's, which {{sender_name}} is filled from.
pub fn check(copy: &FirstEmail, sender_name: &str) -> AppResult<()> {
    let bad = |m: String| Err(AppError::Unprocessable(m));
    if copy.subject.chars().count() > MAX_SUBJECT {
        return bad(format!("Keep the subject under {MAX_SUBJECT} characters"));
    }
    if copy.body.chars().count() > MAX_BODY {
        return bad(format!("Keep the body under {MAX_BODY} characters"));
    }
    if copy.mode == Mode::MyCopy && copy.body.trim().is_empty() {
        return bad("Write the email's body before turning My copy on".into());
    }
    let lower = copy.subject.trim().to_lowercase();
    if lower.starts_with("re:") || lower.starts_with("fwd:") || lower.starts_with("fw:") {
        return bad("The subject can't start with Re: or Fwd:. Follow-ups are threaded under it on their own".into());
    }
    let template = Template::new(&copy.subject, &copy.body).map_err(AppError::Unprocessable)?;
    if template.ai_lines().len() > MAX_AI_LINES {
        return bad(format!("Use up to {MAX_AI_LINES} {{{{ai: …}}}} lines"));
    }
    if copy.mode == Mode::MyCopy
        && sender_name.trim().is_empty()
        && let Some(name) = template.uses_sender_name_without_fallback()
    {
        return bad(format!(
            "{} needs a sender name. Set one under Sender on the Email templates page, or give it a fallback, like {}",
            braces(name),
            braces(&format!("{name}|Alex"))
        ));
    }
    Ok(())
}

/// The template each campaign of a project sends, by campaign. None while it sends the AI email.
pub struct Copies {
    project: Option<Template>,
    campaigns: HashMap<Uuid, Option<Template>>,
}

impl Copies {
    pub async fn load(db: &PgPool, project_id: Uuid) -> sqlx::Result<Self> {
        let project: Option<(String, String, String)> =
            sqlx::query_as("select my_copy_mode, my_copy_subject, my_copy_body from project_settings where project_id = $1")
                .bind(project_id)
                .fetch_optional(db)
                .await?;
        let campaigns: Vec<(Uuid, String, String, String)> = sqlx::query_as(
            "select id, my_copy_mode, my_copy_subject, my_copy_body from segments
             where project_id = $1 and my_copy_source = 'campaign' and my_copy_mode is not null",
        )
        .bind(project_id)
        .fetch_all(db)
        .await?;
        let on = |mode: &str, subject: &str, body: &str| (Mode::from_db(mode) == Mode::MyCopy).then(|| Template::new(subject, body).ok()).flatten();
        Ok(Self {
            project: project.and_then(|(m, s, b)| on(&m, &s, &b)),
            campaigns: campaigns.into_iter().map(|(id, m, s, b)| (id, on(&m, &s, &b))).collect(),
        })
    }

    pub fn get(&self, segment_id: Option<Uuid>) -> Option<&Template> {
        segment_id.and_then(|id| self.campaigns.get(&id)).unwrap_or(&self.project).as_ref()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use uuid::Uuid;

    use super::{Fields, FirstEmail, Mode, Sender, Template, check, gets_ai_email};
    use crate::error::AppError;
    use crate::models::Lead;

    fn lead(id: u128) -> Lead {
        Lead {
            id: Uuid::from_u128(id),
            first_name: "Ana".into(),
            company_name: "Delta Dental".into(),
            title: "Practice manager".into(),
            ..Default::default()
        }
    }

    fn sender() -> Sender {
        Sender { name: "Dana Reyes".into(), company: "Fieldnote".into(), signature: String::new() }
    }

    fn render(subject: &str, body: &str, lead: &Lead, city: &str) -> Result<(String, String), &'static str> {
        let (t, sender) = (Template::new(subject, body).unwrap(), sender());
        let f = t.render(&Fields { lead, company_city: city, sender: &sender })?;
        Ok((f.subject.clone(), t.sign(&f.body(&[]), &sender)))
    }

    fn refused(subject: &str, body: &str) -> String {
        match check(&FirstEmail { mode: Mode::MyCopy, subject: subject.into(), body: body.into() }, "Dana") {
            Err(AppError::Unprocessable(m)) => m,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn variables_fill_from_the_lead_company_and_sender() {
        let body = "Hi {{first_name}},\n\nHow does {{company}} in {{company_city}} hire a {{job_title}}?\n\n{{sender_name}}, {{sender_company}}";
        let (subject, body) = render("For {{first_name}} from {{sender_first_name}}", body, &lead(1), "Leeds").unwrap();
        assert_eq!(subject, "For Ana from Dana");
        assert_eq!(body, "Hi Ana,\n\nHow does Delta Dental in Leeds hire a Practice manager?\n\nDana Reyes, Fieldnote");
        // An empty subject goes out as the writer's default one.
        assert_eq!(render("", "Hi", &lead(1), "").unwrap().0, "Fieldnote x Delta Dental");
    }

    #[test]
    fn a_missing_variable_takes_its_fallback_or_skips_the_text() {
        assert_eq!(render("Hi", "In {{company_city|your city}}?", &lead(1), " ").unwrap().1, "In your city?\n\nDana Reyes");
        assert_eq!(render("Hi", "In {{company_city}}?", &lead(1), "").unwrap_err(), "company_city");
        // A value that looks like a placeholder would reach the lead as one, so it counts as missing.
        let odd = Lead { company_name: "{{company}} Ltd".into(), ..lead(1) };
        assert_eq!(render("Hi", "At {{company|your team}}?", &odd, "").unwrap().1, "At your team?\n\nDana Reyes");
        assert_eq!(render("Hi", "At {{company}}?", &odd, "").unwrap_err(), "company");
    }

    #[test]
    fn the_signature_goes_under_the_text_unless_it_signs_itself() {
        let signs = |body: &str| render("Hi", body, &lead(1), "").unwrap().1;
        assert_eq!(signs("Thanks,\n{{sender_first_name}}"), "Thanks,\nDana");
        assert_eq!(signs("Thanks,\n{{sender_first_name}} - {{sender_company}}\n\n"), "Thanks,\nDana - Fieldnote");
        assert_eq!(signs("Ask {{sender_first_name}} anything"), "Ask Dana anything\n\nDana Reyes");
    }

    #[test]
    fn spintax_picks_one_option_per_lead_and_keeps_it() {
        let text = "{Hi|Hello|Hey} {{first_name}}, {quick|short} one";
        let picks: Vec<String> = (0..200).map(|i| render(text, text, &lead(i), "").unwrap().0).collect();
        for (i, pick) in picks.iter().enumerate() {
            assert_eq!(*pick, render(text, text, &lead(i as u128), "").unwrap().0);
            assert!(!pick.contains(['{', '|', '}']), "{pick}");
        }
        let seen: HashSet<&String> = picks.iter().collect();
        assert_eq!(seen.len(), 6, "{seen:?}");
        // The subject and body pick on their own, so they don't always match.
        assert!((0..50).any(|i| { let (s, b) = render(text, text, &lead(i), "").unwrap(); !b.starts_with(&s) }));
    }

    #[test]
    fn ai_lines_are_left_for_the_writer() {
        let t = Template::new("Hi", "Hi {{first_name}},\n\n{{ai: one line about their latest hire}}\n\n{{ AI: a question }}\nBye").unwrap();
        assert_eq!(t.ai_lines(), vec!["one line about their latest hire", "a question"]);
        assert_eq!(t.outline(), "Hi {{first_name}},\n\n[AI 1]\n\n[AI 2]\nBye");
        let (lead, sender) = (lead(1), sender());
        let filled = t.render(&Fields { lead: &lead, company_city: "", sender: &sender }).unwrap();
        assert_eq!(filled.body(&["You hired a hygienist.".into(), "Busy?".into()]), "Hi Ana,\n\nYou hired a hygienist.\n\nBusy?\nBye");
    }

    #[test]
    fn a_text_with_a_mistake_is_refused_with_what_to_fix() {
        assert!(refused("Hi", "Hi {{frist_name}}").contains("{{frist_name}} isn't a variable. Use one of {{first_name}}"));
        assert!(refused("Hi", "Hi {{first_name}").contains("never closed"));
        assert!(refused("Hi", "Hi first_name}}").contains("has no { before it"));
        assert!(refused("Hi", "{Hi|{{first_name}}}").contains("inside braces"));
        assert!(refused("Hi", "Hi {first_name}").contains("needs a | between its options"));
        assert!(refused("{{ai: a hook}}", "Hi").starts_with("Subject:"));
        assert!(refused("Re: hi", "Hi").contains("Re:"));
        assert!(refused("Hi", "{{ai:}}").contains("needs an instruction"));
        assert!(refused("Hi", &"{{ai: a}} ".repeat(6)).contains("up to 5"));
        assert!(refused("Hi", " \n ").contains("Write the email's body"));
        assert!(refused("Hi", &"a".repeat(3001)).contains("3000"));
        // An empty body is fine while the AI email goes out, and so are the texts above it isn't sending yet but would refuse.
        assert!(check(&FirstEmail { mode: Mode::Instructions, subject: String::new(), body: String::new() }, "").is_ok());
        let needs_name = FirstEmail { mode: Mode::MyCopy, subject: "Hi".into(), body: "Thanks,\n{{sender_first_name}}".into() };
        assert!(matches!(check(&needs_name, " "), Err(AppError::Unprocessable(m)) if m.contains("needs a sender name")));
        assert!(check(&FirstEmail { body: "Thanks,\n{{sender_first_name|Dana}}".into(), ..needs_name }, "").is_ok());
    }

    #[test]
    fn one_lead_in_ten_gets_the_ai_email_and_always_the_same_ones() {
        let ids: Vec<Uuid> = (0..20_000).map(|_| Uuid::new_v4()).collect();
        let ai = ids.iter().filter(|id| gets_ai_email(**id)).count();
        assert!((1_700..=2_300).contains(&ai), "{ai}");
        assert!(ids.iter().all(|id| gets_ai_email(*id) == gets_ai_email(*id)));
        // Pinned, so a change to the split that would move live leads between the two sides fails here.
        let pinned: Vec<u128> = (0..40).filter(|i| gets_ai_email(Uuid::from_u128(*i))).collect();
        assert_eq!(pinned, vec![6, 15, 27, 38]);
    }
}
