/** The words in `text` from GET /spam-words, which spam filters often hold against a cold email. Matches like
 *  backend/src/spam_words.rs: any case, whole words, so "free" counts in "free trial" but not in "freelance". */
export function spamWords(text: string, words: readonly string[]): string[] {
  const lower = text.toLowerCase();
  const edge = (c: string | undefined) => c === undefined || !/[\p{L}\p{N}]/u.test(c);
  return words.filter((word) => {
    for (let at = lower.indexOf(word); at >= 0; at = lower.indexOf(word, at + 1)) {
      if (edge(lower[at - 1]) && edge(lower[at + word.length])) return true;
    }
    return false;
  });
}
