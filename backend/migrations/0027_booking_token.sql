-- The secret in each project's booking webhook URL, which Calendly or Cal.com calls when a lead books a meeting.
alter table project_settings add column booking_token text not null default encode(gen_random_bytes(16), 'hex');
create unique index project_settings_booking_token_idx on project_settings (booking_token);
