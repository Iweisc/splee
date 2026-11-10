//! What Splee's two uses of Baserow share: importing leads from a table (routes/util_baserow.rs) and adding hot and booked
//! leads to one (crm/baserow.rs). Baserow runs at baserow.io or on the customer's own server, so the API is at the link's
//! origin (api.baserow.io for baserow.io), and requests only go to a public address (see hooks::public_client).

use reqwest::Url;

pub struct Ids {
    /// The API's root, like https://api.baserow.io/api/.
    pub api: Url,
    pub table: String,
    pub view: Option<String>,
}

impl Ids {
    /// https://baserow.io/database/{database}/table/{table}/{view}, as the address bar shows a table or one of its views,
    /// on baserow.io or a self-hosted server.
    pub fn parse(link: &str) -> Option<Ids> {
        let link = link.trim();
        let url = Url::parse(if link.contains("://") { link.to_string() } else { format!("https://{link}") }.as_str()).ok()?;
        let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        let number = |i: usize| segments.get(i).filter(|s| s.bytes().all(|b| b.is_ascii_digit())).map(|s| s.to_string());
        let at = segments.iter().position(|s| *s == "table")?;
        let (table, view) = (number(at + 1)?, number(at + 2));
        let origin = match url.host_str()? {
            "baserow.io" | "www.baserow.io" => "https://api.baserow.io".to_string(),
            _ => url.origin().ascii_serialization(),
        };
        Some(Ids { api: Url::parse(&format!("{origin}/api/")).ok()?, table, view })
    }
}

#[cfg(test)]
mod tests {
    use super::Ids;

    #[test]
    fn a_table_link_on_baserow_or_a_server_is_read() {
        let ids = Ids::parse("https://baserow.io/database/12/table/345/678").unwrap();
        assert_eq!((ids.api.as_str(), ids.table.as_str(), ids.view.as_deref()), ("https://api.baserow.io/api/", "345", Some("678")));
        let own = Ids::parse("rows.acme.com/database/1/table/2?row=5").unwrap();
        assert_eq!((own.api.as_str(), own.table.as_str(), own.view), ("https://rows.acme.com/api/", "2", None));
        assert!(Ids::parse("https://baserow.io/database/12").is_none());
        assert!(Ids::parse("https://baserow.io/database/12/table/abc").is_none());
    }
}
