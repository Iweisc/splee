-- When a lead last changed status, which the Analytics page (routes/analytics.rs) reads to put meetings booked and
-- unsubscribes in a date range. A trigger keeps it, so bookings, the inbox, the API, CRM syncs and every later path that
-- sets a status stay right without each one remembering to. An insert keeps a time it was given, for imports and tests.
alter table leads add column status_changed_at timestamptz;

create function leads_status_changed_at() returns trigger language plpgsql as $$
begin
    if tg_op = 'INSERT' then
        new.status_changed_at := coalesce(new.status_changed_at, now());
    elsif new.status is distinct from old.status then
        new.status_changed_at := now();
    end if;
    return new;
end
$$;

create trigger leads_status_changed_at before insert or update of status on leads
    for each row execute function leads_status_changed_at();

-- Older leads never recorded the moment, so booked and unsubscribed ones get their last reply, or else their last sent
-- email, which is when it most likely happened. Other statuses aren't read by time and stay null.
update leads l set status_changed_at = coalesce(
    (select max(e.created_at) from email_messages e where e.lead_id = l.id and e.direction = 'inbound'),
    (select max(e.sent_at) from email_messages e where e.lead_id = l.id and e.direction = 'outbound' and e.status = 'sent'), l.created_at)
where l.status in ('meeting_booked', 'unsubscribed');

-- The range's sent emails, and the LinkedIn steps sent, which had no index by project.
create index email_messages_project_sent_idx on email_messages (project_id, sent_at) where status = 'sent';
create index linkedin_messages_project_done_idx on linkedin_messages (project_id, done_at) where status = 'done';
