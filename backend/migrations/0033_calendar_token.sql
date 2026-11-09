-- The secret in each project's reminders calendar feed URL, which Google Calendar, Outlook or Apple Calendar subscribes to.
alter table project_settings add column calendar_token text not null default encode(gen_random_bytes(16), 'hex');
create unique index project_settings_calendar_token_idx on project_settings (calendar_token);
