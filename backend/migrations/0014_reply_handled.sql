-- Set on a lead's replies when you mark the thread done without answering, so it leaves the inbox's Needs reply filter.
-- A new reply has none, which brings the thread back.
alter table email_messages add column handled_at timestamptz;
