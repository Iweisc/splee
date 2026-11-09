-- A new mailbox can start with a few emails a day and work up to its daily limit, counted from its first send.
-- Mailboxes connected before this keep sending at their full limit.
alter table mailboxes add column ramp_up boolean not null default false;
alter table mailboxes add column first_sent_on date;
update mailboxes m set first_sent_on = (select min(e.sent_at)::date from email_messages e where e.mailbox_id = m.id and e.status = 'sent');
