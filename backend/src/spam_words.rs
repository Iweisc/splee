//! Words and phrases spam filters often hold against a cold email. The draft editor and MCP flag them for whoever reviews it,
//! and the writer is told to keep clear of them. frontend/src/lib/spam-words.ts matches them the same way.

/// Lowercase. The editor gets them from GET /spam-words, so adding one here is enough.
pub const WORDS: &[&str] = &[
    "100% free",
    "100% guaranteed",
    "act fast",
    "act now",
    "apply now",
    "as seen on",
    "best price",
    "buy now",
    "cash bonus",
    "click below",
    "click here",
    "congratulations",
    "dear friend",
    "double your",
    "earn extra",
    "earn money",
    "exclusive deal",
    "extra income",
    "free",
    "guarantee",
    "guaranteed",
    "incredible deal",
    "limited time",
    "lowest price",
    "make money",
    "no catch",
    "no cost",
    "no obligation",
    "no risk",
    "once in a lifetime",
    "order now",
    "risk free",
    "risk-free",
    "special promotion",
    "this is not spam",
    "this isn't spam",
    "urgent",
    "winner",
    "you have been selected",
    "$$$",
];

/// Each of WORDS in `text`, in WORDS' order, any case, as a whole word: "free" in "free trial" but not in "freelance".
pub fn found(text: &str) -> Vec<&'static str> {
    let text = text.to_lowercase();
    let edge = |c: Option<char>| !c.is_some_and(char::is_alphanumeric);
    WORDS
        .iter()
        .copied()
        .filter(|word| {
            text.match_indices(word).any(|(at, _)| edge(text[..at].chars().next_back()) && edge(text[at + word.len()..].chars().next()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::found;

    #[test]
    fn finds_whole_words_in_any_case() {
        assert_eq!(found("Start a FREE trial, guaranteed. Click here!"), ["click here", "free", "guaranteed"]);
        assert_eq!(found("Freelance designers get a guarantee-free quote"), ["free", "guarantee"]);
        assert!(found("Saw your post on hiring a freelancer. Worth a call?").is_empty());
        assert_eq!(found("Margins up 100% guaranteed $$$"), ["100% guaranteed", "guaranteed", "$$$"]);
    }
}
