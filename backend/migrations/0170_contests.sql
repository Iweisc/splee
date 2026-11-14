-- Sales contests (contests.rs): an owner or admin starts one on a metric with a prize until a date, everyone sees the
-- leaderboard on the Targets tab and their place on Today, and the winner is posted to the team's chats when it ends.
create table contests (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    name text not null,
    metric text not null check (metric in ('won_value', 'deals_won', 'pipeline', 'calls', 'practice')),
    prize text not null default '',
    starts_at timestamptz not null default now(),
    ends_at timestamptz not null,
    created_by uuid references users(id) on delete set null,
    -- Set when the winner was posted, so each contest is announced once.
    announced_at timestamptz,
    created_at timestamptz not null default now()
);
create index contests_org_idx on contests (org_id, ends_at desc);
create index contests_due_idx on contests (ends_at) where announced_at is null;
