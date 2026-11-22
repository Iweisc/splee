-- First emails and follow-ups keep to the project's send days and hours in each lead's own time zone, when their phone number
-- or country has a single one (phone::time_zone), instead of the project's (mail/sender.rs send_project). Off until a project
-- turns it on.
alter table project_settings add column lead_time_zone boolean not null default false;
