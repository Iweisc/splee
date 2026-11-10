//! Copies hot and booked leads into the CRMs a project connected, each with a note saying what happened, so the sales
//! team finds them where they work. Tokens are kept encrypted like mailbox passwords and never sent back to the browser.

mod affinity;
mod airtable;
mod attio;
mod baserow;
mod brevo;
mod capsule;
mod close;
mod copper;
mod espocrm;
mod folk;
mod freshsales;
mod grist;
mod hubspot;
mod insightly;
mod lacrm;
mod nocodb;
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

/// Declares every CRM once: its variant, its module (with `check` and `push`), its name and its label.
macro_rules! crms {
    ($($variant:ident: $module:ident, $name:literal, $label:literal;)+) => {
        #[derive(Clone, Copy, Debug)]
        pub enum Crm {
            $($variant,)+
        }

        impl Crm {
            /// Every CRM, in the order the integrations page lists them. Keep in sync with CRM_NAMES in
            /// frontend/src/lib/types.ts.
            pub const ALL: &[Crm] = &[$(Crm::$variant,)+];

            /// Also the prefix of its project_settings columns: {name}_token_enc, {name}_connected, {name}_error,
            /// {name}_failed_at.
            pub fn name(self) -> &'static str {
                match self {
                    $(Crm::$variant => $name,)+
                }
            }

            pub fn label(self) -> &'static str {
                match self {
                    $(Crm::$variant => $label,)+
                }
            }

            /// Checks the CRM accepts the token, so a typo shows up when connecting and not on the first hot lead.
            pub async fn check(self, token: &str) -> anyhow::Result<()> {
                match self {
                    $(Crm::$variant => $module::check(token).await,)+
                }
            }

            async fn push(self, token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
                match self {
                    $(Crm::$variant => $module::push(token, email, lead, note).await,)+
                }
            }
        }
    };
}

crms! {
    HubSpot: hubspot, "hubspot", "HubSpot";
    Pipedrive: pipedrive, "pipedrive", "Pipedrive";
    Attio: attio, "attio", "Attio";
    Close: close, "close", "Close";
    Folk: folk, "folk", "folk";
    Capsule: capsule, "capsule", "Capsule";
    Brevo: brevo, "brevo", "Brevo";
    ZendeskSell: zendesk_sell, "zendesk_sell", "Zendesk Sell";
    Lacrm: lacrm, "lacrm", "Less Annoying CRM";
    Salesflare: salesflare, "salesflare", "Salesflare";
    Affinity: affinity, "affinity", "Affinity";
    Freshsales: freshsales, "freshsales", "Freshsales";
    Copper: copper, "copper", "Copper";
    Insightly: insightly, "insightly", "Insightly";
    Twenty: twenty_cloud, "twenty", "Twenty";
    TwentySelf: twenty_self, "twenty_self", "Twenty (self-hosted)";
    Espo: espocrm, "espocrm", "EspoCRM";
    Odoo: odoo, "odoo", "Odoo";
    Notion: notion, "notion", "Notion";
    Airtable: airtable, "airtable", "Airtable";
    Baserow: baserow, "baserow", "Baserow";
    NocoDb: nocodb, "nocodb", "NocoDB";
    Grist: grist, "grist", "Grist";
}

/// Twenty's cloud and a self-hosted Twenty share one client, which is told which it is.
mod twenty_cloud {
    use crate::models::Lead;

    pub async fn check(token: &str) -> anyhow::Result<()> {
        super::twenty::check(token, false).await
    }

    pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
        super::twenty::push(token, false, email, lead, note).await
    }
}

mod twenty_self {
    use crate::models::Lead;

    pub async fn check(token: &str) -> anyhow::Result<()> {
        super::twenty::check(token, true).await
    }

    pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
        super::twenty::push(token, true, email, lead, note).await
    }
}

impl Crm {
    pub fn parse(name: &str) -> Option<Crm> {
        Crm::ALL.iter().copied().find(|crm| crm.name() == name)
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
            Crm::Airtable | Crm::Baserow | Crm::NocoDb => Some("table link"),
            Crm::Grist => Some("page link"),
            _ => None,
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
    for (crm, enc) in Crm::ALL.iter().copied().zip(tokens) {
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
