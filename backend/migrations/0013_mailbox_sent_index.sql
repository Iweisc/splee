-- The sender counts each mailbox's sends today on every tick to keep it under its daily limit.
-- Without this it reads every email ever sent, about 180 ms per tick at 300k emails.
create index email_messages_mailbox_sent_idx on email_messages (mailbox_id, sent_at) where status = 'sent';
