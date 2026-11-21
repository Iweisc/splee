-- Company database searches saved on a project's Companies page, and the domains each has matched, so the hourly check can
-- tell which companies are new. directory_companies has no reliable "added at": a new Overture release bumps every row's
-- updated_at. So each search keeps the domains it has matched, at most saved_searches::SEEN_CAP, and a broader search
-- isn't watched (too_broad).
create table saved_searches (
  id uuid primary key default gen_random_uuid(),
  project_id uuid not null references projects(id) on delete cascade,
  -- Who hears about new matches. Null for one saved with an API key, which only its chat alerts announce.
  user_id uuid references users(id) on delete set null,
  name text not null check (length(name) between 1 and 120),
  -- The filters, as GET /directory/companies takes them. Empty means not filtered.
  q text not null default '',
  country text not null default '',
  city text not null default '',
  category text not null default '',
  has_email boolean not null default false,
  has_phone boolean not null default false,
  tech text not null default '',
  chat_alerts boolean not null default false,
  -- New matches go into this campaign, a capped number a check.
  add_to_segment_id uuid references segments(id) on delete set null,
  -- When the matches it had on saving were recorded. Null until then.
  baseline_at timestamptz,
  checked_at timestamptz,
  too_broad boolean not null default false,
  -- When it was last opened in the app. The list counts the new matches found since.
  opened_at timestamptz,
  created_at timestamptz not null default now()
);

create index saved_searches_project on saved_searches (project_id, created_at);
create index saved_searches_due on saved_searches (checked_at nulls first) where not too_broad;

create table saved_search_matches (
  search_id uuid not null references saved_searches(id) on delete cascade,
  domain text not null,
  -- Null for the companies that already matched when the search was saved.
  found_at timestamptz,
  primary key (search_id, domain)
);

create index saved_search_matches_found on saved_search_matches (search_id, found_at) where found_at is not null;
