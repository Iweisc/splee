-- When a thread comes back to the inbox's Needs reply, for following up with a lead who went quiet.
alter table leads add column remind_at timestamptz;
