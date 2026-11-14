-- Practice assignments (agent/practice.rs): the practice call a 1:1 prep suggests, assigned to the teammate it was written
-- for. They see it on Today, and the practice call they start from it is scored on the skill and shows on the next prep.

-- skill and drill are the prep's, kept as they were when it was assigned. One assignment per prep.
create table practice_assignments (
    id uuid primary key default gen_random_uuid(),
    one_on_one_id uuid not null unique references one_on_ones(id) on delete cascade,
    deal_id uuid not null references deals(id) on delete cascade,
    user_id uuid not null references users(id) on delete cascade,
    assigned_by uuid references users(id) on delete set null,
    scenario text not null check (scenario in ('next_call', 'pricing', 'competitor')),
    skill text not null,
    drill text not null,
    created_at timestamptz not null default now()
);
create index practice_assignments_user_idx on practice_assignments (user_id, created_at desc);

alter table practice_sessions add column assignment_id uuid references practice_assignments(id) on delete set null;
create index practice_sessions_assignment_idx on practice_sessions (assignment_id) where assignment_id is not null;
