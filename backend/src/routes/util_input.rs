use crate::error::{AppError, AppResult};

/// Lowercased address when it has basic `local@domain.tld` syntax.
pub fn email(input: &str) -> Option<String> {
    let email = input.trim().to_lowercase();
    let (local, domain) = email.split_once('@')?;
    let valid = !local.is_empty()
        && domain_like(domain)
        && !local.chars().any(|c| c.is_whitespace() || matches!(c, '@' | ',' | ';' | '<' | '>' | '"'));
    valid.then_some(email)
}

/// "https://www.Acme.com/about" becomes "acme.com".
pub fn domain(input: &str) -> Option<String> {
    let lower = input.trim().to_lowercase();
    let without_scheme = lower.split_once("://").map_or(lower.as_str(), |(_, rest)| rest);
    let host = without_scheme.split(['/', '?', '#', ':']).next().unwrap_or("");
    let host = host.strip_prefix("www.").unwrap_or(host);
    domain_like(host).then(|| host.to_string())
}

fn domain_like(host: &str) -> bool {
    host.contains('.')
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
        && !host.contains("..")
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

/// ILIKE pattern for a free-text `q` filter. None when the filter is empty.
pub fn like_pattern(q: Option<&str>) -> Option<String> {
    let q = q.map(str::trim).filter(|q| !q.is_empty())?;
    Some(format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")))
}

/// Mirrors the check constraints in the migration so bad input is a 400 instead of a 500.
pub fn one_of(field: &str, value: &str, allowed: &[&str]) -> AppResult<()> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!("{field} must be one of: {}", allowed.join(", "))))
    }
}

pub const LEAD_STATUSES: &[&str] = &[
    "new",
    "queued",
    "contacted",
    "replied",
    "hot",
    "meeting_booked",
    "not_interested",
    "bounced",
    "unsubscribed",
    "do_not_contact",
];
