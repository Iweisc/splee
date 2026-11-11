//! Phone numbers in E.164, like +447700900123: the form leads' numbers are stored in, Twilio takes and sends back, and the
//! do-not-contact list holds. A number written the local way, like 07700 900123, takes its country's code from the lead's
//! or the project's country. Only countries in COUNTRIES are known that way. Anything else has to start with + or 00.

use chrono_tz::Tz;

/// ISO country, calling code, and the time zone when the whole country keeps one. Longer codes come before the shorter
/// ones they start with, so +1242 (Bahamas) wins over +1 when the country is looked up from a number.
const COUNTRIES: &[(&str, &str, Option<Tz>)] = &[
    ("US", "1", None),
    ("CA", "1", None),
    ("GB", "44", Some(Tz::Europe__London)),
    ("IE", "353", Some(Tz::Europe__Dublin)),
    ("FR", "33", Some(Tz::Europe__Paris)),
    ("DE", "49", Some(Tz::Europe__Berlin)),
    ("ES", "34", Some(Tz::Europe__Madrid)),
    ("PT", "351", Some(Tz::Europe__Lisbon)),
    ("IT", "39", Some(Tz::Europe__Rome)),
    ("NL", "31", Some(Tz::Europe__Amsterdam)),
    ("BE", "32", Some(Tz::Europe__Brussels)),
    ("LU", "352", Some(Tz::Europe__Luxembourg)),
    ("CH", "41", Some(Tz::Europe__Zurich)),
    ("AT", "43", Some(Tz::Europe__Vienna)),
    ("DK", "45", Some(Tz::Europe__Copenhagen)),
    ("SE", "46", Some(Tz::Europe__Stockholm)),
    ("NO", "47", Some(Tz::Europe__Oslo)),
    ("FI", "358", Some(Tz::Europe__Helsinki)),
    ("PL", "48", Some(Tz::Europe__Warsaw)),
    ("CZ", "420", Some(Tz::Europe__Prague)),
    ("GR", "30", Some(Tz::Europe__Athens)),
    ("RO", "40", Some(Tz::Europe__Bucharest)),
    ("HU", "36", Some(Tz::Europe__Budapest)),
    ("UA", "380", Some(Tz::Europe__Kyiv)),
    ("TR", "90", Some(Tz::Europe__Istanbul)),
    ("IL", "972", Some(Tz::Asia__Jerusalem)),
    ("AE", "971", Some(Tz::Asia__Dubai)),
    ("SA", "966", Some(Tz::Asia__Riyadh)),
    ("IN", "91", Some(Tz::Asia__Kolkata)),
    ("PK", "92", Some(Tz::Asia__Karachi)),
    ("SG", "65", Some(Tz::Asia__Singapore)),
    ("MY", "60", Some(Tz::Asia__Kuala_Lumpur)),
    ("PH", "63", Some(Tz::Asia__Manila)),
    ("HK", "852", Some(Tz::Asia__Hong_Kong)),
    ("JP", "81", Some(Tz::Asia__Tokyo)),
    ("KR", "82", Some(Tz::Asia__Seoul)),
    ("CN", "86", Some(Tz::Asia__Shanghai)),
    ("AU", "61", None),
    ("NZ", "64", Some(Tz::Pacific__Auckland)),
    ("ZA", "27", Some(Tz::Africa__Johannesburg)),
    ("NG", "234", Some(Tz::Africa__Lagos)),
    ("KE", "254", Some(Tz::Africa__Nairobi)),
    ("EG", "20", Some(Tz::Africa__Cairo)),
    ("MX", "52", None),
    ("BR", "55", None),
    ("AR", "54", Some(Tz::America__Argentina__Buenos_Aires)),
    ("CO", "57", Some(Tz::America__Bogota)),
    ("CL", "56", Some(Tz::America__Santiago)),
];

/// Where carriers or the law expect a marketing text to say how to opt out, so the first text to a number there ends with
/// a STOP line: the US and Canada (CTIA and CASL), the UK, France, Australia and New Zealand.
const STOP_LINE_COUNTRIES: [&str; 6] = ["US", "CA", "GB", "FR", "AU", "NZ"];

pub const STOP_LINE: &str = "Reply STOP to opt out.";

/// `raw` in E.164, or None when it can't be one. `country` is the ISO code to read a local number by.
pub fn normalize(raw: &str, country: &str) -> Option<String> {
    let raw = raw.trim().trim_start_matches("whatsapp:").trim();
    // Only the separators people write numbers with. Letters, or an extension, mean it isn't a plain number.
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit() || " +-.()/".contains(c)) || raw.rfind('+').is_some_and(|i| i > 0) {
        return None;
    }
    let international = raw.starts_with('+') || raw.starts_with("00");
    // +44 (0)7700 900123: the (0) is only dialled from inside the country.
    let raw = if international { raw.replace("(0)", "") } else { raw.to_string() };
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    let full = if raw.starts_with('+') {
        digits
    } else if let Some(rest) = digits.strip_prefix("00") {
        rest.to_string()
    } else {
        let code = COUNTRIES.iter().find(|(iso, ..)| iso.eq_ignore_ascii_case(country.trim()))?.1;
        if code == "1" {
            // North American numbers are 10 digits, sometimes written with the 1 in front.
            match digits.len() {
                10 => format!("1{digits}"),
                11 if digits.starts_with('1') => digits,
                _ => return None,
            }
        } else if country.trim().eq_ignore_ascii_case("IT") {
            // Italy keeps the leading 0 of a landline in the international form.
            format!("{code}{digits}")
        } else {
            format!("{code}{}", digits.strip_prefix('0').unwrap_or(&digits))
        }
    };
    // E.164 allows 15 digits. Under 8 is no country's full number, and a 0 is never the start of a calling code.
    ((8..=15).contains(&full.len()) && !full.starts_with('0')).then(|| format!("+{full}"))
}

/// The country a number in E.164 belongs to, among the ones Splee knows. +1 reads as the US, which keeps the same rules.
pub fn country(e164: &str) -> Option<&'static str> {
    let digits = e164.strip_prefix('+')?;
    let mut known: Vec<_> = COUNTRIES.iter().filter(|(_, code, _)| digits.starts_with(code)).collect();
    known.sort_by_key(|(_, code, _)| std::cmp::Reverse(code.len()));
    known.first().map(|(iso, ..)| *iso)
}

/// The lead's time zone, from their number, else their country, when that country keeps one time zone.
pub fn time_zone(e164: &str, country_code: &str) -> Option<Tz> {
    let zone = |iso: &str| COUNTRIES.iter().find(|(c, ..)| c.eq_ignore_ascii_case(iso)).and_then(|(.., tz)| *tz);
    country(e164).and_then(zone).or_else(|| zone(country_code.trim()))
}

pub fn needs_stop_line(e164: &str) -> bool {
    country(e164).is_some_and(|c| STOP_LINE_COUNTRIES.contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_come_out_in_e164() {
        for (raw, country, want) in [
            ("+44 7700 900123", "", "+447700900123"),
            ("07700 900123", "GB", "+447700900123"),
            ("0044 (0)7700-900123", "", "+447700900123"),
            ("(415) 555-0134", "US", "+14155550134"),
            ("1-415-555-0134", "us", "+14155550134"),
            ("415.555.0134", "CA", "+14155550134"),
            ("06 12 34 56 78", "FR", "+33612345678"),
            ("030 1234567", "DE", "+49301234567"),
            ("06 1234 5678", "IT", "+390612345678"),
            ("whatsapp:+61412345678", "", "+61412345678"),
        ] {
            assert_eq!(normalize(raw, country).as_deref(), Some(want), "{raw} in {country}");
        }
        for (raw, country) in [
            ("07700 900123", ""),
            ("07700 900123", "ZZ"),
            ("555-0134", "US"),
            ("+44 7700 ext 12", ""),
            ("44+7700900123", ""),
            ("+1234", ""),
            ("+1234567890123456", ""),
            ("", "GB"),
        ] {
            assert_eq!(normalize(raw, country), None, "{raw} in {country}");
        }
    }

    #[test]
    fn a_number_names_its_country_and_time_zone() {
        assert_eq!(country("+14155550134"), Some("US"));
        assert_eq!(country("+353861234567"), Some("IE"));
        assert_eq!(country("+35123"), Some("PT"));
        assert_eq!(country("+999123456789"), None);
        assert_eq!(time_zone("+447700900123", "US"), Some(Tz::Europe__London));
        // Several time zones, so the country can't say which. A known country of a lead still can.
        assert_eq!(time_zone("+14155550134", ""), None);
        assert_eq!(time_zone("+999123456789", "DE"), Some(Tz::Europe__Berlin));
        assert!(needs_stop_line("+14155550134") && needs_stop_line("+447700900123"));
        assert!(!needs_stop_line("+491701234567"));
    }
}
