//! Copies hot and booked leads into the CRMs a project connected, each with a note saying what happened, so the sales
//! team finds them where they work. Tokens are kept encrypted like mailbox passwords and never sent back to the browser.

mod affinity;
mod attio;
mod brevo;
mod capsule;
mod close;
mod copper;
mod espocrm;
mod folk;
mod freshsales;
mod hubspot;
mod insightly;
mod lacrm;
mod notion;
mod odoo;
mod pipedrive;
mod salesflare;
mod twenty;
mod zendesk_sell;

use std::time::Duration;

use reqwest::Url;

use uuid::Uuid;

use crate::crypto;
use crate::hooks::{self, Alert};
use crate::models::Lead;
use crate::state::AppState;

#[derive(Clone, Copy, Debug)]
pub enum Crm {
    HubSpot,
    Pipedrive,
    Attio,
    Close,
    Folk,
    Capsule,
    Brevo,
    ZendeskSell,
    Lacrm,
    Salesflare,
    Affinity,
    Freshsales,
    Copper,
    Insightly,
    Twenty,
    TwentySelf,
    Espo,
    Odoo,
    Notion,
}

impl Crm {
    /// Every CRM, in the order the integrations page lists them. Keep in sync with CRM_NAMES in frontend/src/lib/types.ts.
    pub const ALL: [Crm; 19] = [
        Crm::HubSpot,
        Crm::Pipedrive,
        Crm::Attio,
        Crm::Close,
        Crm::Folk,
        Crm::Capsule,
        Crm::Brevo,
        Crm::ZendeskSell,
        Crm::Lacrm,
        Crm::Salesflare,
        Crm::Affinity,
        Crm::Freshsales,
        Crm::Copper,
        Crm::Insightly,
        Crm::Twenty,
        Crm::TwentySelf,
        Crm::Espo,
        Crm::Odoo,
        Crm::Notion,
    ];

    pub fn parse(name: &str) -> Option<Crm> {
        Crm::ALL.into_iter().find(|crm| crm.name() == name)
    }

    /// Also the prefix of its project_settings columns: {name}_token_enc, {name}_connected, {name}_error, {name}_failed_at.
    pub fn name(self) -> &'static str {
        match self {
            Crm::HubSpot => "hubspot",
            Crm::Pipedrive => "pipedrive",
            Crm::Attio => "attio",
            Crm::Close => "close",
            Crm::Folk => "folk",
            Crm::Capsule => "capsule",
            Crm::Brevo => "brevo",
            Crm::ZendeskSell => "zendesk_sell",
            Crm::Lacrm => "lacrm",
            Crm::Salesflare => "salesflare",
            Crm::Affinity => "affinity",
            Crm::Freshsales => "freshsales",
            Crm::Copper => "copper",
            Crm::Insightly => "insightly",
            Crm::Twenty => "twenty",
            Crm::TwentySelf => "twenty_self",
            Crm::Espo => "espocrm",
            Crm::Odoo => "odoo",
            Crm::Notion => "notion",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Crm::HubSpot => "HubSpot",
            Crm::Pipedrive => "Pipedrive",
            Crm::Attio => "Attio",
            Crm::Close => "Close",
            Crm::Folk => "folk",
            Crm::Capsule => "Capsule",
            Crm::Brevo => "Brevo",
            Crm::ZendeskSell => "Zendesk Sell",
            Crm::Lacrm => "Less Annoying CRM",
            Crm::Salesflare => "Salesflare",
            Crm::Affinity => "Affinity",
            Crm::Freshsales => "Freshsales",
            Crm::Copper => "Copper",
            Crm::Insightly => "Insightly",
            Crm::Twenty => "Twenty",
            Crm::TwentySelf => "Twenty (self-hosted)",
            Crm::Espo => "EspoCRM",
            Crm::Odoo => "Odoo",
            Crm::Notion => "Notion",
        }
    }

    /// What the CRM needs besides the token to find the account, asked for on its own. It's saved in the token, a line
    /// before it, so syncing needs nothing new.
    pub fn account(self) -> Option<&'static str> {
        match self {
            Crm::Freshsales => Some("bundle alias"),
            Crm::Copper => Some("email"),
            Crm::Insightly => Some("API URL"),
            Crm::TwentySelf | Crm::Espo | Crm::Odoo => Some("site URL"),
            Crm::Notion => Some("database link"),
            _ => None,
        }
    }

    /// Checks the CRM accepts the token, so a typo shows up when connecting and not on the first hot lead.
    pub async fn check(self, token: &str) -> anyhow::Result<()> {
        match self {
            Crm::HubSpot => hubspot::check(token).await,
            Crm::Pipedrive => pipedrive::check(token).await,
            Crm::Attio => attio::check(token).await,
            Crm::Close => close::check(token).await,
            Crm::Folk => folk::check(token).await,
            Crm::Capsule => capsule::check(token).await,
            Crm::Brevo => brevo::check(token).await,
            Crm::ZendeskSell => zendesk_sell::check(token).await,
            Crm::Lacrm => lacrm::check(token).await,
            Crm::Salesflare => salesflare::check(token).await,
            Crm::Affinity => affinity::check(token).await,
            Crm::Freshsales => freshsales::check(token).await,
            Crm::Copper => copper::check(token).await,
            Crm::Insightly => insightly::check(token).await,
            Crm::Twenty => twenty::check(token, false).await,
            Crm::TwentySelf => twenty::check(token, true).await,
            Crm::Espo => espocrm::check(token).await,
            Crm::Odoo => odoo::check(token).await,
            Crm::Notion => notion::check(token).await,
        }
    }

    async fn push(self, token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
        match self {
            Crm::HubSpot => hubspot::push(token, email, lead, note).await,
            Crm::Pipedrive => pipedrive::push(token, email, lead, note).await,
            Crm::Attio => attio::push(token, email, lead, note).await,
            Crm::Close => close::push(token, email, lead, note).await,
            Crm::Folk => folk::push(token, email, lead, note).await,
            Crm::Capsule => capsule::push(token, email, lead, note).await,
            Crm::Brevo => brevo::push(token, email, lead, note).await,
            Crm::ZendeskSell => zendesk_sell::push(token, email, lead, note).await,
            Crm::Lacrm => lacrm::push(token, email, lead, note).await,
            Crm::Salesflare => salesflare::push(token, email, lead, note).await,
            Crm::Affinity => affinity::push(token, email, lead, note).await,
            Crm::Freshsales => freshsales::push(token, email, lead, note).await,
            Crm::Copper => copper::push(token, email, lead, note).await,
            Crm::Insightly => insightly::push(token, email, lead, note).await,
            Crm::Twenty => twenty::push(token, false, email, lead, note).await,
            Crm::TwentySelf => twenty::push(token, true, email, lead, note).await,
            Crm::Espo => espocrm::push(token, email, lead, note).await,
            Crm::Odoo => odoo::push(token, email, lead, note).await,
            Crm::Notion => notion::push(token, email, lead, note).await,
        }
    }
}

/// Adds or updates the lead in every connected CRM and puts `note` on it. Failures are logged and shown on the
/// integrations page, they never fail the caller.
pub async fn sync(state: &AppState, project_id: Uuid, lead: &Lead, note: &str) {
    let Some(email) = lead.email.as_deref() else { return };
    // The column names come from Crm::name, never from input.
    let columns: Vec<String> = Crm::ALL.iter().map(|crm| format!("{}_token_enc", crm.name())).collect();
    let sql = format!("select array[{}] from project_settings where project_id = $1", columns.join(", "));
    let tokens: Option<Vec<String>> = sqlx::query_scalar(&sql).bind(project_id).fetch_optional(&state.db).await.ok().flatten();
    let Some(tokens) = tokens else { return };
    for (crm, enc) in Crm::ALL.into_iter().zip(tokens) {
        if enc.is_empty() {
            continue;
        }
        let result = match crypto::decrypt(&state.cfg.app_secret, &enc) {
            Ok(token) => crm.push(&token, email, lead, note).await,
            Err(e) => Err(e),
        };
        if let Err(e) = &result {
            tracing::warn!(error = %e, crm = crm.name(), "crm sync failed");
        }
        hooks::record(&state.db, project_id, Alert::Crm(crm), &enc, &result).await;
    }
}

fn client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder().timeout(Duration::from_secs(20)).build()
}

/// Sends the request and turns a failure into a plain message: a 401 says the key was turned down, and anything else gives
/// the CRM's own `message` or `error`, or the status. reqwest's own errors quote the URL, which can hold the lead's email
/// and reads like noise on the integrations page.
async fn send(request: reqwest::RequestBuilder) -> anyhow::Result<reqwest::Response> {
    let res = request.send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key or token is wrong or was revoked");
    let body: serde_json::Value = res.json().await.unwrap_or_default();
    let said = body["message"].as_str().or(body["error"].as_str()).filter(|s| !s.is_empty());
    anyhow::bail!("{}", said.map_or_else(|| status.to_string(), str::to_string))
}

/// "Sarah van Dijk" is ("Sarah", "van Dijk"). A one-word name has no last name.
fn split_name(full: &str) -> (&str, &str) {
    let full = full.trim();
    full.split_once(' ').map(|(f, l)| (f, l.trim())).unwrap_or((full, ""))
}

/// HubSpot, Pipedrive, Brevo, Insightly and Odoo render notes as HTML, so the reply is escaped and its line breaks kept.
fn note_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\n', "<br>")
}

/// A self-hosted CRM's root, whatever page of it was pasted, like https://acme.odoo.com/odoo/contacts. `https://` is
/// assumed.
fn site_root(site: &str) -> Option<Url> {
    let site = site.trim();
    let site = if site.contains("://") { site.to_string() } else { format!("https://{site}") };
    let url = Url::parse(&site).ok().filter(|u| u.host_str().is_some())?;
    Url::parse(&url.origin().ascii_serialization()).ok()
}

/// Twenty and EspoCRM read a note as markdown, so a reply's `#`, `*` or `1.` would become a heading, bold or a list. Every
/// punctuation mark is escaped, which markdown shows as written, except in a link, which they make clickable only as
/// written. Line breaks stay line breaks.
fn plain_markdown(text: &str) -> String {
    let word = |word: &str| {
        if word.starts_with("https://") || word.starts_with("http://") {
            return word.to_string();
        }
        word.chars().fold(String::new(), |mut out, c| {
            if c.is_ascii_punctuation() {
                out.push('\\');
            }
            out.push(c);
            out
        })
    };
    text.split('\n').map(|line| line.split(' ').map(word).collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::{note_html, plain_markdown, site_root, split_name};

    #[test]
    fn names_split_at_the_first_space() {
        assert_eq!(split_name(" Sarah van Dijk "), ("Sarah", "van Dijk"));
        assert_eq!(split_name("Cher"), ("Cher", ""));
    }

    #[test]
    fn notes_keep_lines_and_escape_html() {
        assert_eq!(note_html("Tuesday works <3\nSarah & co"), "Tuesday works &lt;3<br>Sarah &amp; co");
    }

    #[test]
    fn replies_stay_plain_text() {
        assert_eq!(plain_markdown("# Hi *you*\n1. ok"), "\\# Hi \\*you\\*\n1\\. ok");
        assert_eq!(plain_markdown("Booked.\nhttps://app/p/1/inbox?lead=a_b"), "Booked\\.\nhttps://app/p/1/inbox?lead=a_b");
    }

    #[test]
    fn any_page_of_the_site_gives_its_root() {
        let root = |site: &str| site_root(site).map(|u| u.to_string()).unwrap_or_default();
        assert_eq!(root("https://acme.odoo.com/odoo/contacts"), "https://acme.odoo.com/");
        assert_eq!(root(" erp.acme.com "), "https://erp.acme.com/");
        assert_eq!(root("http://10.0.0.5:8069/web#action=1"), "http://10.0.0.5:8069/");
        assert_eq!(root("not a url"), "");
    }
}
