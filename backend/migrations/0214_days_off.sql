-- Dates no first emails, follow-ups, sequence texts or LinkedIn steps go out on, like holidays, each in the time zone its
-- send window is counted in (mail::in_window). Replies still go out.
alter table project_settings add column days_off date[] not null default '{}';
