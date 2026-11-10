-- The public job board a company's domain uses, shared by every project since it's the same for all of them. provider is
-- '' when none was found, which is tried again after a while. slug is the company's name on the board, like "gitlab".
create table job_boards (
  domain citext primary key,
  provider text not null default '' check (provider in ('', 'greenhouse', 'lever', 'ashby', 'workable', 'recruitee')),
  slug text not null default '',
  detected_at timestamptz not null default now()
);

-- When the hiring worker last looked at each of a project's company domains. It checks the oldest first, at most 500 a
-- day per project, so this also counts the day's checks.
create table hiring_checks (
  project_id uuid not null references projects(id) on delete cascade,
  domain citext not null,
  checked_at timestamptz not null default now(),
  primary key (project_id, domain)
);

create index hiring_checks_recent on hiring_checks (project_id, checked_at);

-- Open roles on a company's job board that match the project's hiring roles. last_seen moves on every check that still
-- finds the post, and closed_at is set once the board stops listing it. A post that comes back is open again.
create table job_openings (
  id uuid primary key default gen_random_uuid(),
  project_id uuid not null references projects(id) on delete cascade,
  domain citext not null,
  provider text not null,
  title text not null,
  -- The watched role the title matched, like "Account Executive" for "Senior Account Executive, EMEA". Badges show these.
  role text not null default '',
  location text not null default '',
  department text not null default '',
  url text not null,
  -- The start of the post as plain text, for the email writer.
  excerpt text not null default '',
  posted_at timestamptz,
  first_seen timestamptz not null default now(),
  last_seen timestamptz not null default now(),
  closed_at timestamptz,
  unique (project_id, url)
);

create index job_openings_open on job_openings (project_id, domain) where closed_at is null;
create index job_openings_first_seen on job_openings (project_id, first_seen desc);
