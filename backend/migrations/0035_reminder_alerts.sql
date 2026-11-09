-- When the chat alert for the lead's reminder went out. Older than remind_at means the reminder was moved since.
alter table leads add column remind_alerted_at timestamptz;
