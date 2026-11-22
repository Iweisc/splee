-- First emails to a lead whose mail goes to Google or Microsoft go out from one of the organization's mailboxes on the same
-- provider when one has room (mail/sender.rs pick_from). Off until a project turns it on.
alter table project_settings add column match_provider boolean not null default false;
