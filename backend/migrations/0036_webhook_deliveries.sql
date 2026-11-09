-- The project's most recent webhook deliveries, kept so the integrations page can show what went out and send one again.
-- The URL isn't stored: some carry their own secret, and a resend goes to the URL saved now.
create table webhook_deliveries (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    event text not null,
    payload jsonb not null,
    -- Null when the endpoint answered 2xx.
    error text,
    created_at timestamptz not null default now()
);
create index webhook_deliveries_project_idx on webhook_deliveries (project_id, created_at desc);
