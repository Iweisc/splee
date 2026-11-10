-- Warmup: mailboxes with it on email each other short business-style messages, reply to some, and rescue them from spam,
-- so providers see a new mailbox getting engagement before it sends campaigns. Everything defaults off, so existing
-- mailboxes keep sending as they did. New ones get warmup and the hold from the create route.
alter table mailboxes
  add column warmup boolean not null default false,
  -- The UTC day of its first warmup email. The ramp and the 14-day hold count from it. Turning warmup off clears it.
  add column warmup_started_on date,
  add column warmup_next_at timestamptz,
  add column warmup_daily_cap int not null default 40,
  -- Campaign and warmup emails together, counted only while warmup is on.
  add column total_daily_limit int not null default 80,
  add column hold_until_warm boolean not null default false,
  -- The newest UID the warmup pass has read in the spam folder, so each poll only reads what arrived since.
  add column warmup_junk_uid bigint not null default 0;

-- One row per warmup email. token is in the X-Splee-Warmup header and the body, so the receiving mailbox's poller knows
-- the email whether or not the header survived. landed is where that mailbox found it.
create table warmup_emails (
  id uuid primary key default gen_random_uuid(),
  token text not null unique,
  from_mailbox_id uuid not null references mailboxes(id) on delete cascade,
  to_mailbox_id uuid not null references mailboxes(id) on delete cascade,
  -- Set on a reply: the warmup email it answers, in the same thread.
  parent_id uuid references warmup_emails(id) on delete cascade,
  subject text not null,
  message_id text not null,
  -- queued: a reply waiting for send_after. sending: claimed by the warmup worker.
  status text not null default 'queued' check (status in ('queued', 'sending', 'sent', 'failed')),
  send_after timestamptz not null default now(),
  sent_at timestamptz,
  landed text check (landed in ('inbox', 'spam')),
  landed_at timestamptz,
  error text not null default '',
  created_at timestamptz not null default now()
);
create index warmup_emails_from on warmup_emails (from_mailbox_id, sent_at);
create index warmup_emails_to on warmup_emails (to_mailbox_id, landed_at);
create index warmup_emails_queued on warmup_emails (send_after) where status = 'queued';
create unique index warmup_emails_one_reply on warmup_emails (parent_id) where parent_id is not null;
