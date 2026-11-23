-- The mailboxes a project's first emails go out from. Null for all of the org's. Deleting one takes it out of the list.
alter table project_settings add column mailbox_ids uuid[];
