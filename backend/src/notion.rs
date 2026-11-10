//! What Splee's two uses of Notion share: importing leads from a database (routes/util_notion.rs) and adding hot and
//! booked leads to one (crm/notion.rs).

use reqwest::Url;

pub const API: &str = "https://api.notion.com/v1";
pub const VERSION: &str = "2026-03-11";

/// The id at the end of a database's link, with or without dashes: https://www.notion.so/acme/Leads-{32 hex}?v=…, as the
/// address bar shows it, or the same on notion.com or a notion.site page.
pub fn database_id(link: &str) -> Option<String> {
    let url = Url::parse(link.trim()).ok()?;
    let host = url.host_str()?;
    let notion = ["notion.so", "notion.com", "notion.site"].iter().any(|d| host == *d || host.ends_with(&format!(".{d}")));
    if !notion || url.scheme() != "https" {
        return None;
    }
    let last: String = url.path_segments()?.next_back()?.chars().filter(|c| *c != '-').collect();
    let id = last.get(last.len().checked_sub(32)?..)?;
    id.bytes().all(|b| b.is_ascii_hexdigit()).then(|| id.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::database_id;

    #[test]
    fn only_a_notion_database_link_is_read() {
        let id = "1a2b3c4d5e6f40718293a4b5c6d7e8f9";
        assert_eq!(database_id(&format!("https://www.notion.so/acme/Leads-{id}?v=0123456789abcdef0123456789abcdef")).as_deref(), Some(id));
        assert_eq!(database_id(&format!("https://www.notion.so/{}", id.to_uppercase())).as_deref(), Some(id));
        assert_eq!(database_id("https://acme.notion.site/1a2b3c4d-5e6f-4071-8293-a4b5c6d7e8f9").as_deref(), Some(id));
        assert_eq!(database_id(&format!("https://www.notion.com/acme/{id}")).as_deref(), Some(id));
        assert!(database_id(&format!("https://evil.example/{id}")).is_none());
        assert!(database_id(&format!("https://notion.so.evil.example/{id}")).is_none());
        assert!(database_id("https://www.notion.so/acme/Leads-1a2b3c").is_none());
    }
}
