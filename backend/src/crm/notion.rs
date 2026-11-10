//! Notion: the lead becomes a row in a database, found by its email property or created, and the note goes at the end of
//! the row's page. The saved token is the database's link and an integration's secret, a line apart (see Crm::account).
//! Only the database's id is taken from the link, so requests always go to Notion.

use anyhow::Context;
use reqwest::Method;
use serde_json::{Value, json};

use super::client;
use crate::models::Lead;
use crate::notion::{API, VERSION, database_id};

/// Checks the integration can read the database and that it has a column for the email, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    Database::open(token).await.map(|_| ())
}

/// The database's first data source, with the names of its title and email columns, which every database names its own way.
struct Database<'a> {
    client: reqwest::Client,
    secret: &'a str,
    source: String,
    title: String,
    email: String,
}

impl<'a> Database<'a> {
    async fn open(token: &'a str) -> anyhow::Result<Database<'a>> {
        let (link, secret) = token.split_once('\n').context("reconnect Notion with the database's link")?;
        let id = database_id(link).context("the database link should look like https://www.notion.so/Leads-1a2b…")?;
        let client = client()?;
        let database = call(&client, secret, Method::GET, &format!("databases/{id}"), None).await?;
        let source = database["data_sources"][0]["id"].as_str().context("the database has no table")?.to_string();
        let schema = call(&client, secret, Method::GET, &format!("data_sources/{source}"), None).await?;
        let (title, email) = columns(&schema["properties"]);
        let title = title.context("the database has no title column")?;
        let email = email.context("the database needs a column with the Email type for the lead's address")?;
        Ok(Database { client, secret, source, title, email })
    }

    async fn call(&self, method: Method, path: &str, body: Value) -> anyhow::Result<Value> {
        call(&self.client, self.secret, method, path, Some(body)).await
    }
}

/// The title column, and the email column, the one called Email if there are several.
fn columns(properties: &Value) -> (Option<String>, Option<String>) {
    let Some(properties) = properties.as_object() else { return (None, None) };
    let of = |kind: &'static str| properties.iter().filter(move |(_, p)| p["type"] == kind).map(|(name, _)| name.clone());
    let email = of("email").find(|name| name.eq_ignore_ascii_case("email")).or_else(|| of("email").next());
    (of("title").next(), email)
}

/// A failure is `{code, message}`. A database the integration wasn't added to is a 404.
async fn call(client: &reqwest::Client, secret: &str, method: Method, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
    let mut request = client.request(method, format!("{API}/{path}")).bearer_auth(secret).header("Notion-Version", VERSION);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let res = request.send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the integration secret was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status != reqwest::StatusCode::NOT_FOUND, "the integration can't see the database, so add it under ••• → Connections");
    anyhow::ensure!(status.is_success(), "{}", body["message"].as_str().map_or_else(|| status.to_string(), str::to_string));
    Ok(body)
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let db = Database::open(token).await?;
    let email = email.to_lowercase();
    let query = json!({"filter": {"property": db.email, "email": {"equals": email}}, "page_size": 1});
    let found = db.call(Method::POST, &format!("data_sources/{}/query", db.source), query).await?;
    let page_id = match found["results"][0]["id"].as_str() {
        Some(id) => id.to_string(),
        None => {
            let name = if lead.full_name.trim().is_empty() { email.as_str() } else { lead.full_name.trim() };
            let mut properties = serde_json::Map::new();
            properties.insert(db.title.clone(), json!({"title": [{"text": {"content": name}}]}));
            properties.insert(db.email.clone(), json!({"email": email}));
            let page = json!({"parent": {"data_source_id": db.source}, "properties": properties});
            let created = db.call(Method::POST, "pages", page).await?;
            created["id"].as_str().context("Notion didn't return the page")?.to_string()
        }
    };
    db.call(Method::PATCH, &format!("blocks/{page_id}/children"), json!({"children": blocks(note)})).await?;
    Ok(())
}

/// The note as blocks: its first line a heading, so notes added over time stand apart, and a paragraph for each other line,
/// a link when it's a URL. Notion takes up to 2,000 characters in one piece of text and 100 blocks at a time.
fn blocks(note: &str) -> Vec<Value> {
    let text = |line: &str| {
        let link = (line.starts_with("https://") || line.starts_with("http://")) && !line.contains(' ');
        let chars: Vec<char> = line.chars().collect();
        let pieces = chars.chunks(2000).map(|c| c.iter().collect::<String>());
        let items: Vec<Value> = pieces
            .map(|content| if link { json!({"text": {"content": content, "link": {"url": line}}}) } else { json!({"text": {"content": content}}) })
            .collect();
        json!({"rich_text": items})
    };
    let mut lines = note.lines().map(str::trim_end).filter(|l| !l.is_empty());
    let mut blocks: Vec<Value> = lines.next().map(|first| json!({"type": "heading_3", "heading_3": text(first)})).into_iter().collect();
    blocks.extend(lines.take(99).map(|line| json!({"type": "paragraph", "paragraph": text(line)})));
    blocks
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{blocks, columns};

    #[test]
    fn the_email_column_is_found_whatever_its_name() {
        let schema = json!({"Lead": {"type": "title"}, "Work email": {"type": "email"}, "Notes": {"type": "rich_text"}});
        assert_eq!(columns(&schema), (Some("Lead".into()), Some("Work email".into())));
        let two = json!({"Name": {"type": "title"}, "Assistant": {"type": "email"}, "Email": {"type": "email"}});
        assert_eq!(columns(&two).1.as_deref(), Some("Email"));
        assert_eq!(columns(&json!({"Name": {"type": "title"}})).1, None);
    }

    #[test]
    fn notes_become_a_heading_and_paragraphs_with_links() {
        let note = blocks("Hot lead: Sarah replied\n\n> Sounds good, send times\nhttps://app/p/1/inbox?lead=a");
        assert_eq!(note.len(), 3);
        assert_eq!(note[0]["heading_3"]["rich_text"][0]["text"]["content"], "Hot lead: Sarah replied");
        assert_eq!(note[1]["paragraph"]["rich_text"][0]["text"], json!({"content": "> Sounds good, send times"}));
        assert_eq!(note[2]["paragraph"]["rich_text"][0]["text"]["link"]["url"], "https://app/p/1/inbox?lead=a");
        let long = blocks(&format!("Hot\n{}", "a".repeat(2500)));
        assert_eq!(long[1]["paragraph"]["rich_text"].as_array().map(Vec::len), Some(2));
    }
}
