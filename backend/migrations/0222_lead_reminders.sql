-- Reminders a member sets themselves on a lead (routes/reminders.rs): come back to it at due_at, with a note. They show on the
-- lead to the whole team and on the member's Today page once due, until done_at.
create table lead_reminders (
    id uuid primary key default gen_random_uuid(),
    lead_id uuid not null references leads(id) on delete cascade,
    user_id uuid not null references users(id) on delete cascade,
    due_at timestamptz not null,
    note text not null default '',
    done_at timestamptz,
    created_at timestamptz not null default now()
);
create index lead_reminders_open on lead_reminders (user_id, due_at) where done_at is null;
create index lead_reminders_lead on lead_reminders (lead_id) where done_at is null;
