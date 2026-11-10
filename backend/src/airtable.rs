//! What Splee's two uses of Airtable share: importing leads from a table (routes/util_airtable.rs) and adding hot and
//! booked leads to one (crm/airtable.rs). Only the ids are taken from a table's link, so requests always go to Airtable.

use reqwest::Url;

pub const API: &str = "https://api.airtable.com/v0";

pub struct Ids {
    pub base: String,
    pub table: String,
    pub view: Option<String>,
}

impl Ids {
    /// https://airtable.com/{app…}/{tbl…}/{viw…}?blocks=hide, as the address bar shows a table or one of its views.
    pub fn parse(link: &str) -> Option<Ids> {
        let url = Url::parse(link.trim()).ok()?;
        if !matches!(url.host_str(), Some("airtable.com" | "www.airtable.com")) {
            return None;
        }
        let segments: Vec<&str> = url.path_segments()?.collect();
        let id = |prefix: &str| segments.iter().find(|s| is_id(s, prefix)).map(|s| s.to_string());
        Some(Ids { base: id("app")?, table: id("tbl")?, view: id("viw") })
    }
}

/// Airtable's ids are a three-letter prefix and 14 letters or digits, like appXXXXXXXXXXXXXX.
fn is_id(segment: &str, prefix: &str) -> bool {
    segment.len() == 17 && segment.starts_with(prefix) && segment.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::Ids;

    #[test]
    fn only_an_airtable_table_link_is_read() {
        let ids = Ids::parse("https://airtable.com/appAbCdEfGhIjKlMn/tblAbCdEfGhIjKlMn/viwAbCdEfGhIjKlMn?blocks=hide").unwrap();
        assert_eq!((ids.base.as_str(), ids.table.as_str(), ids.view.as_deref()), ("appAbCdEfGhIjKlMn", "tblAbCdEfGhIjKlMn", Some("viwAbCdEfGhIjKlMn")));
        assert!(Ids::parse("https://airtable.com/appAbCdEfGhIjKlMn/tblAbCdEfGhIjKlMn").unwrap().view.is_none());
        assert!(Ids::parse("https://airtable.com/appAbCdEfGhIjKlMn").is_none());
        assert!(Ids::parse("https://evil.example/appAbCdEfGhIjKlMn/tblAbCdEfGhIjKlMn").is_none());
        assert!(Ids::parse("https://airtable.com/appShort/tblAbCdEfGhIjKlMn").is_none());
    }
}
