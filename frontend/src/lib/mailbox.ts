import type { BadgeTone } from "@/components/ui/misc";
import type { Mailbox, WarmupHealth } from "./types";

/**
 * Past about 5% bounced, mailbox providers start sending a domain's mail to spam. Under 20 leads a single bounce
 * crosses that, so it waits for enough sends to mean something. Keep in sync with MailboxStats::bouncing.
 */
export function bouncing(emailed: number, bounced: number): boolean {
  return emailed >= 20 && bounced / emailed > 0.05;
}

/** "15%", rounded like MailboxStats::bounce_percent in the digest. */
export function wholePercent(part: number, total: number): string {
  return `${Math.min(100, Math.round((part / Math.max(1, total)) * 100))}%`;
}

const DAY_MS = 86_400_000;

/**
 * Emails `m` may send today. A ramping mailbox starts at 10 on its first day and adds 4 a day until its daily limit.
 * Keep in sync with TODAY_LIMIT in backend/src/mail/sender.rs, which counts days in UTC like this.
 */
export function todayLimit(m: Mailbox): number {
  if (!m.ramp_up) return m.daily_limit;
  const firstDay = m.first_sent_on ? Date.parse(`${m.first_sent_on}T00:00:00Z`) : Date.now();
  const days = Math.max(0, Math.floor((Date.now() - firstDay) / DAY_MS));
  return Math.min(m.daily_limit, 10 + 4 * days);
}

/** Campaigns from a mailbox held until warm start after this many days of warmup. Keep in sync with warmup::HOLD_DAYS. */
export const HOLD_DAYS = 14;

/**
 * The receiving mailboxes found more than 1 in 5 of its warmup emails in spam, out of at least 10 they saw. Keep in sync
 * with LANDING_IN_SPAM in backend/src/mail/warmup.rs.
 */
export function landingInSpam(h: WarmupHealth): boolean {
  const seen = h.inbox + h.spam;
  return seen >= 10 && h.spam * 5 > seen;
}

export type WarmupStatus = { tone: BadgeTone; label: string };

/** Where a warming mailbox stands, worst first. */
export function warmupStatus(m: Mailbox, h?: WarmupHealth): WarmupStatus {
  if (!m.imap_host) return { tone: "amber", label: "Needs IMAP" };
  if (m.status === "paused") return { tone: "neutral", label: "Waits while paused" };
  if (m.status === "error") return { tone: "red", label: "Stopped" };
  if (!h) return { tone: "neutral", label: "Starting" };
  if (landingInSpam(h)) return { tone: "amber", label: "Landing in spam" };
  if (!h.has_peers && !h.day) return { tone: "neutral", label: "Waiting for a second mailbox" };
  if (!h.day) return { tone: "neutral", label: "Starting today" };
  if (h.day <= HOLD_DAYS) return { tone: "blue", label: `Warming up, day ${h.day} of ~${HOLD_DAYS}` };
  return { tone: "green", label: "Ready" };
}

/** Whether campaigns from it wait for warmup right now. Keep in sync with mailboxes_with_room in sender.rs. */
export function heldForWarmup(m: Mailbox, h?: WarmupHealth): boolean {
  if (!m.warmup || !m.hold_until_warm) return false;
  return !h?.day || h.day <= HOLD_DAYS || landingInSpam(h);
}
