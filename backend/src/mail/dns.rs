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
