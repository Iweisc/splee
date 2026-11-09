export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

type Body = Record<string, unknown> | unknown[] | FormData;

/**
 * Client-side fetch for the Rust API. Paths start after /api, e.g. api("/projects").
 * Throws ApiError with the backend's `{error}` message on non-2xx, and with status 0 when the request got no answer.
 */
export async function api<T>(path: string, init: { method?: string; body?: Body; signal?: AbortSignal } = {}): Promise<T> {
  const isForm = init.body instanceof FormData;
  let res: Response;
  try {
    res = await fetch(`/api${path}`, {
      method: init.method ?? (init.body ? "POST" : "GET"),
      credentials: "same-origin",
      headers: init.body && !isForm ? { "Content-Type": "application/json" } : undefined,
      body: init.body ? (isForm ? (init.body as FormData) : JSON.stringify(init.body)) : undefined,
      signal: init.signal,
    });
  } catch (e) {
    // fetch only throws when no answer came back. Its own message is the browser's, like "Failed to fetch" or
    // "NetworkError when attempting to fetch resource.", which is what toasts and load errors would show.
    if (init.signal?.aborted) throw e;
    throw new ApiError(0, "Couldn't reach Splee. Check your connection and try again.");
  }
  if (!res.ok) {
    let message = res.statusText;
    try {
      message = (await res.json()).error ?? message;
    } catch {}
    throw new ApiError(res.status, message);
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
