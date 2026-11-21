-- Find-email-by-name batches from the public API and MCP (POST /v1/projects/:id/enrich/email/batch). email_lookups.rs searches
-- each person with the project's email waterfall without making them a lead, within the organization's hourly limit. A batch
-- is kept for 30 days, since the app shows it nowhere and nobody could delete the names in it otherwise.
create table email_lookup_batches (
  id uuid primary key default gen_random_uuid(),
  org_id uuid not null references organizations(id) on delete cascade,
  project_id uuid not null references projects(id) on delete cascade,
  created_at timestamptz not null default now(),
  -- Set once every person in it is searched.
  finished_at timestamptz
);

create index email_lookup_batches_waiting on email_lookup_batches (org_id, created_at) where finished_at is null;
create index email_lookup_batches_created on email_lookup_batches (created_at);

create table email_lookups (
  batch_id uuid not null references email_lookup_batches(id) on delete cascade,
  position integer not null,
  client_ref text not null default '',
  first_name text not null,
  last_name text not null default '',
  company_domain text not null,
  linkedin_url text not null default '',
  -- One of migration 0103's email_search values but '' and 'queued', or null until searched.
  result text,
  email text,
  -- verified, or found when no verifier confirmed it.
  email_status text,
  source text not null default '',
  note text not null default '',
  phone text,
  searched_at timestamptz,
  primary key (batch_id, position)
);

create index email_lookups_waiting on email_lookups (batch_id, position) where result is null;
