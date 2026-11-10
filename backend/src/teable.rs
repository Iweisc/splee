//! Where a Teable link points, for importing leads from a table (routes/util_teable.rs). Teable runs at app.teable.ai or on the customer's own server, and its API is at the
//! link's origin either way, so requests only go to a public address (see hooks::public_client).

use reqwest::Url;

pub struct Ids {
    pub origin: Url,
    pub base: String,
    pub table: String,
    pub view: Option<String>,
}

impl Ids {
    /// {origin}/base/{base}/table/{table}/{view}, as the address bar shows a table or one of its views. Bases start with
    /// bse, tables with tbl and views with viw.
    pub fn parse(link: &str) -> Option<Ids> {
        let link = link.trim();
        let url = Url::parse(if link.contains("://") { link.to_string() } else { format!("https://{link}") }.as_str()).ok()?;
        let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        let id = |s: &&str, prefix: &str| s.len() > prefix.len() + 6 && s.starts_with(prefix) && s.bytes().all(|b| b.is_ascii_alphanumeric());
        let at = segments.iter().position(|s| id(s, "tbl"))?;
        let base = segments[..at].iter().find(|s| id(s, "bse"))?.to_string();
        let view = segments.get(at + 1).filter(|s| id(s, "viw")).map(|s| s.to_string());
        let origin = Url::parse(&format!("{}/", url.origin().ascii_serialization())).ok()?;
        Some(Ids { origin, base, table: segments[at].to_string(), view })
    }
}

#[cfg(test)]
mod tests {
    use super::Ids;

    #[test]
    fn a_table_or_view_link_is_read() {
        let ids = Ids::parse("https://app.teable.ai/base/bseTcK9QPzyXDlXQgNL/table/tblaTy6C1IpfhtrfHkl/viwytDHYTcg163HmGfq?recordId=rec1").unwrap();
        assert_eq!((ids.origin.as_str(), ids.table.as_str(), ids.view.as_deref()), ("https://app.teable.ai/", "tblaTy6C1IpfhtrfHkl", Some("viwytDHYTcg163HmGfq")));
        let own = Ids::parse("teable.acme.com/base/bseTcK9QPzyXDlXQgNL/tblaTy6C1IpfhtrfHkl").unwrap();
        assert_eq!((own.origin.as_str(), own.table.as_str(), own.view), ("https://teable.acme.com/", "tblaTy6C1IpfhtrfHkl", None));
        assert!(Ids::parse("https://app.teable.ai/base/bseTcK9QPzyXDlXQgNL").is_none());
        assert!(Ids::parse("https://app.teable.ai/table/tblaTy6C1IpfhtrfHkl").is_none());
    }
}
