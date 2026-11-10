-- Countries loaded into the company database from the Companies page. One runs at a time across the instance.
-- queued: waiting its turn. running: a worker is reading Overture's files and bumps heartbeat_at as it goes, so a row
-- whose process died is told apart from a slow one. done and failed are final, and a country can be queued again after.
create table directory_imports (
  id uuid primary key default gen_random_uuid(),
  country text not null check (country ~ '^[A-Z]{2}$'),
  status text not null default 'queued' check (status in ('queued', 'running', 'done', 'failed')),
  -- Row groups are Overture's chunks of a few thousand places. Only the ones that can hold the country are read.
  groups_done int not null default 0,
  groups_total int not null default 0,
  rows_seen bigint not null default 0,
  rows_kept bigint not null default 0,
  error text not null default '',
  requested_by uuid references users(id) on delete set null,
  created_at timestamptz not null default now(),
  started_at timestamptz,
  heartbeat_at timestamptz,
  finished_at timestamptz
);

create unique index directory_imports_active on directory_imports (country) where status in ('queued', 'running');
create unique index directory_imports_one_running on directory_imports ((true)) where status = 'running';
create index directory_imports_created on directory_imports (created_at desc);
