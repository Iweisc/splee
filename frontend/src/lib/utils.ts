import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** micro-dollars -> "$1.23" */
export function money(micros: number, opts: { cents?: boolean } = {}): string {
  const usd = micros / 1_000_000;
  const digits = opts.cents === false ? 0 : 2;
  return usd.toLocaleString("en-US", { style: "currency", currency: "USD", minimumFractionDigits: digits, maximumFractionDigits: digits });
}

/** A deal or price in its currency's main unit: "$12,000", or "$187.50" when it has cents. */
export function dealMoney(value: number, currency: string): string {
  const digits = Number.isInteger(value) ? 0 : 2;
  return value.toLocaleString("en-US", { style: "currency", currency, minimumFractionDigits: digits, maximumFractionDigits: digits });
}

/** 120000 -> "120.0K", 4500000 -> "4.5M" (matches the campaign sizes in the Explee demo) */
export function compact(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
}

/** One decimal under 10%, where it matters most for reply rates. */
export function percent(ratio: number): string {
  const p = ratio * 100;
  return `${p < 10 ? p.toFixed(1) : Math.round(p)}%`;
}

export function num(n: number): string {
  return n.toLocaleString("en-US");
}

/** "1 lead", "2,340 leads". */
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${num(n)} ${n === 1 ? one : many}`;
}

/** "3h ago", or "in 3h" for a time still ahead, like an email's scheduled send. */
export function timeAgo(ts: string): string {
  const diff = Math.round((Date.now() - new Date(ts).getTime()) / 1000);
  const s = Math.abs(diff);
  const say = (span: string) => (diff < 0 ? `in ${span}` : `${span} ago`);
  if (s < 60) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return say(`${m} min`);
  const h = Math.round(m / 60);
  if (h < 24) return say(`${h}h`);
  const d = Math.round(h / 24);
  if (d < 30) return say(`${d}d`);
  return new Date(ts).toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
}

export function dateTime(ts: string): string {
  return new Date(ts).toLocaleString("en-US", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

/** With the weekday, for meetings people plan around: "Thu, Nov 12, 03:00 PM". */
export function dayTime(ts: string): string {
  return new Date(ts).toLocaleString("en-US", { weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

export function isPast(ts: string): boolean {
  return new Date(ts).getTime() <= Date.now();
}

export function favicon(domain: string, size = 32): string {
  return `https://www.google.com/s2/favicons?domain=${encodeURIComponent(domain)}&sz=${size}`;
}

export function initials(name: string): string {
  return name
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((p) => p[0]!.toUpperCase())
    .join("");
}

/** "https://www.Acme.com/about" -> "acme.com" */
export function normalizeDomain(input: string): string {
  return input
    .trim()
    .toLowerCase()
    .replace(/^https?:\/\//, "")
    .replace(/^www\./, "")
    .split(/[/?#]/)[0]!;
}

export function isDomain(input: string): boolean {
  return /^[a-z0-9-]+(\.[a-z0-9-]+)+$/.test(normalizeDomain(input));
}
