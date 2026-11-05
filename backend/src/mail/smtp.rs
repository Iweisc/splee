use std::time::Duration;

use anyhow::Context;
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::Mailbox as Address;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::crypto;
use crate::models::Mailbox;
use crate::state::AppState;

pub type Transport = AsyncSmtpTransport<Tokio1Executor>;

pub fn transport(state: &AppState, mb: &Mailbox) -> anyhow::Result<Transport> {
    let host = mb.smtp_host.trim();
    let builder = match mb.smtp_security.as_str() {
        "tls" => Transport::relay(host)?,
        "starttls" => Transport::starttls_relay(host)?,
        _ => Transport::builder_dangerous(host),
    };
    let mut builder = builder.port(u16::try_from(mb.smtp_port).context("invalid SMTP port")?).timeout(Some(Duration::from_secs(30)));
    // An empty username means the server accepts mail without login (local relays, test servers).
    if !mb.smtp_username.trim().is_empty() {
        let password = crypto::decrypt(&state.cfg.app_secret, &mb.smtp_password_enc)?;
        builder = builder.credentials(Credentials::new(mb.smtp_username.trim().to_string(), password));
    }
    Ok(builder.build())
}

/// Connects, authenticates and says NOOP.
pub async fn test(state: &AppState, mb: &Mailbox) -> anyhow::Result<()> {
    let ok = transport(state, mb)?.test_connection().await.map_err(|e| anyhow::anyhow!("SMTP: {e}"))?;
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
    Ok(builder.body(email.body.to_string())?)
}

pub async fn send(transport: &Transport, message: Message) -> Result<(), lettre::transport::smtp::Error> {
    transport.send(message).await.map(|_| ())
}
