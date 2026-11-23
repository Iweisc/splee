export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

/** JSON, or a form or file sent as it is. */
type Body = Record<string, unknown> | unknown[] | FormData | Blob;

/**
 * Client-side fetch for the Rust API. Paths start after /api, e.g. api("/projects").
 * Throws ApiError with the backend's `{error}` message on non-2xx, and with status 0 when the request got no answer.
 */
export async function api<T>(path: string, init: { method?: string; body?: Body; signal?: AbortSignal } = {}): Promise<T> {
  // fetch sets the Content-Type of a form or a file itself.
  const raw = init.body instanceof FormData || init.body instanceof Blob;
  let res: Response;
  try {
    res = await fetch(`/api${path}`, {
      method: init.method ?? (init.body ? "POST" : "GET"),
      credentials: "same-origin",
      headers: init.body && !raw ? { "Content-Type": "application/json" } : undefined,
      body: init.body ? (raw ? (init.body as FormData | Blob) : JSON.stringify(init.body)) : undefined,
      signal: init.signal,
    });
  } catch (e) {
    // fetch only throws when no answer came back. Its own message is the browser's, like "Failed to fetch" or
    // "NetworkError when attempting to fetch resource.", which is what toasts and load errors would show.
    if (init.signal?.aborted) throw e;
    throw new ApiError(0, "Couldn't reach Splee. Check your connection and try again.");
  }
  if (!res.ok) {
    let error: unknown;
    try {
      error = (await res.json()).error;
    } catch {}
    // The backend always answers with a string {error}, and never with 502 to 504. Anything else came from what sits in front
    // of it while it's down or restarting, and its status text, like "Bad Gateway", tells nobody anything.
    const message = typeof error === "string" && error.trim() && ![502, 503, 504].includes(res.status) ? error : undefined;
    throw new ApiError(res.status, message ?? (res.status >= 500 ? "Splee isn't answering right now. Try again in a moment." : res.statusText));
  }
  if (res.status === 204) return undefined as T;
  return res.json() as Promise<T>;
}

export function qs(params: Record<string, string | number | boolean | null | undefined>): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== null && v !== undefined && v !== "") p.set(k, String(v));
  }
  const s = p.toString();
  return s ? `?${s}` : "";
}
