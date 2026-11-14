-- Plays (plays.rs): "when this happens, do that" rules a project runs on its own, like "a company raises money: add it to
-- a campaign and find its people". The worker checks each play's trigger for events after checked_at.

-- trigger is what it watches: funding, news, hiring and visit happen at a company, job_change and score to a lead.
-- action is add_to_campaign (companies, into segment_id, finding their people), write_email (leads without a first email)
-- or notify (the project's chats). min_score only lets through companies or leads scoring at least that, and the score
-- trigger fires when a lead reaches it. daily_cap is how many times it fires a day.
create table plays (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    trigger text not null check (trigger in ('funding', 'news', 'hiring', 'visit', 'job_change', 'score')),
    action text not null check (action in ('add_to_campaign', 'write_email', 'notify')),
    segment_id uuid references segments(id) on delete set null,
    min_score int not null default 0 check (min_score between 0 and 100),
    daily_cap int not null default 20 check (daily_cap between 1 and 200),
    enabled boolean not null default true,
    checked_at timestamptz not null default now(),
    created_at timestamptz not null default now()
);
create index plays_project_idx on plays (project_id, created_at);

-- Each time a play fired, once per company or lead (subject is "domain:acme.com" or "lead:{id}"). outcome is done,
-- skipped (with why in note) or failed.
create table play_firings (
    id uuid primary key default gen_random_uuid(),
    play_id uuid not null references plays(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    subject text not null,
    domain citext,
    lead_id uuid references leads(id) on delete set null,
    what text not null,
    outcome text not null check (outcome in ('done', 'skipped', 'failed')),
    note text not null default '',
    run_id uuid references agent_runs(id) on delete set null,
    created_at timestamptz not null default now(),
    unique (play_id, subject)
);
create index play_firings_project_idx on play_firings (project_id, created_at desc);
