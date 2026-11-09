-- The inbox starts from leads that replied, and finds each one's latest reply, without scanning every email.
-- Replies are a small share of email_messages, so the index stays small.
create index email_messages_inbound_idx on email_messages (lead_id, created_at) where direction = 'inbound';
