//! The health verdict: each problem as a plain reason with what to do about it, and the worst of them as the verdict.
//! The lines come from thresholds.

use chrono::NaiveDate;
use serde::Serialize;

use super::blocklists::{Kind, List};
use super::thresholds::{self as t, Level, percent};

/// Leads a mailbox emailed in a window, and how many of them bounced, opted out or replied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, sqlx::FromRow)]
pub struct Rates {
    pub emailed: i64,
    pub bounced: i64,
    pub opted_out: i64,
    pub replied: i64,
}

/// `n` out of `of`, or None when nothing was sent to divide by.
pub fn rate(n: i64, of: i64) -> Option<f64> {
    (of > 0).then(|| n as f64 / of as f64)
}

impl Rates {
    pub fn add(self, other: Rates) -> Rates {
        Rates {
            emailed: self.emailed + other.emailed,
            bounced: self.bounced + other.bounced,
            opted_out: self.opted_out + other.opted_out,
            replied: self.replied + other.replied,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reason {
    /// Stable across checks, like "bounce" or "list:spamhaus_dbl:try-acme.com", so a mailbox resumed through a problem
    /// isn't held again for it.
    pub code: String,
    pub level: Level,
    pub text: String,
    pub fix: String,
}

impl Reason {
    fn new(code: impl Into<String>, level: Level, text: impl Into<String>, fix: &str) -> Self {
        Reason { code: code.into(), level, text: text.into(), fix: fix.into() }
    }
}

pub fn verdict(reasons: &[Reason]) -> Level {
    reasons.iter().map(|r| r.level).max().unwrap_or(Level::Good)
}

/// The codes of the reasons that hold a mailbox.
pub fn at_risk_codes(reasons: &[Reason]) -> Vec<String> {
    reasons.iter().filter(|r| r.level == Level::AtRisk).map(|r| r.code.clone()).collect()
}

/// A listing a target has now.
pub struct Listing<'a> {
    pub list: &'a List,
    pub target: &'a str,
    pub detail: &'a str,
}

/// A domain's day in Postmaster Tools.
#[derive(Clone, Debug, PartialEq, Serialize, sqlx::FromRow)]
pub struct PostmasterDay {
    pub domain: String,
    pub day: NaiveDate,
    pub reputation: String,
    pub spam_rate: Option<f64>,
    pub spf_rate: Option<f64>,
    pub dkim_rate: Option<f64>,
    pub dmarc_rate: Option<f64>,
}

const BOUNCE_FIX: &str = "Check the addresses in your lead lists and drop the ones that bounced before you send more. Guessed addresses bounce the most.";
const OPT_OUT_FIX: &str = "Email fewer people who fit less well, and make the first lines about them. Send fewer a day from this mailbox until it drops.";
const LOW_REPLY_FIX: &str = "Few replies often means the emails land in spam. Send one to a Gmail and an Outlook address you own and see where it lands.";
const WARMUP_FIX: &str = "Keep warmup on and send fewer campaign emails a day until most warmup emails reach the inbox again.";
const DOMAIN_LIST_FIX: &str = "Stop sending from this domain, find what got it listed, like a bought list or a hacked website, then ask for removal on the list's page.";
const IP_LIST_FIX: &str = "If you run this mail server, fix what got it listed and ask for removal. If it's your email provider's, ask them about it or send from another provider.";
const LOW_REPUTATION_FIX: &str = "Email only your best-fitting leads, fewer a day, until Google rates it Medium or High again. That can take a few weeks.";
const MEDIUM_REPUTATION_FIX: &str = "Keep bounces and spam reports low and the rating should climb back to High.";
const SPAM_FIX: &str = "Stop emailing people who haven't replied after a few follow-ups, and keep the unsubscribe link in every email.";
const AUTH_FIX: &str = "Check the domain's SPF, DKIM and DMARC records on the Mailboxes page, and turn on DKIM signing with your email provider.";

/// The window a rate is judged on: this week once enough leads were emailed in it, else the last 30 days.
fn window<'a>(week: &'a Rates, month: &'a Rates) -> Option<(&'a Rates, &'static str)> {
    if week.emailed >= t::MIN_EMAILED {
        Some((week, "this week"))
    } else if month.emailed >= t::MIN_EMAILED {
        Some((month, "in the last 30 days"))
    } else {
        None
    }
}

/// "Bounce rate 4.1% this week, above 3%", at the worst line it crosses.
fn over(code: &str, what: &str, value: f64, when: &str, lines: [(f64, Level); 2], fix: &str) -> Option<Reason> {
    let (line, level) = lines.into_iter().find(|(line, _)| value > *line)?;
    Some(Reason::new(code, level, format!("{what} {} {when}, above {}", percent(value), percent(line)), fix))
}

/// The problems Splee's own numbers and the mailbox's sending addresses show.
pub fn mailbox_reasons(week: &Rates, month: &Rates, ip_listings: &[Listing], warmup_spam: bool) -> Vec<Reason> {
    let mut reasons = vec![];
    if let Some((rates, when)) = window(week, month) {
        let bounce = rate(rates.bounced, rates.emailed).unwrap_or(0.0);
        reasons.extend(over("bounce", "Bounce rate", bounce, when, [(t::BOUNCE_AT_RISK, Level::AtRisk), (t::BOUNCE_WATCH, Level::Watch)], BOUNCE_FIX));
        let opt_out = rate(rates.opted_out, rates.emailed).unwrap_or(0.0);
        reasons.extend(over("opt_out", "Opt-out rate", opt_out, when, [(t::OPT_OUT_AT_RISK, Level::AtRisk), (t::OPT_OUT_WATCH, Level::Watch)], OPT_OUT_FIX));
    }
    if month.emailed >= t::LOW_REPLY_MIN_EMAILED {
        let replied = rate(month.replied, month.emailed).unwrap_or(0.0);
        if replied < t::LOW_REPLY {
            let text = format!("Reply rate {} in the last 30 days, under {}", percent(replied), percent(t::LOW_REPLY));
            reasons.push(Reason::new("low_reply", Level::Watch, text, LOW_REPLY_FIX));
        }
    }
    if warmup_spam {
        reasons.push(Reason::new("warmup_spam", Level::Watch, "Over 1 in 5 warmup emails landed in spam", WARMUP_FIX));
    }
    reasons.extend(ip_listings.iter().map(|l| listed(l)));
    reasons
}

fn listed(l: &Listing) -> Reason {
    let (text, fix) = match l.list.kind {
        Kind::Domain => (format!("Listed on {}", l.list.label), DOMAIN_LIST_FIX),
        Kind::Ip => (format!("Sending IP {} listed on {}", l.target, l.list.label), IP_LIST_FIX),
    };
    let text = if l.detail.is_empty() || l.detail == "Listed" { text } else { format!("{text} ({})", l.detail.to_lowercase()) };
    Reason::new(format!("list:{}:{}", l.list.id, l.target), l.list.level, text, fix)
}

/// The problems the domain itself has: blocklist listings, and Google's numbers when the day they're from is recent.
pub fn domain_reasons(listings: &[Listing], postmaster: Option<&PostmasterDay>, today: NaiveDate) -> Vec<Reason> {
    let mut reasons: Vec<Reason> = listings.iter().map(|l| listed(l)).collect();
    let Some(day) = postmaster.filter(|d| (today - d.day).num_days() <= t::POSTMASTER_FRESH_DAYS) else { return reasons };
    let level = t::reputation_level(&day.reputation);
    if level != Level::Good {
        let fix = if level == Level::AtRisk { LOW_REPUTATION_FIX } else { MEDIUM_REPUTATION_FIX };
        reasons.push(Reason::new("reputation", level, format!("Google rates the domain {}", title_case(&day.reputation)), fix));
    }
    if let Some(spam) = day.spam_rate {
        let when = format!("on {}", day.day.format("%b %-d"));
        reasons.extend(over("spam_rate", "Gmail spam rate", spam, &when, [(t::SPAM_AT_RISK, Level::AtRisk), (t::SPAM_WATCH, Level::Watch)], SPAM_FIX));
    }
    let checks = [("SPF", day.spf_rate), ("DKIM", day.dkim_rate), ("DMARC", day.dmarc_rate)];
    let failing: Vec<(&str, f64)> = checks.into_iter().filter_map(|(name, r)| Some((name, r?))).filter(|(_, r)| *r < t::AUTH_WATCH).collect();
    if let Some((name, worst)) = failing.iter().copied().min_by(|a, b| a.1.total_cmp(&b.1)) {
        let text = format!("Only {} of its Gmail mail passed {name}", percent(worst));
        reasons.push(Reason::new("auth", Level::Watch, text, AUTH_FIX));
    }
    reasons
}

fn title_case(word: &str) -> String {
    let lower = word.to_lowercase();
    let mut chars = lower.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deliverability::blocklists::list;

    fn rates(emailed: i64, bounced: i64, opted_out: i64, replied: i64) -> Rates {
        Rates { emailed, bounced, opted_out, replied }
    }

    fn texts(reasons: &[Reason]) -> Vec<(Level, &str)> {
        reasons.iter().map(|r| (r.level, r.text.as_str())).collect()
    }

    #[test]
    fn rates_need_something_sent() {
        assert_eq!(rate(1, 0), None);
        assert_eq!(rate(3, 100), Some(0.03));
        assert_eq!(rates(10, 1, 0, 2).add(rates(30, 2, 1, 1)), rates(40, 3, 1, 3));
    }

    #[test]
    fn bounces_are_judged_on_the_week_with_enough_sends() {
        let quiet = Rates::default();
        // 41 of 1000 is 4.1%, over the 3% line.
        let reasons = mailbox_reasons(&rates(1000, 41, 0, 30), &quiet, &[], false);
        assert_eq!(texts(&reasons), vec![(Level::AtRisk, "Bounce rate 4.1% this week, above 3%")]);
        assert_eq!(verdict(&reasons), Level::AtRisk);
        // 5 of 200 is 2.5%: watch. Exactly 3% isn't above the line.
        assert_eq!(texts(&mailbox_reasons(&rates(200, 5, 0, 10), &quiet, &[], false)), vec![(Level::Watch, "Bounce rate 2.5% this week, above 2%")]);
        assert_eq!(verdict(&mailbox_reasons(&rates(100, 3, 0, 5), &quiet, &[], false)), Level::Watch);
        assert_eq!(verdict(&mailbox_reasons(&rates(100, 2, 0, 5), &quiet, &[], false)), Level::Good);
        // A quiet week falls back to the month, and under 20 leads in both nothing is judged.
        let month = mailbox_reasons(&rates(5, 2, 0, 0), &rates(80, 2, 0, 3), &[], false);
        assert_eq!(texts(&month), vec![(Level::Watch, "Bounce rate 2.5% in the last 30 days, above 2%")]);
        assert!(mailbox_reasons(&rates(5, 3, 3, 0), &rates(19, 5, 5, 0), &[], false).is_empty());
    }

    #[test]
    fn opt_outs_low_replies_and_warmup_spam() {
        let reasons = mailbox_reasons(&rates(100, 0, 6, 0), &rates(400, 0, 9, 2), &[], true);
        assert_eq!(
            texts(&reasons),
            vec![
                (Level::AtRisk, "Opt-out rate 6% this week, above 5%"),
                (Level::Watch, "Reply rate 0.5% in the last 30 days, under 1%"),
                (Level::Watch, "Over 1 in 5 warmup emails landed in spam"),
            ]
        );
        // Low replies only count past 100 leads in the month.
        assert!(mailbox_reasons(&rates(0, 0, 0, 0), &rates(99, 0, 0, 0), &[], false).is_empty());
    }

    #[test]
    fn listings_name_the_list_and_spamcop_only_watches() {
        let (dbl, zen, spamcop) = (list("spamhaus_dbl").unwrap(), list("spamhaus_zen").unwrap(), list("spamcop").unwrap());
        let domain = domain_reasons(&[Listing { list: dbl, target: "try-acme.com", detail: "Spam domain" }], None, NaiveDate::MIN);
        assert_eq!(texts(&domain), vec![(Level::AtRisk, "Listed on Spamhaus DBL (spam domain)")]);
        assert_eq!(domain[0].code, "list:spamhaus_dbl:try-acme.com");
        let ips = [Listing { list: zen, target: "85.1.2.3", detail: "Listed" }, Listing { list: spamcop, target: "85.1.2.3", detail: "Listed" }];
        let reasons = mailbox_reasons(&Rates::default(), &Rates::default(), &ips, false);
        assert_eq!(
            texts(&reasons),
            vec![(Level::AtRisk, "Sending IP 85.1.2.3 listed on Spamhaus ZEN"), (Level::Watch, "Sending IP 85.1.2.3 listed on SpamCop")]
        );
        assert_eq!(at_risk_codes(&reasons), vec!["list:spamhaus_zen:85.1.2.3"]);
    }

    #[test]
    fn google_numbers_count_while_recent() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let day = |reputation: &str, spam: Option<f64>, dkim: Option<f64>, days_ago: u64| PostmasterDay {
            domain: "try-acme.com".into(),
            day: today - chrono::Days::new(days_ago),
            reputation: reputation.into(),
            spam_rate: spam,
            spf_rate: Some(1.0),
            dkim_rate: dkim,
            dmarc_rate: Some(0.99),
        };
        let low = domain_reasons(&[], Some(&day("LOW", Some(0.0035), Some(0.91), 1)), today);
        assert_eq!(
            texts(&low),
            vec![
                (Level::AtRisk, "Google rates the domain Low"),
                (Level::AtRisk, "Gmail spam rate 0.35% on Nov 10, above 0.3%"),
                (Level::Watch, "Only 91% of its Gmail mail passed DKIM"),
            ]
        );
        let medium = domain_reasons(&[], Some(&day("MEDIUM", Some(0.0015), None, 0)), today);
        assert_eq!(texts(&medium), vec![(Level::Watch, "Google rates the domain Medium"), (Level::Watch, "Gmail spam rate 0.15% on Nov 11, above 0.1%")]);
        assert!(domain_reasons(&[], Some(&day("HIGH", Some(0.0005), Some(0.99), 0)), today).is_empty());
        assert!(domain_reasons(&[], Some(&day("BAD", None, None, 8)), today).is_empty(), "a week-old day doesn't count");
    }
}
