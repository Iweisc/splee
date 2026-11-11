-- A built-in deals pipeline (deals.rs, routes/deals.rs), for teams that take hot leads to won or lost without a CRM.
-- Each organization gets the default stages the first time it needs them, which pipeline_seeded records, so deleting or
-- renaming them never brings the defaults back. currency is what new deals are priced in.
alter table organizations
    add column currency text not null default 'USD',
    add column pipeline_seeded boolean not null default false;

-- kind says whether a deal in the stage is still open, won or lost. probability (0-100) weights an open stage's deals in the
-- forecast. auto marks where the automatic moves go: 'hot' is the stage a hot reply opens a deal in, 'booked' the one a
-- booked meeting moves it to. It sticks to the stage through renames and reorders, and only open stages have one.
create table deal_stages (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    name text not null,
    kind text not null default 'open' check (kind in ('open', 'won', 'lost')),
    position integer not null,
    probability integer not null default 50 check (probability between 0 and 100),
    auto text check (auto in ('hot', 'booked')),
    created_at timestamptz not null default now()
);
create index deal_stages_org_idx on deal_stages (org_id, position);
create unique index deal_stages_auto_idx on deal_stages (org_id, auto) where auto is not null;

-- status copies the stage's kind, so one lead has at most one open deal (the partial index below), and closed deals
-- stay with the lead. value is in minor units (cents) of currency. stage_changed_at is when it entered its stage.
-- stage_id has no cascade: a stage is only deleted after its deals move elsewhere.
create table deals (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    lead_id uuid not null references leads(id) on delete cascade,
    stage_id uuid not null references deal_stages(id),
    status text not null default 'open' check (status in ('open', 'won', 'lost')),
    value_cents bigint not null default 0 check (value_cents >= 0),
    currency text not null default 'USD',
    close_date date,
    owner_id uuid references users(id) on delete set null,
    notes text not null default '',
    lost_reason text not null default '',
    source text not null default 'manual' check (source in ('manual', 'hot_reply', 'booking', 'api')),
    stage_changed_at timestamptz not null default now(),
    closed_at timestamptz,
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now()
);
create unique index deals_one_open_per_lead on deals (lead_id) where status = 'open';
create index deals_lead_idx on deals (lead_id);
create index deals_org_idx on deals (org_id, status, closed_at);
create index deals_stage_idx on deals (stage_id);
