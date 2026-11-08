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

async fn mx_hosts(resolver: &TokioResolver, domain: &str) -> Vec<String> {
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
    hosts.into_iter().map(|(pref, host)| format!("{pref} {host}")).collect()
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

async fn txt_records(resolver: &TokioResolver, name: &str) -> Vec<String> {
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

fn check(found: Option<String>, missing: &str) -> Value {
    match found {
        Some(record) => json!({"ok": true, "record": record}),
        None => json!({"ok": false, "detail": missing}),
    }
}

pub async fn check_dns(domain: &str) -> anyhow::Result<Value> {
    let resolver = resolver()?;
    let mx = mx_hosts(&resolver, domain).await;
    let spf = txt_records(&resolver, domain).await.into_iter().find(|t| t.to_lowercase().starts_with("v=spf1"));
    let dmarc = txt_records(&resolver, &format!("_dmarc.{domain}")).await.into_iter().find(|t| t.to_lowercase().starts_with("v=dmarc1"));
    Ok(json!({
        "mx": check((!mx.is_empty()).then(|| mx.join(", ")), "No MX records, this domain can't receive replies"),
        "spf": check(spf, "No SPF record, add one that includes your email provider"),
        "dmarc": check(dmarc, "No DMARC record, add one at _dmarc with at least p=none"),
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
}
