-- Pages made before reminders send none until someone picks one. New pages remind the guest a day before.
alter table booking_pages add column reminder_hours integer not null default 0;
alter table booking_pages alter column reminder_hours set default 24;

-- When the guest's reminder is due, cleared once it goes. The token is kept encrypted too, because the reminder links to
-- the same move and cancel page as the first email and token_hash can't give it back.
alter table bookings add column remind_at timestamptz;
alter table bookings add column token_enc text not null default '';
create index bookings_remind_at on bookings (remind_at) where remind_at is not null;
