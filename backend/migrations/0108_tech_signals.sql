-- The tools a domain's homepage and DNS show it uses, by their names in fingerprints.rs. Shared by every project and
-- organization, since a domain's site is the same for all of them. checked_at is when the tech worker took the domain,
-- which also counts its hourly budget, and scanned_at when that scan finished. A domain is scanned again after 30 days.
create table tech_scans (
  domain citext primary key,
  techs text[] not null default '{}',
  checked_at timestamptz not null default now(),
  scanned_at timestamptz
);

create index tech_scans_checked on tech_scans (checked_at);
create index tech_scans_techs on tech_scans using gin (techs);

-- The tools a project looks for. 'uses' is a good sign on its own, and 'competitor' means the company uses a tool the
-- project's product could replace.
create table project_tech_picks (
  project_id uuid not null references projects(id) on delete cascade,
  tech text not null,
  kind text not null check (kind in ('uses', 'competitor')),
  position int not null default 0,
  primary key (project_id, tech)
);
