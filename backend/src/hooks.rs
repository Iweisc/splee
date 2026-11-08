//! Posts JSON to the URLs users paste into project settings (the hot-lead webhook and Slack).
//! Those URLs can point anywhere, including this server's own network, so a host that resolves to a
//! private, loopback or link-local address is refused. The request is pinned to the addresses that
//! were checked, with no proxy and no redirects, so neither a second DNS answer nor a 302 can get around it.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::Context;
use reqwest::Url;
use serde_json::Value;

pub async fn post_json(raw: &str, body: &Value) -> anyhow::Result<()> {
    let url = Url::parse(raw.trim())?;
    anyhow::ensure!(matches!(url.scheme(), "http" | "https"), "only http and https URLs are allowed");
    let host = url.host_str().context("the URL has no host")?.to_string();
    let port = url.port_or_known_default().context("the URL has no port")?;
    let addrs: Vec<SocketAddr> = match host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host.as_str(), port)).await?.collect(),
    };
    anyhow::ensure!(!addrs.is_empty() && addrs.iter().all(|a| is_public(a.ip())), "{host} is not a public address");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        // A proxy from HTTP(S)_PROXY would resolve the host itself and skip the check above.
        .no_proxy()
        .resolve_to_addrs(&host, &addrs)
        .build()?;
    client.post(url).json(body).send().await?.error_for_status()?;
    Ok(())
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            let shared = a == 100 && (64..128).contains(&b); // 100.64.0.0/10, carrier-grade NAT
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_documentation()
                || v4.is_multicast()
                || shared
                || a == 0
                || a >= 240)
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public(IpAddr::V4(v4)),
            None => !(v6.is_loopback() || v6.is_unspecified() || v6.is_unique_local() || v6.is_unicast_link_local() || v6.is_multicast()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_pass() {
        for ip in ["127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:127.0.0.1"] {
            assert!(!is_public(ip.parse().unwrap()), "{ip} should be refused");
        }
        for ip in ["1.1.1.1", "52.1.2.3", "2606:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip} should be allowed");
        }
    }

    #[tokio::test]
    async fn internal_hosts_are_refused_before_connecting() {
        for url in ["http://127.0.0.1:8787/api", "http://localhost/hook", "http://[::1]/hook", "http://169.254.169.254/latest/meta-data"] {
            let err = post_json(url, &Value::Null).await.unwrap_err().to_string();
            assert!(err.ends_with("is not a public address"), "{url}: {err}");
        }
        assert!(post_json("ftp://example.com/x", &Value::Null).await.is_err());
    }
}
