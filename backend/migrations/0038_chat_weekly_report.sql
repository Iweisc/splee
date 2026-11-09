-- Whether the project's chat channel gets a Monday summary of the last 7 days, and the day it last went out, so a second
-- check in the digest hour never posts it twice.
alter table project_settings add column chat_weekly_report boolean not null default false;
alter table project_settings add column chat_report_on date;
