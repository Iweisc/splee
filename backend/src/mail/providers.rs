//! The SMTP and IMAP settings of the email providers people connect, recognized from where their domain's mail goes,
//! so connecting a mailbox needs only the address and a password.

use serde::Serialize;

#[derive(Debug, PartialEq, Serialize)]
pub struct Settings {
    pub provider: &'static str,
    pub smtp_host: String,
    pub smtp_port: u16,
    /// "tls" for 465, "starttls" for 587, as the mailbox form takes them.
    pub smtp_security: &'static str,
    pub imap_host: String,
    pub imap_port: u16,
}

fn settings(provider: &'static str, smtp: &str, smtp_port: u16, imap: &str) -> Settings {
    let smtp_security = if smtp_port == 465 { "tls" } else { "starttls" };
    Settings { provider, smtp_host: smtp.to_string(), smtp_port, smtp_security, imap_host: imap.to_string(), imap_port: 993 }
}

/// The provider behind the first MX host it recognizes, with the hosts in preference order.
pub fn from_mx(hosts: &[String]) -> Option<Settings> {
    hosts.iter().find_map(|host| provider(&host.to_lowercase()))
}

fn provider(host: &str) -> Option<Settings> {
    let under = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    // Zoho and IONOS run servers per region, named after the region's domain: mx.zoho.eu sends through smtp.zoho.eu.
    let region = |name: &str, tlds: &[&'static str]| tlds.iter().copied().find(|tld| under(&format!("{name}.{tld}")));
    Some(if under("google.com") || under("googlemail.com") {
        settings("Google Workspace", "smtp.gmail.com", 587, "imap.gmail.com")
    } else if under("mail.protection.outlook.com") || under("mx.microsoft") {
        settings("Microsoft 365", "smtp.office365.com", 587, "outlook.office365.com")
    } else if under("zohomail.com") {
        settings("Zoho", "smtp.zoho.com", 465, "imap.zoho.com")
    } else if let Some(tld) = region("zoho", &["com", "eu", "in", "com.au", "jp", "ca", "sa", "uk", "com.cn"]) {
        settings("Zoho", &format!("smtp.zoho.{tld}"), 465, &format!("imap.zoho.{tld}"))
    } else if under("kundenserver.de") {
        settings("IONOS", "smtp.ionos.de", 587, "imap.ionos.de")
    } else if let Some(tld) = region("ionos", &["com", "de", "co.uk", "es", "fr", "it", "mx"]) {
        settings("IONOS", &format!("smtp.ionos.{tld}"), 587, &format!("imap.ionos.{tld}"))
    } else if under("messagingengine.com") {
        settings("Fastmail", "smtp.fastmail.com", 465, "imap.fastmail.com")
    } else if under("mail.icloud.com") {
        settings("iCloud Mail", "smtp.mail.me.com", 587, "imap.mail.me.com")
    } else if under("yahoodns.net") {
        settings("Yahoo Mail", "smtp.mail.yahoo.com", 465, "imap.mail.yahoo.com")
    } else if under("secureserver.net") {
        settings("GoDaddy", "smtpout.secureserver.net", 465, "imap.secureserver.net")
    } else if under("privateemail.com") {
        settings("Namecheap Private Email", "mail.privateemail.com", 465, "mail.privateemail.com")
    } else if under("hostinger.com") {
        settings("Hostinger", "smtp.hostinger.com", 465, "imap.hostinger.com")
    } else if under("migadu.com") {
        settings("Migadu", "smtp.migadu.com", 465, "imap.migadu.com")
    } else if under("yandex.net") || under("yandex.ru") {
        settings("Yandex 360", "smtp.yandex.com", 465, "imap.yandex.com")
    } else {
        return None;
    })
}

/// The two providers most leads' mail goes to. Their filters trust mail from their own servers more, so provider matching
/// (mail/sender.rs pick_from) sends a lead on one from a mailbox on the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Google,
    Microsoft,
}

/// The family of a mail server, an MX host or a mailbox's SMTP host. Outlook.com and Hotmail addresses take mail at
/// *.olc.protection.outlook.com, and Microsoft 365 at *.mail.protection.outlook.com or *.mx.microsoft.
pub fn family(host: &str) -> Option<Family> {
    let host = host.trim().trim_end_matches('.').to_lowercase();
    let under = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    if ["google.com", "googlemail.com", "gmail.com"].iter().any(|d| under(d)) {
        Some(Family::Google)
    } else if ["outlook.com", "office365.com", "mx.microsoft"].iter().any(|d| under(d)) {
        Some(Family::Microsoft)
    } else {
        None
    }
}

/// The family of a domain's mail, from its MX hosts in preference order.
pub fn family_of_mx(hosts: &[String]) -> Option<Family> {
    hosts.iter().find_map(|h| family(h))
}

/// A mailbox's family: the provider it signed in with, else where it sends from.
pub fn mailbox_family(provider: &str, smtp_host: &str) -> Option<Family> {
    match provider {
        "google" => Some(Family::Google),
        "microsoft" => Some(Family::Microsoft),
        _ => family(smtp_host),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_google_and_microsoft_mail_apart() {
        let mx = |list: &[&str]| family_of_mx(&hosts(list));
        assert_eq!(mx(&["gmail-smtp-in.l.google.com", "alt1.gmail-smtp-in.l.google.com"]), Some(Family::Google));
        assert_eq!(mx(&["aspmx.l.google.com"]), Some(Family::Google));
        assert_eq!(mx(&["hotmail-com.olc.protection.outlook.com"]), Some(Family::Microsoft));
        assert_eq!(mx(&["acme-com.mail.protection.outlook.com"]), Some(Family::Microsoft));
        assert_eq!(mx(&["acme-com.p-v1.mx.microsoft."]), Some(Family::Microsoft));
        // A filtering service in front decides nothing, so the first host that says wins.
        assert_eq!(mx(&["mx1.mimecast.example", "acme-com.mail.protection.outlook.com"]), Some(Family::Microsoft));
        assert_eq!(mx(&["mx.zoho.eu"]), None);
        assert_eq!(mx(&["notgoogle.com"]), None);
        assert_eq!(mx(&[]), None);
        assert_eq!(mailbox_family("password", "smtp.gmail.com"), Some(Family::Google));
        assert_eq!(mailbox_family("password", "smtp.office365.com"), Some(Family::Microsoft));
        assert_eq!(mailbox_family("password", "smtp-mail.outlook.com"), Some(Family::Microsoft));
        assert_eq!(mailbox_family("google", ""), Some(Family::Google));
        assert_eq!(mailbox_family("microsoft", ""), Some(Family::Microsoft));
        assert_eq!(mailbox_family("password", "smtp.zoho.com"), None);
    }

    fn hosts(list: &[&str]) -> Vec<String> {
        list.iter().map(|h| h.to_string()).collect()
    }

    #[test]
    fn recognizes_providers_by_their_mx() {
        let found = |list: &[&str]| from_mx(&hosts(list)).map(|s| (s.provider, s.smtp_host, s.smtp_port, s.smtp_security, s.imap_host));
        let google = Some(("Google Workspace", "smtp.gmail.com".into(), 587, "starttls", "imap.gmail.com".into()));
        assert_eq!(found(&["aspmx.l.google.com", "alt1.aspmx.l.google.com"]), google);
        assert_eq!(found(&["acme-com.mail.protection.outlook.com"]).unwrap().1, "smtp.office365.com");
        assert_eq!(found(&["acme-com.p-v1.mx.microsoft"]).unwrap().1, "smtp.office365.com");
        assert_eq!(found(&["mx.zoho.eu", "mx2.zoho.eu"]).unwrap().1, "smtp.zoho.eu");
        assert_eq!(found(&["mx.zoho.com.au"]).unwrap().4, "imap.zoho.com.au");
        assert_eq!(found(&["mx.zohomail.com"]).unwrap().1, "smtp.zoho.com");
        assert_eq!(found(&["mx00.ionos.com"]).unwrap().1, "smtp.ionos.com");
        assert_eq!(found(&["mx00.kundenserver.de"]).unwrap().1, "smtp.ionos.de");
        assert_eq!(found(&["IN1-SMTP.messagingengine.com"]).unwrap().0, "Fastmail");
        assert_eq!(found(&["mx01.mail.icloud.com"]).unwrap().1, "smtp.mail.me.com");
        assert_eq!(found(&["mailstore1.secureserver.net"]).unwrap().3, "tls");
        // The first host decides, and an unknown one is skipped for the next.
        assert_eq!(found(&["mx.unknown-filter.net", "mx1.privateemail.com"]).unwrap().0, "Namecheap Private Email");
        // A lookalike isn't the provider.
        assert_eq!(found(&["mx.notgoogle.com", "zoho.attacker.net", "fakeionos.com", "mx.ionos.ca"]), None);
        assert_eq!(found(&[]), None);
    }
}
