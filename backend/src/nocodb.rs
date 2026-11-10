//! What Splee's two uses of NocoDB share: importing leads from a table (routes/util_nocodb.rs) and adding hot and booked
//! leads to one (crm/nocodb.rs). NocoDB runs at app.nocodb.com or on the customer's own server, and its API is at the
//! link's origin either way, so requests only go to a public address (see hooks::public_client).

use reqwest::Url;
use serde_json::Value;

pub struct Ids {
    pub origin: Url,
    pub table: String,
    pub view: Option<String>,
}

impl Ids {
    /// {origin}/#/{workspace}/{base}/{table}/{view}/{slug}, as the address bar shows a table or one of its views, with or
    /// without the `#` and the workspace depending on the version. Bases start with p, tables with m and views with vw.
    pub fn parse(link: &str) -> Option<Ids> {
        let link = link.trim();
        let url = Url::parse(if link.contains("://") { link.to_string() } else { format!("https://{link}") }.as_str()).ok()?;
        let path = format!("{}/{}", url.path(), url.fragment().unwrap_or_default());
        let segments: Vec<&str> = path.split(['/', '?']).filter(|s| !s.is_empty()).collect();
        let id = |s: &&str, prefix: &str| s.len() > prefix.len() + 6 && s.starts_with(prefix) && s.bytes().all(|b| b.is_ascii_alphanumeric());
        let base = segments.iter().position(|s| id(s, "p"))?;
        let table = segments.get(base + 1).filter(|s| id(s, "m"))?.to_string();
        let view = segments.get(base + 2).filter(|s| id(s, "vw")).map(|s| s.to_string());
        let origin = Url::parse(&format!("{}/", url.origin().ascii_serialization())).ok()?;
        Some(Ids { origin, table, view })
    }
}

/// NocoDB's flags are true, 1 or "1" depending on the database behind it.
pub fn truthy(value: &Value) -> bool {
    value == true || value == 1 || value == "1"
}

#[cfg(test)]
mod tests {
    use super::Ids;

    #[test]
    fn a_table_link_from_any_version_is_read() {
        let ids = Ids::parse("https://app.nocodb.com/#/wa1b2c3d4/pcy935deadpctht/mfyrw3xgbuxbdv9/vwd0osl7269enqes/leads-leads").unwrap();
        assert_eq!((ids.origin.as_str(), ids.table.as_str(), ids.view.as_deref()), ("https://app.nocodb.com/", "mfyrw3xgbuxbdv9", Some("vwd0osl7269enqes")));
        let own = Ids::parse("db.acme.com/w8077162/pcy935deadpctht/mfyrw3xgbuxbdv9").unwrap();
        assert_eq!((own.origin.as_str(), own.table.as_str(), own.view), ("https://db.acme.com/", "mfyrw3xgbuxbdv9", None));
        let hashed = Ids::parse("http://10.0.0.5:8080/dashboard/#/nc/pcy935deadpctht/mfyrw3xgbuxbdv9?rowId=1").unwrap();
        assert_eq!((hashed.origin.as_str(), hashed.table.as_str()), ("http://10.0.0.5:8080/", "mfyrw3xgbuxbdv9"));
        assert!(Ids::parse("https://app.nocodb.com/#/wa1b2c3d4/pcy935deadpctht").is_none());
        assert!(Ids::parse("https://app.nocodb.com/#/wa1b2c3d4/pcy935deadpctht/leads").is_none());
    }
}
