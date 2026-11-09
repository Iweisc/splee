-- LinkedIn steps come due in order: the connect note once it's written, each message a few days after the step before it
-- was sent. done_at is when a step was marked sent.
alter table linkedin_messages add column due_at timestamptz, add column done_at timestamptz;

-- Sequences written before this: the first step still to do is due now.
update linkedin_messages m set due_at = now()
where status = 'todo' and not exists (select 1 from linkedin_messages p where p.lead_id = m.lead_id and p.step < m.step and p.status = 'todo');
