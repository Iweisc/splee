//! What Splee's two uses of Grist share: importing leads from a table (routes/util_grist.rs) and adding hot and booked
//! leads to one (crm/grist.rs). Grist runs at getgrist.com or on the customer's own server, and its API is at the link's
//! origin either way, so requests only go to a public address (see hooks::public_client).

use reqwest::Url;

pub struct Ids {
    /// The API's root, like https://docs.getgrist.com/api/, or https://grist.acme.com/o/sales/api/ for a team on a
    /// self-hosted server.
    pub api: Url,
    pub doc: String,
    pub page: Option<u32>,
}

impl Ids {
    /// https://docs.getgrist.com/{doc}/{name}/p/{page}, as the address bar shows a document's page, with /o/{team} before
    /// it on some servers and /doc/ on older links.
    pub fn parse(link: &str) -> Option<Ids> {
        let link = link.trim();
        let url = Url::parse(if link.contains("://") { link.to_string() } else { format!("https://{link}") }.as_str()).ok()?;
        let mut segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        let mut api = url.origin().ascii_serialization();
        if segments.first() == Some(&"o") && segments.len() > 1 {
            api = format!("{api}/o/{}", segments[1]);
            segments.drain(..2);
        }
        if segments.first() == Some(&"doc") {
            segments.remove(0);
        }
        let doc = segments.first().filter(|s| s.len() >= 12 && s.bytes().all(|b| b.is_ascii_alphanumeric()))?.to_string();
        let page = segments.iter().position(|s| *s == "p").and_then(|i| segments.get(i + 1)?.parse().ok());
        Some(Ids { api: Url::parse(&format!("{api}/api/")).ok()?, doc, page })
    }

    /// SQL for the id of the table the link's page shows: its first widget's, or the first page's when the link has no
    /// page. The page is a number, so it's safe in the query.
    pub fn table_query(&self) -> String {
        match self.page {
            Some(page) => format!(
                "select t.tableId from _grist_Views_section s join _grist_Tables t on s.tableRef = t.id \
                 where s.parentId = {page} order by s.id limit 1"
            ),
            None => "select t.tableId from _grist_Pages p join _grist_Views_section s on s.parentId = p.viewRef \
                     join _grist_Tables t on s.tableRef = t.id order by p.pagePos, s.id limit 1"
                .to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Ids;

    #[test]
    fn a_page_link_on_getgrist_or_a_server_is_read() {
        let ids = Ids::parse("https://docs.getgrist.com/d1oetuJtF651/Sales/p/3#a1.s8.r2.c3").unwrap();
        assert_eq!((ids.api.as_str(), ids.doc.as_str(), ids.page), ("https://docs.getgrist.com/api/", "d1oetuJtF651", Some(3)));
        let team = Ids::parse("grist.acme.com/o/sales/d1oetuJtF651/Sales").unwrap();
        assert_eq!((team.api.as_str(), team.doc.as_str(), team.page), ("https://grist.acme.com/o/sales/api/", "d1oetuJtF651", None));
        let old = Ids::parse("https://acme.getgrist.com/doc/d1oetuJtF651AoLQVQVvkC/p/acl").unwrap();
        assert_eq!((old.api.as_str(), old.doc.as_str(), old.page), ("https://acme.getgrist.com/api/", "d1oetuJtF651AoLQVQVvkC", None));
        assert!(Ids::parse("https://docs.getgrist.com/").is_none());
        assert!(Ids::parse("https://docs.getgrist.com/o/docs/").is_none());
    }
}
