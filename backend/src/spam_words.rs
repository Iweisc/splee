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

/// Each of WORDS in `text`, in WORDS' order, any case, as a whole word: "free" in "free trial" but not in "freelance". A word
/// only ever found inside a longer one that matched, like "free" in "100% free" or "risk-free", is left out, as in the editor.
pub fn found(text: &str) -> Vec<&'static str> {
    let text = text.to_lowercase();
    let edge = |c: Option<char>| !c.is_some_and(char::is_alphanumeric);
    // Every start, not match_indices, which skips overlapping ones: "$$$" in "a$$$$" only stands alone one dollar in.
    let mut matches: Vec<(usize, usize, &str)> = WORDS
        .iter()
        .flat_map(|word| (0..text.len()).map(move |at| (at, at + word.len(), *word)))
        .filter(|&(at, end, word)| {
            text.is_char_boundary(at)
                && text[at..].starts_with(word)
                && edge(text[..at].chars().next_back())
                && edge(text[end..].chars().next())
        })
        .collect();
    // Longest first at each start, so a match is inside a longer one exactly when an earlier one reaches as far.
    matches.sort_by_key(|&(at, end, _)| (at, std::cmp::Reverse(end)));
    let mut named = Vec::new();
    let mut reach = 0;
    for (_, end, word) in matches {
        if end > reach {
            named.push(word);
        }
        reach = reach.max(end);
    }
    WORDS.iter().copied().filter(|word| named.contains(word)).collect()
}

#[cfg(test)]
mod tests {
    use super::found;

    #[test]
    fn finds_whole_words_in_any_case() {
        assert_eq!(found("Start a FREE trial, guaranteed. Click here!"), ["click here", "free", "guaranteed"]);
        assert_eq!(found("Freelance designers get a guarantee-free quote"), ["free", "guarantee"]);
        assert!(found("Saw your post on hiring a freelancer. Worth a call?").is_empty());
        assert_eq!(found("Margins up 100% guaranteed $$$"), ["100% guaranteed", "$$$"]);
        assert_eq!(found("a$$$$"), ["$$$"]);
        assert_eq!(found("Café, 100% guaranteed. Ça va?"), ["100% guaranteed"]);
        assert_eq!(found("It's risk-free, and free to try"), ["free", "risk-free"]);
        assert_eq!(found("100% free, risk-free"), ["100% free", "risk-free"]);
    }
}
