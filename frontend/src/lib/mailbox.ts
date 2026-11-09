import type { Mailbox } from "./types";

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
