-- The email waterfall: finding an address for a lead that came without one, like LinkedIn people search's leads.
-- '' is never tried, which the worker picks up on its own. 'queued' is a lead someone asked to search, or search again.
alter table leads add column email_search text not null default ''
  check (email_search in ('', 'queued', 'found', 'guessed', 'catch_all', 'not_found', 'suppressed', 'duplicate', 'failed'));
-- The finder that found the address, like 'Hunter', or the pattern a guess followed, like 'first.last'.
alter table leads add column email_source text not null default '';
-- Why the search ended the way it did, as the Leads page shows it.
alter table leads add column email_search_note text not null default '';
-- Calls to paid finders and verifiers, and the credits they said they charged, null when none said.
alter table leads add column email_search_lookups integer not null default 0;
alter table leads add column email_search_credits integer;
alter table leads add column email_searched_at timestamptz;

-- The worker's two questions: what's waiting, and how many an organization's projects searched in the last hour.
create index leads_email_search_idx on leads (project_id, created_at) where email is null and email_search in ('', 'queued');
create index leads_email_searched_idx on leads (project_id, email_searched_at) where email_searched_at is not null;

-- One row per search the worker starts, which the hourly limit counts. A lead searched again counts again, and a deleted lead
-- still counts, which the lead rows alone couldn't do.
create table email_search_attempts (
  org_id uuid not null references organizations(id) on delete cascade,
  at timestamptz not null default now()
);
create index email_search_attempts_idx on email_search_attempts (org_id, at);
