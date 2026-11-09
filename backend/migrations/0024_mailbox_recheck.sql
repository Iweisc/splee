-- When the sender next tests a mailbox it stopped because its server couldn't be reached. Null when it waits for a person.
alter table mailboxes add column recheck_at timestamptz;
