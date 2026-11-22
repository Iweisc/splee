//! Text helpers shared across features.

/// Lowercase words only, so two texts match whatever their spacing, punctuation and line breaks. Product feedback checks
/// quotes against their source with it, and the answer library keys questions with it.
pub fn plain(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn plain_keeps_lowercase_words() {
        assert_eq!(super::plain(" \"Salesforce  SYNC!\"\n"), "salesforce sync");
        assert_eq!(super::plain("Do you encrypt data at rest?"), super::plain("do you  encrypt data at rest"));
        assert_eq!(super::plain("Übertragung"), "übertragung");
    }
}
