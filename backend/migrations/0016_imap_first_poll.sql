-- -1 means the poller hasn't read this mailbox yet. Its first poll starts just before the mail that arrived since the
-- mailbox's first send, instead of downloading a busy inbox from UID 1. Existing mailboxes keep their cursor.
alter table mailboxes alter column imap_last_uid set default -1;
