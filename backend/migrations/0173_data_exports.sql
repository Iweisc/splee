-- The daily data export to the organization's own S3-compatible bucket (exports/). Numbered 0139 because 0137 and 0138 are
-- taken by other branches.

-- An empty endpoint means AWS S3 in the region. path_style puts the bucket in the path (https://host/bucket/key), which MinIO
-- needs, instead of the host (https://bucket.host/key). secret_key_enc is encrypted with APP_SECRET and never leaves the
-- server. tables are the names in exports::tables::TABLES. include_bodies adds the text of emails and replies.
create table export_settings (
    org_id uuid primary key references organizations(id) on delete cascade,
    endpoint text not null default '',
    region text not null,
    bucket text not null,
    prefix text not null default '',
    path_style boolean not null default false,
    access_key_id text not null,
    secret_key_enc text not null,
    tables text[] not null,
    include_bodies boolean not null default false,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

-- One row per export, the daily one and each Export now. dt is the UTC day the files are under. files is
-- [{"table", "key", "rows", "bytes"}], as the manifest lists them.
create table export_runs (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    dt date not null,
    trigger text not null check (trigger in ('daily', 'manual')),
    status text not null default 'running' check (status in ('running', 'done', 'failed')),
    files jsonb not null default '[]',
    rows bigint not null default 0,
    bytes bigint not null default 0,
    error text,
    started_at timestamptz not null default now(),
    finished_at timestamptz
);
create index export_runs_org_idx on export_runs (org_id, started_at desc);
-- One export at a time per organization.
create unique index export_runs_running_idx on export_runs (org_id) where status = 'running';
