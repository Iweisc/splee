use chrono::Utc;
use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;
use serde_json::{Value, json};

pub fn resolver() -> anyhow::Result<TokioResolver> {
    Ok(TokioResolver::builder_tokio()?.build()?)
}

/// The trailing dot keeps the system resolver from trying search domains first.
fn fqdn(domain: &str) -> String {
    format!("{}.", domain.trim().trim_end_matches('.'))
}

/// The domain's MX hosts, most preferred first, like "aspmx.l.google.com".
pub async fn mx_exchanges(resolver: &TokioResolver, domain: &str) -> Vec<String> {
    mx_records(resolver, domain).await.into_iter().map(|(_, host)| host).collect()
}

async fn mx_hosts(resolver: &TokioResolver, domain: &str) -> Vec<String> {
    mx_records(resolver, domain).await.into_iter().map(|(pref, host)| format!("{pref} {host}")).collect()
}

async fn mx_records(resolver: &TokioResolver, domain: &str) -> Vec<(u16, String)> {
    let Ok(lookup) = resolver.mx_lookup(fqdn(domain)).await else { return vec![] };
    let mut hosts: Vec<(u16, String)> = lookup
        .answers()
        .iter()
        .filter_map(|r| match &r.data {
            RData::MX(mx) => Some((mx.preference, mx.exchange.to_string().trim_end_matches('.').to_string())),
            _ => None,
        })
        .collect();
    hosts.sort();
    hosts
}

pub async fn has_mx(resolver: &TokioResolver, domain: &str) -> bool {
    !mx_hosts(resolver, domain).await.is_empty()
}

/// True when DNS says mail to this domain can't arrive: the domain doesn't exist, it publishes a null MX (RFC 7505),
/// or it has no MX and no address to fall back to (RFC 5321). A lookup that fails for any other reason counts as
/// deliverable, so a DNS hiccup never cancels an email.
pub async fn undeliverable(resolver: &TokioResolver, domain: &str) -> bool {
    let exchanges: Vec<String> = match resolver.mx_lookup(fqdn(domain)).await {
        Ok(lookup) => lookup
            .answers()
            .iter()
            .filter_map(|r| match &r.data {
                RData::MX(mx) => Some(mx.exchange.to_string()),
                _ => None,
            })
            .collect(),
        Err(e) if e.is_nx_domain() => return true,
        Err(e) if e.is_no_records_found() => vec![],
        Err(_) => return false,
    };
    if !exchanges.is_empty() {
        return exchanges.iter().all(|host| host == ".");
    }
    // Asked apart: a combined lookup reports one error for both, so an IPv4 timeout could pass for "no records".
    let (v4, v6) = tokio::join!(resolver.ipv4_lookup(fqdn(domain)), resolver.ipv6_lookup(fqdn(domain)));
    matches!(v4, Err(e) if e.is_no_records_found()) && matches!(v6, Err(e) if e.is_no_records_found())
}

pub async fn txt_records(resolver: &TokioResolver, name: &str) -> Vec<String> {
    let Ok(lookup) = resolver.txt_lookup(fqdn(name)).await else { return vec![] };
    lookup
        .answers()
        .iter()
        .filter_map(|r| match &r.data {
            RData::TXT(txt) => Some(txt.txt_data.iter().map(|part| String::from_utf8_lossy(part).into_owned()).collect::<String>()),
            _ => None,
        })
        .collect()
}

/// The selectors the usual providers sign with: Google Workspace, Microsoft 365, Cloudflare, Zoho, Proton, Fastmail,
/// Mailchimp and SendGrid, then common defaults. DKIM has no way to list a domain's keys, so these are tried by name.
const DKIM_SELECTORS: [&str; 15] =
    ["google", "selector1", "selector2", "cf2024-1", "zmail", "zoho", "protonmail", "fm1", "k1", "s1", "s2", "default", "dkim", "mail", "smtp"];

/// The first DKIM key found under the mailbox's own selector, when it has one, then DKIM_SELECTORS, as
/// "{selector}: {start of the record}".
async fn dkim(resolver: &TokioResolver, domain: &str, own: Option<&str>) -> Option<String> {
    let selectors = own.into_iter().chain(DKIM_SELECTORS);
    let lookups = selectors.map(|selector| async move { (selector, txt_records(resolver, &format!("{selector}._domainkey.{domain}")).await) });
    futures::future::join_all(lookups).await.into_iter().find_map(|(selector, records)| {
        let key = records.into_iter().find(|t| is_dkim_key(t))?;
        let start: String = key.chars().take(48).collect();
        Some(format!("{selector}: {start}{}", if key.chars().count() > 48 { "…" } else { "" }))
    })
}

/// A published key. An empty `p=` means the key was revoked, so it doesn't count.
fn is_dkim_key(record: &str) -> bool {
    record.split(';').any(|tag| tag.trim().strip_prefix("p=").is_some_and(|key| !key.trim().is_empty()))
}

fn check(found: Option<String>, missing: &str) -> Value {
    match found {
        Some(record) => json!({"ok": true, "record": record}),
        None => json!({"ok": false, "detail": missing}),
    }
}

pub async fn check_dns(domain: &str, dkim_selector: Option<&str>) -> anyhow::Result<Value> {
    let resolver = resolver()?;
    let mx = mx_hosts(&resolver, domain).await;
    let dkim = dkim(&resolver, domain, dkim_selector).await;
    let spf = txt_records(&resolver, domain).await.into_iter().find(|t| t.to_lowercase().starts_with("v=spf1"));
    let dmarc = txt_records(&resolver, &format!("_dmarc.{domain}")).await.into_iter().find(|t| t.to_lowercase().starts_with("v=dmarc1"));
    Ok(json!({
        "mx": check((!mx.is_empty()).then(|| mx.join(", ")), "No MX records, this domain can't receive replies"),
        "spf": check(spf, "No SPF record, add one that includes your email provider"),
        "dmarc": check(dmarc, "No DMARC record, add one at _dmarc with at least p=none"),
        "dkim": check(
            dkim,
            match dkim_selector {
                Some(_) => "No DKIM key under this mailbox's selector or the ones common providers use. Turn on DKIM signing with your email provider",
                None => "No DKIM key under the selectors common providers use. Turn on DKIM signing, or set the mailbox's DKIM selector if your provider uses its own",
            },
        ),
        "checked_at": Utc::now(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ignored by default because it needs real DNS. Run with `cargo test -- --ignored undeliverable`.
    #[tokio::test]
    #[ignore = "needs DNS"]
    async fn undeliverable_only_for_domains_that_cannot_take_mail() {
        let resolver = resolver().unwrap();
        assert!(!undeliverable(&resolver, "gmail.com").await);
        // No MX, but an A record that mail falls back to.
        assert!(!undeliverable(&resolver, "neverssl.com").await);
        // example.com publishes a null MX.
        assert!(undeliverable(&resolver, "example.com").await);
        assert!(undeliverable(&resolver, "no-such-domain-splee-test.com").await);
    }

    #[tokio::test]
    #[ignore = "needs DNS"]
    async fn dkim_is_found_under_the_providers_selector() {
        let resolver = resolver().unwrap();
        // Microsoft 365 publishes selector1 and selector2 as CNAMEs to its own zone and rotates the key between them.
        let found = dkim(&resolver, "microsoft.com", None).await;
        assert!(found.as_deref().is_some_and(|f| f.starts_with("selector") && f.contains("v=DKIM1")), "{found:?}");
        assert_eq!(dkim(&resolver, "no-such-domain-splee-test.com", None).await, None);
        // Mailchimp signs with mandrill, which isn't a common selector, so it's found only when named.
        assert_eq!(dkim(&resolver, "mailchimp.com", None).await.filter(|f| f.starts_with("mandrill")), None);
        let own = dkim(&resolver, "mailchimp.com", Some("mandrill")).await;
        assert!(own.as_deref().is_some_and(|f| f.starts_with("mandrill: ")), "{own:?}");
    }

    #[test]
    fn a_revoked_dkim_key_is_not_a_key() {
        assert!(is_dkim_key("v=DKIM1; k=rsa; p=MIIBIjANBgkq"));
        assert!(!is_dkim_key("v=DKIM1; k=rsa; p="));
        assert!(!is_dkim_key("v=spf1 include:_spf.google.com ~all"));
    }
}
