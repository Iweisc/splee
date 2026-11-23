-- The mailboxes a campaign's first emails go out from, in place of the project's (project_settings.mailbox_ids). Null follows
-- the project's. Deleting a mailbox takes it out of the list.
alter table segments add column mailbox_ids uuid[];
