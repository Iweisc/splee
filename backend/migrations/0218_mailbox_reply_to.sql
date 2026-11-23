-- Emails from this mailbox carry Reply-To with another of the org's mailboxes, so replies land in an inbox Splee also reads,
-- like a rep's main address for a mailbox on a secondary domain (mail/sender.rs deliver). Null for its own address.
alter table mailboxes add column reply_to_mailbox_id uuid references mailboxes(id) on delete set null;
