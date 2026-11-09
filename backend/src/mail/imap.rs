use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use async_imap::Session;
use futures::TryStreamExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use crate::crypto;
use crate::models::Mailbox;
use crate::state::AppState;

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send + Debug {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + Debug> Stream for T {}

pub type ImapSession = Session<Box<dyn Stream>>;

const TIMEOUT: Duration = Duration::from_secs(30);

fn tls_connector() -> anyhow::Result<TlsConnector> {
    let mut roots = RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    // Several crates in the tree enable different rustls backends, so pick one explicitly.
    let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// Port 993 is implicit TLS. Any other port is plain IMAP (local relays and test servers).
pub async fn connect(state: &AppState, mb: &Mailbox) -> anyhow::Result<ImapSession> {
    let host = mb.imap_host.trim();
    let port = u16::try_from(mb.imap_port).context("invalid IMAP port")?;
    let tcp = tokio::time::timeout(TIMEOUT, TcpStream::connect((host, port))).await.map_err(|_| anyhow!("IMAP: connection timed out"))??;
    let stream: Box<dyn Stream> = if port == 993 {
        let name = ServerName::try_from(host.to_string()).context("invalid IMAP host")?;
        Box::new(tls_connector()?.connect(name, tcp).await.context("IMAP: TLS handshake failed")?)
    } else {
        Box::new(tcp)
    };

    let mut client = async_imap::Client::new(stream);
    tokio::time::timeout(TIMEOUT, client.read_response()).await.map_err(|_| anyhow!("IMAP: no greeting"))??.context("IMAP: no greeting")?;
    let username = if mb.imap_username.trim().is_empty() { mb.smtp_username.trim() } else { mb.imap_username.trim() };
    let password_enc = if mb.imap_password_enc.is_empty() { &mb.smtp_password_enc } else { &mb.imap_password_enc };
    let password = crypto::decrypt(&state.cfg.app_secret, password_enc)?;
    let session = tokio::time::timeout(TIMEOUT, client.login(username, password))
        .await
        .map_err(|_| anyhow!("IMAP: login timed out"))?
        .map_err(|(e, _)| anyhow!("IMAP login failed: {e}"))?;
    Ok(session)
}

pub async fn test(state: &AppState, mb: &Mailbox) -> anyhow::Result<()> {
    let mut session = connect(state, mb).await?;
    session.select("INBOX").await.map_err(|e| anyhow!("IMAP: could not open INBOX: {e}"))?;
    let _ = session.logout().await;
    Ok(())
}

/// Where a mailbox's first poll starts: just before the oldest message in INBOX that arrived on or after `since`, so a
/// busy inbox isn't downloaded from UID 1. Without such a message, after the newest one.
pub async fn first_uid(state: &AppState, mb: &Mailbox, since: chrono::NaiveDate) -> anyhow::Result<u32> {
    let mut session = connect(state, mb).await?;
    let inbox = session.select("INBOX").await.map_err(|e| anyhow!("IMAP: could not open INBOX: {e}"))?;
    let found = session.uid_search(format!("SINCE {}", since.format("%-d-%b-%Y"))).await.map_err(|e| anyhow!("IMAP search failed: {e}"))?;
    let _ = session.logout().await;
    Ok(match found.into_iter().min() {
        Some(oldest) => oldest - 1,
        None => inbox.uid_next.unwrap_or(1).saturating_sub(1),
    })
}

/// INBOX's UIDVALIDITY and its raw RFC 822 messages with a UID above `after`, oldest first. When UIDVALIDITY isn't the
/// one `after` was taken under, the UIDs were renumbered and `after` means nothing, so it fetches nothing.
pub async fn fetch_new(state: &AppState, mb: &Mailbox, after: u32) -> anyhow::Result<(Option<u32>, Vec<(u32, Vec<u8>)>)> {
    let mut session = connect(state, mb).await?;
    let inbox = session.select("INBOX").await.map_err(|e| anyhow!("IMAP: could not open INBOX: {e}"))?;
    let validity = inbox.uid_validity;
    if let (Some(old), Some(new)) = (mb.imap_uid_validity, validity)
        && old != i64::from(new)
    {
        let _ = session.logout().await;
        return Ok((validity, vec![]));
    }
    let mut out = vec![];
    {
        let fetches = session.uid_fetch(format!("{}:*", after + 1), "(UID BODY.PEEK[])").await.map_err(|e| anyhow!("IMAP fetch failed: {e}"))?;
        let fetches: Vec<_> = tokio::time::timeout(Duration::from_secs(120), fetches.try_collect())
            .await
            .map_err(|_| anyhow!("IMAP: fetch timed out"))?
            .map_err(|e| anyhow!("IMAP fetch failed: {e}"))?;
        for f in fetches {
            // "N:*" returns the newest message even when its UID is below N, so filter again.
            if let (Some(uid), Some(body)) = (f.uid, f.body())
                && uid > after
            {
                out.push((uid, body.to_vec()));
            }
        }
    }
    let _ = session.logout().await;
    out.sort_by_key(|(uid, _)| *uid);
    Ok((validity, out))
}
