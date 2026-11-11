//! Every line the health verdict draws, in one place. Rates are fractions, so 0.03 is 3%. Keep the copy on the
//! Deliverability page in sync when one changes.

use serde::Serialize;

/// How bad a problem is. At risk holds the mailbox from sending.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Good,
    Watch,
    AtRisk,
}

/// Under this many leads emailed in a window, one bounce swings the rate too far to judge by.
pub const MIN_EMAILED: i64 = 20;

/// Past 2% bounced, Gmail and Outlook start to notice a sender is careless with its lists.
pub const BOUNCE_WATCH: f64 = 0.02;
/// Past 3%, mailbox providers send more of the domain's mail to spam.
pub const BOUNCE_AT_RISK: f64 = 0.03;

/// Replies asking to stop and unsubscribe clicks. A couple in a hundred is normal for cold email.
pub const OPT_OUT_WATCH: f64 = 0.02;
/// Past 5% the emails read as unwanted, and many who don't opt out press Report spam instead.
pub const OPT_OUT_AT_RISK: f64 = 0.05;

/// Under 1 reply in 100 over 30 days, the emails likely sit in spam where nobody reads them.
pub const LOW_REPLY: f64 = 0.01;
/// The low reply line only counts once this many leads were emailed in the 30 days.
pub const LOW_REPLY_MIN_EMAILED: i64 = 100;

/// Google's bulk sender rules: keep Gmail's user-reported spam rate under 0.1%.
pub const SPAM_WATCH: f64 = 0.001;
/// And never let it reach 0.3%, where Gmail starts rejecting the domain's mail.
pub const SPAM_AT_RISK: f64 = 0.003;

/// Under 98% passing SPF, DKIM or DMARC, some of the domain's mail fails a check Gmail requires.
pub const AUTH_WATCH: f64 = 0.98;

/// Postmaster numbers older than this say nothing about today, so they're shown but don't count.
pub const POSTMASTER_FRESH_DAYS: i64 = 7;

/// Google's domain reputation: Medium is worth watching, Low and Bad mean much of the mail already goes to spam.
pub fn reputation_level(reputation: &str) -> Level {
    match reputation {
        "LOW" | "BAD" => Level::AtRisk,
        "MEDIUM" => Level::Watch,
        _ => Level::Good,
    }
}

/// A rate as the page writes it: one decimal from 1% up, two below, without trailing zeros, like "4.1%" or "0.25%".
pub fn percent(rate: f64) -> String {
    let pct = rate * 100.0;
    let text = if pct >= 1.0 { format!("{pct:.1}") } else { format!("{pct:.2}") };
    let text = if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.') } else { &text };
    format!("{text}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percents_read_like_people_write_them() {
        assert_eq!(percent(0.041), "4.1%");
        assert_eq!(percent(0.03), "3%");
        assert_eq!(percent(0.001), "0.1%");
        assert_eq!(percent(0.0025), "0.25%");
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(1.0), "100%");
    }
}
