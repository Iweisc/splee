/** The words in `text` from GET /spam-words, which spam filters often hold against a cold email. Matches like
 *  backend/src/spam_words.rs: any case, whole words, so "free" counts in "free trial" but not in "freelance". */
export function spamWords(text: string, words: readonly string[]): string[] {
  const lower = text.toLowerCase();
  // Whole characters, not UTF-16 halves, and the letters and numbers Rust's is_alphanumeric counts.
  const before = /[\p{Alphabetic}\p{N}]$/u;
  const after = /^[\p{Alphabetic}\p{N}]/u;
  return words.filter((word) => {
    for (let at = lower.indexOf(word); at >= 0; at = lower.indexOf(word, at + 1)) {
      if (!before.test(lower.slice(0, at)) && !after.test(lower.slice(at + word.length))) return true;
    }
    return false;
  });
}
