"use client";

import useSWR from "swr";
import { buttonClass } from "@/components/ui/button";
import { fetcher } from "@/hooks/use-me";
import { qs } from "@/lib/api";
import type { SsoProvider } from "@/lib/types";

export const SSO_LABELS: Record<SsoProvider, string> = { google: "Google", microsoft: "Microsoft" };

type StartParams = Record<string, string | null | undefined>;

/** A full page load rather than a fetch, since the API answers with a redirect to the provider. */
export function ssoStartUrl(provider: SsoProvider, params: StartParams): string {
  return `/api/auth/sso/${provider}/start${qs(params)}`;
}

export function SsoIcon({ provider }: { provider: SsoProvider }) {
  if (provider === "microsoft") {
    return (
      <svg viewBox="0 0 21 21" aria-hidden className="size-4 shrink-0">
        <rect x="1" y="1" width="9" height="9" fill="#f25022" />
        <rect x="11" y="1" width="9" height="9" fill="#7fba00" />
        <rect x="1" y="11" width="9" height="9" fill="#00a4ef" />
        <rect x="11" y="11" width="9" height="9" fill="#ffb900" />
      </svg>
    );
  }
  return (
    <svg viewBox="0 0 48 48" aria-hidden className="size-4 shrink-0">
      <path fill="#ea4335" d="M24 9.5c3.5 0 6.6 1.2 9.1 3.6l6.8-6.8C35.8 2.4 30.2 0 24 0 14.6 0 6.6 5.4 2.7 13.3l7.9 6.1C12.5 13.6 17.8 9.5 24 9.5z" />
      <path fill="#4285f4" d="M46.1 24.5c0-1.6-.1-3.1-.4-4.5H24v9h12.4c-.5 2.9-2.2 5.3-4.6 6.9l7.4 5.7c4.3-4 6.9-9.9 6.9-17.1z" />
      <path fill="#fbbc05" d="M10.5 28.6c-.5-1.4-.8-3-.8-4.6s.3-3.2.8-4.6l-7.9-6.1C1 16.6 0 20.2 0 24s1 7.4 2.7 10.7l7.8-6.1z" />
      <path fill="#34a853" d="M24 48c6.5 0 11.9-2.1 15.9-5.8l-7.4-5.7c-2.1 1.4-4.8 2.3-8.5 2.3-6.2 0-11.5-4.1-13.4-9.8l-7.9 6.1C6.6 42.6 14.6 48 24 48z" />
    </svg>
  );
}

/**
 * Continue with Google and Continue with Microsoft, for the providers this Splee has an app for, above the email form.
 * `params` go through the sign-in and come back on this page with any error: from, next, invite, claim and token.
 */
export function SsoButtons({ params }: { params: StartParams }) {
  const { data } = useSWR<{ providers: SsoProvider[] }>("/auth/sso", fetcher);
  const providers = data?.providers ?? [];
  if (providers.length === 0) return null;
  return (
    <div className="mt-6 flex flex-col gap-2">
      {providers.map((p) => (
        <a key={p} href={ssoStartUrl(p, params)} className={buttonClass("outline", "md", "w-full")}>
          <SsoIcon provider={p} />
          Continue with {SSO_LABELS[p]}
        </a>
      ))}
      <div className="mt-4 -mb-2 flex items-center gap-3 text-xs text-muted-foreground">
        <span aria-hidden className="h-px flex-1 bg-border" />
        or with your email
        <span aria-hidden className="h-px flex-1 bg-border" />
      </div>
    </div>
  );
}
