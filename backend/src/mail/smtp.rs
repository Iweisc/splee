use std::time::Duration;

use anyhow::Context;
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::Mailbox as Address;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::crypto;
use crate::models::Mailbox;
use crate::state::AppState;

pub type Transport = AsyncSmtpTransport<Tokio1Executor>;

pub async fn transport(state: &AppState, mb: &Mailbox) -> anyhow::Result<Transport> {
    let host = mb.smtp_host.trim();
    let builder = match mb.smtp_security.as_str() {
        "tls" => Transport::relay(host)?,
        "starttls" => Transport::starttls_relay(host)?,
        _ => Transport::builder_dangerous(host),
    };
    let mut builder = builder.port(u16::try_from(mb.smtp_port).context("invalid SMTP port")?).timeout(Some(Duration::from_secs(30)));
    if mb.provider != "password" {
        let token = super::oauth::access_token(state, mb).await?;
        builder = builder.credentials(Credentials::new(mb.email.clone(), token)).authentication(vec![Mechanism::Xoauth2]);
    // An empty username means the server accepts mail without login (local relays, test servers).
    } else if !mb.smtp_username.trim().is_empty() {
        let password = crypto::decrypt(&state.cfg.app_secret, &mb.smtp_password_enc)?;
        builder = builder.credentials(Credentials::new(mb.smtp_username.trim().to_string(), password));
    }
    Ok(builder.build())
}

/// Connects, authenticates and says NOOP.
pub async fn test(state: &AppState, mb: &Mailbox) -> anyhow::Result<()> {
    let ok = transport(state, mb).await?.test_connection().await.map_err(|e| anyhow::anyhow!("SMTP: {e}"))?;
    anyhow::ensure!(ok, "SMTP: the server did not answer NOOP");
    Ok(())
}

pub struct Outgoing<'a> {
    pub to_name: &'a str,
    pub to_email: &'a str,
    pub subject: &'a str,
    pub body: &'a str,
    /// Without angle brackets, like every message id stored in email_messages.
    pub message_id: &'a str,
    pub in_reply_to: Option<&'a str>,
    pub references: &'a [String],
    pub unsubscribe_url: Option<&'a str>,
    /// The token of a warmup email, sent as X-Splee-Warmup. See warmup::find_token.
    pub warmup: Option<&'a str>,
}

fn bracket(id: &str) -> String {
    format!("<{}>", id.trim_matches(['<', '>']))
}

pub fn build(mb: &Mailbox, email: &Outgoing) -> anyhow::Result<Message> {
    let from_name = (!mb.from_name.trim().is_empty()).then(|| mb.from_name.trim().to_string());
    let from = Address::new(from_name, mb.email.parse().context("invalid mailbox address")?);
    let to_name = (!email.to_name.trim().is_empty()).then(|| email.to_name.trim().to_string());
    let to = Address::new(to_name, email.to_email.parse().with_context(|| format!("invalid recipient {}", email.to_email))?);

    let mut builder = Message::builder()
        .from(from)
        .to(to)
        .subject(email.subject)
        .message_id(Some(bracket(email.message_id)))
        .header(ContentType::TEXT_PLAIN);
    if let Some(parent) = email.in_reply_to {
        builder = builder.in_reply_to(bracket(parent));
    }
    if !email.references.is_empty() {
        builder = builder.references(email.references.iter().map(|r| bracket(r)).collect::<Vec<_>>().join(" "));
    }
    if let Some(url) = email.unsubscribe_url {
        builder = builder
            .raw_header(HeaderValue::new(HeaderName::new_from_ascii_str("List-Unsubscribe"), format!("<{url}>")))
            .raw_header(HeaderValue::new(HeaderName::new_from_ascii_str("List-Unsubscribe-Post"), "List-Unsubscribe=One-Click".into()));
    }
    if let Some(token) = email.warmup {
        builder = builder.raw_header(HeaderValue::new(HeaderName::new_from_ascii_str(super::warmup::HEADER), token.to_string()));
    }
    Ok(builder.body(email.body.to_string())?)
}

/// The server says this recipient can't get mail: a 550, 551 or 553 that, when it carries an enhanced status code
/// (RFC 3463), is about the address (5.1.x). Our own server also answers 550 for its sending quota (Gmail's 5.4.5) or a
/// policy block (5.7.x), and those are the mailbox's problem, not a reason to give up on the lead.
pub fn recipient_rejected(e: &lettre::transport::smtp::Error) -> bool {
    if !e.status().is_some_and(|c| matches!(c.to_string().as_str(), "550" | "551" | "553")) {
        return false;
    }
    let reply = std::error::Error::source(e).map(ToString::to_string).unwrap_or_default();
    enhanced_code(&reply).is_none_or(|code| code.starts_with("5.1."))
}

/// The mailbox ran out of its sending quota, like Gmail's "550 5.4.5 Daily user sending limit exceeded". It sends again
/// once the quota resets.
pub fn over_quota(e: &lettre::transport::smtp::Error) -> bool {
    let reply = std::error::Error::source(e).map(ToString::to_string).unwrap_or_default();
    e.is_permanent() && enhanced_code(&reply) == Some("5.4.5")
}

/// The "5.1.1" that starts a reply like "5.1.1 The email account that you tried to reach does not exist".
fn enhanced_code(reply: &str) -> Option<&str> {
    let first = reply.split_whitespace().next()?;
    let parts: Vec<&str> = first.split('.').collect();
    let numeric = parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()));
    (parts.len() == 3 && numeric).then_some(first)
}

/// The server never answered: it was down, unreachable or too slow. Its replies, TLS and our own setup are other kinds.
pub fn unreachable(e: &lettre::transport::smtp::Error) -> bool {
    !(e.is_response() || e.is_client() || e.is_transient() || e.is_permanent() || e.is_tls() || e.is_transport_shutdown())
}

pub async fn send(transport: &Transport, message: Message) -> Result<(), lettre::transport::smtp::Error> {
    transport.send(message).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    use lettre::{AsyncTransport, Message};
    use tokio::io::{AsyncBufReadExt, BufReader};

    use super::{Transport, over_quota, recipient_rejected, unreachable};

    async fn test_on(port: u16) -> lettre::transport::smtp::Error {
        let transport: Transport = Transport::builder_dangerous("127.0.0.1").port(port).build();
        transport.test_connection().await.unwrap_err()
    }

    /// Sends one email through a fake server that answers RCPT TO with `rcpt_reply`.
    async fn send_rejected(rcpt_reply: &'static str) -> lettre::transport::smtp::Error {
        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (socket, _) = server.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            write.write_all(b"220 fake\r\n").await.unwrap();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let reply = if line.starts_with("RCPT") { rcpt_reply } else if line.starts_with("QUIT") { "221 bye" } else { "250 ok" };
                write.write_all(format!("{reply}\r\n").as_bytes()).await.unwrap();
            }
        });
        let transport: Transport = Transport::builder_dangerous("127.0.0.1").port(port).build();
        let message = Message::builder().from("a@example.com".parse().unwrap()).to("b@example.com".parse().unwrap()).body(String::new()).unwrap();
        transport.send(message).await.unwrap_err()
    }

    #[tokio::test]
    async fn only_an_unknown_address_counts_as_a_rejected_recipient() {
        for reply in ["550 5.1.1 The email account that you tried to reach does not exist", "553 sorry, no such user here"] {
            let err = send_rejected(reply).await;
            assert!(recipient_rejected(&err), "{err}");
        }
        for reply in ["550 5.4.5 Daily user sending limit exceeded", "550 5.7.1 Message rejected as spam", "554 5.7.1 Relay denied"] {
            let err = send_rejected(reply).await;
            assert!(!recipient_rejected(&err), "{err}");
        }
    }

    #[tokio::test]
    async fn only_a_sending_limit_counts_as_over_quota() {
        assert!(over_quota(&send_rejected("550 5.4.5 Daily user sending limit exceeded").await));
        for reply in ["550 5.1.1 No such user", "550 5.7.1 Message rejected as spam", "550 sorry"] {
            let err = send_rejected(reply).await;
            assert!(!over_quota(&err), "{err}");
        }
    }

    #[tokio::test]
    async fn only_a_server_that_never_answers_is_unreachable() {
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let err = test_on(port).await;
        assert!(unreachable(&err), "{err}");

        let refusing = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = refusing.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = refusing.accept().await.unwrap();
            socket.write_all(b"554 5.7.1 No thanks\r\n").await.unwrap();
        });
        let err = test_on(port).await;
        assert!(!unreachable(&err), "{err}");
    }
}
