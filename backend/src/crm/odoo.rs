//! Odoo: the lead becomes a contact, found by exact email or created, and gets a note in its chatter. It uses Odoo 19's
//! JSON-2 API, which takes an API key alone. Odoo runs on the customer's own site, so the saved token is the site's address
//! and the key, a line apart (see Crm::account), and requests only go to a public address (see hooks::public_client).

use anyhow::Context;
use serde_json::{Value, json};

use super::{note_html, site_root};
use crate::hooks;
use crate::models::Lead;

/// Checks the site and key reach an Odoo user, before they're saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(token, "res.users/context_get", json!({})).await.map(|_| ())
}

/// A failure is `{name, message}`, like an AccessError saying which model the key's user can't touch.
async fn call(token: &str, path: &str, body: Value) -> anyhow::Result<Value> {
    let (site, key) = token.split_once('\n').context("reconnect Odoo with its site URL")?;
    let root = site_root(site).context("the site URL should look like https://acme.odoo.com")?;
    let url = root.join(&format!("json/2/{path}"))?;
    let request = hooks::public_client(&url).await?.post(url).bearer_auth(key).json(&body);
    let res = match request.send().await {
        Err(e) if e.is_connect() => anyhow::bail!("couldn't reach {}, so check the site URL", root.host_str().unwrap_or_default()),
        res => res.map_err(reqwest::Error::without_url)?,
    };
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    anyhow::ensure!(!status.is_redirection(), "the site redirected, so paste the address Odoo opens at, with https://");
    let body: Value = res.json().await.unwrap_or_default();
    // An Odoo before 19 has no JSON-2 API, so it answers 404 without Odoo's error body.
    anyhow::ensure!(status != reqwest::StatusCode::NOT_FOUND || body["message"].is_string(), "the site has no Odoo 19 API, so check the site URL");
    anyhow::ensure!(status.is_success(), "{}", body["message"].as_str().map_or_else(|| status.to_string(), str::to_string));
    Ok(body)
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let email = email.to_lowercase();
    // =ilike finds the address whatever its case. It reads _ and % as wildcards, so only an exact match is used.
    let search = json!({"domain": [["email", "=ilike", email]], "fields": ["id", "email"], "limit": 20});
    let found = call(token, "res.partner/search_read", search).await?;
    let partner_id = match matching_partner(&found, &email) {
        Some(id) => id,
        None => {
            // Odoo needs a name, so a lead without a name is named after their address.
            let name = if lead.full_name.trim().is_empty() { email.as_str() } else { lead.full_name.trim() };
            let contact = json!({"name": name, "email": email, "function": lead.title.trim()});
            let created = call(token, "res.partner/create", json!({"vals_list": [contact]})).await?;
            created[0].as_i64().context("Odoo didn't return the contact")?
        }
    };
    let post = json!({"ids": [partner_id], "body": linked_note(note), "body_is_html": true, "message_type": "comment", "subtype_xmlid": "mail.mt_note"});
    call(token, "res.partner/message_post", post).await?;
    Ok(())
}

/// The note as HTML, like note_html, with its links made clickable, since Odoo's chatter shows a bare URL as text.
fn linked_note(note: &str) -> String {
    let word = |w: &str| {
        let text = note_html(w);
        if w.starts_with("https://") || w.starts_with("http://") { format!("<a href=\"{}\">{text}</a>", text.replace('"', "&quot;")) } else { text }
    };
    note.split('\n').map(|line| line.split(' ').map(word).collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("<br>")
}

/// Only a contact whose address is exactly this one, whatever the search matched.
fn matching_partner(found: &Value, email: &str) -> Option<i64> {
    found.as_array()?.iter().find(|p| p["email"].as_str().is_some_and(|e| e.trim().eq_ignore_ascii_case(email)))?["id"].as_i64()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{linked_note, matching_partner};

    #[test]
    fn notes_escape_the_reply_and_link_urls() {
        assert_eq!(
            linked_note("Hot lead: <b>yes</b> & co\nhttps://app/p/1/inbox?lead=a&x=1"),
            "Hot lead: &lt;b&gt;yes&lt;/b&gt; &amp; co<br><a href=\"https://app/p/1/inbox?lead=a&amp;x=1\">https://app/p/1/inbox?lead=a&amp;x=1</a>"
        );
    }

    #[test]
    fn only_a_contact_with_the_exact_address_matches() {
        let found = json!([{"id": 6, "email": "sarah_x@acme.com"}, {"id": 7, "email": "Sarah.X@Acme.com"}, {"id": 8, "email": false}]);
        assert_eq!(matching_partner(&found, "sarah.x@acme.com"), Some(7));
        assert_eq!(matching_partner(&found, "sarah@acme.com"), None);
    }
}
