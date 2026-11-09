-- When the call a lead booked through Calendly or Cal.com starts, for the lead's page, the chat alert and the webhook.
alter table leads add column meeting_at timestamptz;
