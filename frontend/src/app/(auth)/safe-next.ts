/** Only same-site paths, so ?next= can't bounce people to another domain after they sign in. */
export function safeNext(next: string | string[] | undefined): string | null {
  return typeof next === "string" && next.startsWith("/") && !next.startsWith("//") ? next : null;
}
