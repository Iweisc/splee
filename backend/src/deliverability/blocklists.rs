//! Domain and IP blocklists, asked with plain DNS A queries: `{domain}.{zone}` or `{reversed IPv4}.{zone}`. No answer
//! means not listed, a 127.0.0.x answer is a listing code, and each list has codes of its own for "you can't ask me",
//! which mean the check didn't happen, not that the address is listed.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr};

use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;

use super::thresholds::Level;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Domain,
    Ip,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Domain => "domain",
            Kind::Ip => "ip",
        }
    }
}

pub struct List {
    /// As stored in blocklist_checks.list.
    pub id: &'static str,
    pub label: &'static str,
    pub zone: &'static str,
    pub kind: Kind,
    /// Where the owner of a listed domain or address looks it up and asks for removal. `{}` is the domain or address.
    pub delist_url: &'static str,
    /// How much a listing hurts. See thresholds::list_level.
    pub level: Level,
}

pub const LISTS: [List; 6] = [
    List {
        id: "spamhaus_dbl",
        label: "Spamhaus DBL",
        zone: "dbl.spamhaus.org",
        kind: Kind::Domain,
        delist_url: "https://check.spamhaus.org/results/?query={}",
        level: Level::AtRisk,
    },
    List { id: "surbl", label: "SURBL", zone: "multi.surbl.org", kind: Kind::Domain, delist_url: "https://surbl.org/surbl-analysis", level: Level::AtRisk },
    List { id: "uribl", label: "URIBL", zone: "multi.uribl.com", kind: Kind::Domain, delist_url: "https://admin.uribl.com/", level: Level::AtRisk },
    List {
        id: "spamhaus_zen",
        label: "Spamhaus ZEN",
        zone: "zen.spamhaus.org",
        kind: Kind::Ip,
        delist_url: "https://check.spamhaus.org/results/?query={}",
        level: Level::AtRisk,
    },
    List {
        id: "barracuda",
        label: "Barracuda",
        zone: "b.barracudacentral.org",
        kind: Kind::Ip,
        delist_url: "https://www.barracudacentral.org/rbl/removal-request/{}",
        level: Level::AtRisk,
    },
    List { id: "spamcop", label: "SpamCop", zone: "bl.spamcop.net", kind: Kind::Ip, delist_url: "https://www.spamcop.net/bl.shtml?{}", level: Level::Watch },
];

pub fn list(id: &str) -> Option<&'static List> {
    LISTS.iter().find(|l| l.id == id)
}

pub fn delist_url(list: &List, target: &str) -> String {
    list.delist_url.replace("{}", target)
}

/// The name to ask for `target` on `list`: the domain, or the IPv4 address's octets reversed, under the list's zone.
pub fn query_name(list: &List, target: &str) -> Option<String> {
    let name = match list.kind {
        Kind::Domain => target.trim().trim_end_matches('.').to_ascii_lowercase(),
        Kind::Ip => {
            let ip: Ipv4Addr = target.parse().ok()?;
            let [a, b, c, d] = ip.octets();
            format!("{d}.{c}.{b}.{a}")
        }
    };
    Some(format!("{name}.{}.", list.zone))
}

/// What the resolver said to one A query.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Addrs(Vec<Ipv4Addr>),
    /// NXDOMAIN or no A records: the usual "not listed".
    NotFound,
    /// A timeout, SERVFAIL or a refused query.
    Failed(String),
}

/// The DNS lookups the checks make, so tests can answer them without the network.
pub trait Resolve: Sync {
    fn a(&self, name: &str) -> impl Future<Output = Answer> + Send;
}

impl Resolve for TokioResolver {
    async fn a(&self, name: &str) -> Answer {
        match self.ipv4_lookup(name).await {
            Ok(lookup) => {
                let addrs: Vec<Ipv4Addr> = lookup
                    .answers()
                    .iter()
                    .filter_map(|r| match &r.data {
                        RData::A(a) => Some(a.0),
                        _ => None,
                    })
                    .collect();
                if addrs.is_empty() { Answer::NotFound } else { Answer::Addrs(addrs) }
            }
            Err(e) if e.is_nx_domain() || e.is_no_records_found() => Answer::NotFound,
            Err(e) => Answer::Failed(e.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Clear,
    /// What the listing means, like "Spam domain".
    Listed(String),
    /// Why the list couldn't be asked.
    Unknown(String),
}

impl Outcome {
    pub fn status(&self) -> &'static str {
        match self {
            Outcome::Clear => "clear",
            Outcome::Listed(_) => "listed",
            Outcome::Unknown(_) => "unknown",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Outcome::Clear => "",
            Outcome::Listed(d) | Outcome::Unknown(d) => d,
        }
    }
}

const PUBLIC_RESOLVER: &str = "The list refused to answer this server's DNS resolver, which is likely a public one like 8.8.8.8. \
                               Point the server at its own resolver to check it.";

/// Reads a list's answer. Codes outside 127.0.0.0/8 come from a resolver that answers missing names with an address of its
/// own, which says nothing about the list.
pub fn read(list: &List, answer: &Answer) -> Outcome {
    let addrs = match answer {
        Answer::NotFound => return Outcome::Clear,
        Answer::Failed(why) => return Outcome::Unknown(format!("The lookup failed: {why}.")),
        Answer::Addrs(addrs) => addrs,
    };
    let codes: Vec<[u8; 4]> = addrs.iter().map(|a| a.octets()).filter(|o| o[0] == 127).collect();
    if codes.is_empty() {
        return Outcome::Unknown("The DNS resolver answered with an address that isn't a blocklist code, so it may rewrite missing names.".into());
    }
    if codes.iter().any(|&c| refused(list, c)) {
        return Outcome::Unknown(PUBLIC_RESOLVER.into());
    }
    let mut meanings: Vec<&str> = codes.iter().filter_map(|&c| meaning(list, c)).collect();
    meanings.dedup();
    if meanings.is_empty() { Outcome::Clear } else { Outcome::Listed(meanings.join(", ")) }
}

/// Spamhaus answers 127.255.255.252 for a malformed query, .254 for a public or open resolver and .255 for too many
/// queries. SURBL and URIBL answer 127.0.0.1 to a resolver they block.
fn refused(list: &List, code: [u8; 4]) -> bool {
    match list.id {
        "spamhaus_dbl" | "spamhaus_zen" => code[1..3] == [255, 255],
        "surbl" | "uribl" => code == [127, 0, 0, 1],
        _ => false,
    }
}

/// What a code means on its list, or None for a code that isn't a listing, like URIBL's grey list, which URIBL itself
/// says not to block on.
fn meaning(list: &List, [_, b, c, d]: [u8; 4]) -> Option<&'static str> {
    match list.id {
        "spamhaus_dbl" if (b, c) == (0, 1) => Some(match d {
            2 => "Spam domain",
            4 => "Phishing domain",
            5 => "Malware domain",
            6 => "Botnet domain",
            102..=106 => "Abused legitimate domain",
            _ => "Listed",
        }),
        "spamhaus_zen" if (b, c) == (0, 0) => Some(match d {
            2 | 9 => "Spam source (SBL)",
            3 => "Snowshoe spam (CSS)",
            4..=7 => "Infected or exploited (XBL)",
            10 | 11 => "Not meant to send email directly (PBL)",
            _ => "Listed",
        }),
        "surbl" if (b, c) == (0, 0) => surbl(d),
        "uribl" if (b, c) == (0, 0) && d & 0b1010 != 0 => Some(if d & 0b10 != 0 { "Black list" } else { "Red list" }),
        "barracuda" | "spamcop" if (b, c, d) == (0, 0, 2) => Some("Listed"),
        _ => None,
    }
}

/// SURBL's multi list is a bitmask: 8 phishing, 16 malware, 64 spam, 128 cracked sites.
fn surbl(bits: u8) -> Option<&'static str> {
    [(8, "Phishing"), (16, "Malware"), (64, "Spam"), (128, "Cracked site")].into_iter().find(|(bit, _)| bits & bit != 0).map(|(_, what)| what)
}

/// Asks one list about one target.
pub async fn check(resolver: &impl Resolve, list: &List, target: &str) -> Outcome {
    match query_name(list, target) {
        Some(name) => read(list, &resolver.a(&name).await),
        None => Outcome::Unknown("Not an IPv4 address.".into()),
    }
}

/// The public IPv4 addresses `host` resolves to. Only IPv4: the IP lists here key their IPv6 entries differently, and
/// most mail servers still send over IPv4.
pub async fn public_ipv4(resolver: &impl Resolve, host: &str) -> Vec<Ipv4Addr> {
    let host = host.trim().trim_end_matches('.');
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return if crate::hooks::is_public(IpAddr::V4(ip)) { vec![ip] } else { vec![] };
    }
    match resolver.a(&format!("{host}.")).await {
        Answer::Addrs(addrs) => addrs.into_iter().filter(|a| crate::hooks::is_public(IpAddr::V4(*a))).collect(),
        _ => vec![],
    }
}

/// The server that handed a message to its recipient's mail system: the first Received header from the top whose "from"
/// part carries a public IPv4 address in brackets. Hops inside the receiving system above it carry private addresses or
/// none, and the hops below it are the sender's own.
pub fn received_ip(raw: &[u8]) -> Option<Ipv4Addr> {
    let text = String::from_utf8_lossy(raw);
    let head = text.split("\r\n\r\n").next().unwrap_or_default().split("\n\n").next().unwrap_or_default();
    // Continuation lines start with whitespace and belong to the header above them.
    let mut headers: Vec<String> = vec![];
    for line in head.lines() {
        match headers.last_mut() {
            Some(last) if line.starts_with([' ', '\t']) => {
                last.push(' ');
                last.push_str(line.trim());
            }
            _ => headers.push(line.trim_end().to_string()),
        }
    }
    headers.iter().filter_map(|h| h.get(..9).filter(|name| name.eq_ignore_ascii_case("received:")).map(|_| &h[9..])).find_map(|value| {
        let from = value.trim_start().strip_prefix("from ")?;
        let from = from.split(" by ").next().unwrap_or(from);
        from.split('[').skip(1).filter_map(|part| part.split(']').next()?.parse::<Ipv4Addr>().ok()).find(|ip| crate::hooks::is_public(IpAddr::V4(*ip)))
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Answers from a table, and NotFound for anything else, like a list that has never heard of the name.
    #[derive(Default)]
    pub(crate) struct FakeResolver(pub HashMap<String, Answer>);

    impl FakeResolver {
        pub(crate) fn with(mut self, name: &str, addrs: &[[u8; 4]]) -> Self {
            self.0.insert(name.to_string(), Answer::Addrs(addrs.iter().map(|&o| Ipv4Addr::from(o)).collect()));
            self
        }

        pub(crate) fn failing(mut self, name: &str) -> Self {
            self.0.insert(name.to_string(), Answer::Failed("timed out".into()));
            self
        }
    }

    impl Resolve for FakeResolver {
        async fn a(&self, name: &str) -> Answer {
            self.0.get(name).cloned().unwrap_or(Answer::NotFound)
        }
    }

    fn on(id: &str, code: [u8; 4]) -> Outcome {
        read(list(id).unwrap(), &Answer::Addrs(vec![Ipv4Addr::from(code)]))
    }

    #[test]
    fn query_names_put_the_target_under_the_zone() {
        assert_eq!(query_name(list("spamhaus_dbl").unwrap(), "Try-Acme.com.").as_deref(), Some("try-acme.com.dbl.spamhaus.org."));
        assert_eq!(query_name(list("spamhaus_zen").unwrap(), "203.0.113.7").as_deref(), Some("7.113.0.203.zen.spamhaus.org."));
        assert_eq!(query_name(list("barracuda").unwrap(), "not-an-ip"), None);
    }

    #[test]
    fn listing_codes_say_what_they_mean() {
        assert_eq!(on("spamhaus_dbl", [127, 0, 1, 2]), Outcome::Listed("Spam domain".into()));
        assert_eq!(on("spamhaus_dbl", [127, 0, 1, 104]), Outcome::Listed("Abused legitimate domain".into()));
        assert_eq!(on("spamhaus_zen", [127, 0, 0, 4]), Outcome::Listed("Infected or exploited (XBL)".into()));
        assert_eq!(on("spamhaus_zen", [127, 0, 0, 10]), Outcome::Listed("Not meant to send email directly (PBL)".into()));
        assert_eq!(on("surbl", [127, 0, 0, 64 + 8]), Outcome::Listed("Phishing".into()));
        assert_eq!(on("uribl", [127, 0, 0, 2]), Outcome::Listed("Black list".into()));
        assert_eq!(on("barracuda", [127, 0, 0, 2]), Outcome::Listed("Listed".into()));
        assert_eq!(on("spamcop", [127, 0, 0, 2]), Outcome::Listed("Listed".into()));
    }

    #[test]
    fn refused_queries_are_unknown_not_listed() {
        for code in [[127, 255, 255, 252], [127, 255, 255, 254], [127, 255, 255, 255]] {
            assert_eq!(on("spamhaus_dbl", code), Outcome::Unknown(PUBLIC_RESOLVER.into()), "{code:?}");
            assert_eq!(on("spamhaus_zen", code), Outcome::Unknown(PUBLIC_RESOLVER.into()), "{code:?}");
        }
        assert_eq!(on("surbl", [127, 0, 0, 1]), Outcome::Unknown(PUBLIC_RESOLVER.into()));
        assert_eq!(on("uribl", [127, 0, 0, 1]), Outcome::Unknown(PUBLIC_RESOLVER.into()));
        // A refusal wins over a listing code in the same answer.
        let mixed = Answer::Addrs(vec![Ipv4Addr::new(127, 0, 1, 2), Ipv4Addr::new(127, 255, 255, 254)]);
        assert!(matches!(read(list("spamhaus_dbl").unwrap(), &mixed), Outcome::Unknown(_)));
    }

    #[test]
    fn other_answers_are_clear_or_unknown() {
        let dbl = list("spamhaus_dbl").unwrap();
        assert_eq!(read(dbl, &Answer::NotFound), Outcome::Clear);
        assert!(matches!(read(dbl, &Answer::Failed("SERVFAIL".into())), Outcome::Unknown(why) if why.contains("SERVFAIL")));
        // An ISP resolver that answers every missing name with its search page.
        assert!(matches!(read(dbl, &Answer::Addrs(vec![Ipv4Addr::new(198, 51, 100, 9)])), Outcome::Unknown(_)));
        // URIBL's grey list and codes a list doesn't use aren't listings.
        assert_eq!(on("uribl", [127, 0, 0, 4]), Outcome::Clear);
        assert_eq!(on("spamcop", [127, 0, 0, 3]), Outcome::Clear);
    }

    #[tokio::test]
    async fn checks_ask_the_resolver_by_name() {
        let resolver = FakeResolver::default()
            .with("bad.test.dbl.spamhaus.org.", &[[127, 0, 1, 2]])
            .failing("7.113.0.203.b.barracudacentral.org.")
            .with("smtp.acme.test.", &[[203, 0, 113, 7], [10, 0, 0, 1]]);
        assert_eq!(check(&resolver, list("spamhaus_dbl").unwrap(), "bad.test").await, Outcome::Listed("Spam domain".into()));
        assert_eq!(check(&resolver, list("spamhaus_dbl").unwrap(), "good.test").await, Outcome::Clear);
        assert!(matches!(check(&resolver, list("barracuda").unwrap(), "203.0.113.7").await, Outcome::Unknown(_)));
        // 203.0.113.0/24 is documentation space and 10.0.0.1 private, so neither is a sending address.
        assert_eq!(public_ipv4(&resolver, "smtp.acme.test").await, Vec::<Ipv4Addr>::new());
        let resolver = resolver.with("smtp.acme.test.", &[[142, 250, 1, 109], [10, 0, 0, 1], [74, 125, 140, 108]]);
        assert_eq!(public_ipv4(&resolver, "smtp.acme.test").await, vec![Ipv4Addr::new(142, 250, 1, 109), Ipv4Addr::new(74, 125, 140, 108)]);
        assert_eq!(public_ipv4(&resolver, "74.125.140.108").await, vec![Ipv4Addr::new(74, 125, 140, 108)]);
    }

    #[test]
    fn the_sending_ip_comes_from_the_receivers_first_hop() {
        let raw = "Received: by 2002:a05:6a10:a1c4:b0:5a1:1b2c:3d4e with SMTP id x4csp1;\r\n        Tue, 10 Nov 2026 09:00:01 -0800\r\n\
                   Received: from mx.internal (mx.internal [10.1.2.3]) by relay.internal;\r\n\
                   Received: from mail-sor-f41.google.com (mail-sor-f41.google.com.\r\n [209.85.220.41]) by mx.google.com with SMTPS id a1;\r\n\
                   Received: from [192.168.1.20] (host-85-1-2-3.example.net [85.1.2.3]) by smtp.gmail.com;\r\n\
                   Subject: hi\r\n\r\nReceived: from body [8.8.8.8] by nobody\r\n";
        assert_eq!(received_ip(raw.as_bytes()), Some(Ipv4Addr::new(209, 85, 220, 41)));
        assert_eq!(received_ip(b"Subject: no hops\r\n\r\nbody"), None);
        assert_eq!(received_ip(b"Received: from x (x [IPv6:2001:db8::1]) by y\r\n\r\n"), None);
    }
}
