-- POST /v1/campaigns/import: a lead list that becomes a new campaign. The request keeps every submitted lead here and returns,
-- and campaign_imports.rs works through it in the background. Both tables are deleted 30 days after the import.
create table campaign_imports (
  id uuid primary key default gen_random_uuid(),
  org_id uuid not null references organizations(id) on delete cascade,
  project_id uuid not null references projects(id) on delete cascade,
  -- The new campaign, made by the request. Null once someone deletes it.
  segment_id uuid references segments(id) on delete set null,
  status text not null default 'pending' check (status in ('pending', 'completed', 'failed')),
  imported integer not null default 0,
  skipped integer not null default 0,
  deduped integer not null default 0,
  error text not null default '',
  created_at timestamptz not null default now(),
  completed_at timestamptz
);

create index campaign_imports_pending on campaign_imports (created_at) where status = 'pending';
create index campaign_imports_created on campaign_imports (created_at);

create table campaign_import_rows (
  import_id uuid not null references campaign_imports(id) on delete cascade,
  -- The lead's zero-based index in the request's leads.
  row_index integer not null,
  email text not null,
  first_name text not null,
  last_name text not null,
  company_domain text not null,
  job_title text not null,
  linkedin_url text not null,
  client_ref text,
  -- Null until the import runs.
  outcome text check (outcome in ('imported', 'skipped', 'deduped')),
  reason text,
  lead_id uuid references leads(id) on delete set null,
  primary key (import_id, row_index)
);
