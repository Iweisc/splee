import type { Mailbox } from "./types";

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
