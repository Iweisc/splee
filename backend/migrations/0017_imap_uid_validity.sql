-- The INBOX's UIDVALIDITY when imap_last_uid was taken. A server that renumbers its messages changes it, and then the
-- cursor starts over like a new mailbox's instead of pointing past every new message.
alter table mailboxes add column imap_uid_validity bigint;
