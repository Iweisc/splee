/** "us" -> "🇺🇸". Returns "" for anything that isn't a two-letter code. */
export function flag(countryCode: string | undefined): string {
  if (!countryCode || !/^[a-z]{2}$/i.test(countryCode)) return "";
  return String.fromCodePoint(...[...countryCode.toUpperCase()].map((c) => 0x1f1e6 + c.charCodeAt(0) - 65));
}
