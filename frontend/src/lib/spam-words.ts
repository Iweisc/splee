/** The words in `text` from GET /spam-words, which spam filters often hold against a cold email. Matches like
 *  backend/src/spam_words.rs: any case, whole words, so "free" counts in "free trial" but not in "freelance".
 *  Unlike the backend, a word only ever found inside a longer one that matched, like "free" in "100% free" or "risk-free",
 *  is left out, so the editor names that phrase once. */
export function spamWords(text: string, words: readonly string[]): string[] {
  const lower = text.toLowerCase();
  // Whole characters, not UTF-16 halves, and the letters and numbers Rust's is_alphanumeric counts. Two code units hold
  // any one character, so the checks look no further than that while someone types.
  const before = /[\p{Alphabetic}\p{N}]$/u;
  const after = /^[\p{Alphabetic}\p{N}]/u;
  const found: { word: string; start: number; end: number }[] = [];
  for (const word of words) {
    for (let i = lower.indexOf(word); i >= 0; i = lower.indexOf(word, i + 1)) {
      const end = i + word.length;
      if (!before.test(lower.slice(Math.max(0, i - 2), i)) && !after.test(lower.slice(end, end + 2))) found.push({ word, start: i, end });
    }
  }
  // Longest first at each start, so a match is inside a longer one exactly when an earlier one reaches as far.
  found.sort((a, b) => a.start - b.start || b.end - a.end);
  const named = new Set<string>();
  let reach = -1;
  for (const m of found) {
    if (m.end > reach) named.add(m.word);
    reach = Math.max(reach, m.end);
  }
  return words.filter((word) => named.has(word));
}
