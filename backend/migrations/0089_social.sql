-- The X and Reddit accounts a project posts from. X signs in with the auth_token and ct0 cookies of a logged-in browser,
-- Reddit with a script app and the account's password. The secrets are encrypted with APP_SECRET.
alter table project_settings
  add column x_auth_token_enc text not null default '',
  add column x_ct0_enc text not null default '',
  add column x_handle text not null default '',
  add column x_connected boolean generated always as (x_auth_token_enc <> '') stored,
  add column x_error text,
  add column x_failed_at timestamptz,
  add column reddit_client_id text not null default '',
  add column reddit_secret_enc text not null default '',
  add column reddit_username text not null default '',
  add column reddit_password_enc text not null default '',
  add column reddit_connected boolean generated always as (reddit_password_enc <> '') stored,
  add column reddit_error text,
  add column reddit_failed_at timestamptz;

-- Replies and posts the agent drafted or the user wrote. in_reply_to is the thread's canonical URL, null for an original
-- post. account is the handle or username it went out from, which the daily cap counts by.
create table social_posts (
  id uuid primary key default gen_random_uuid(),
  project_id uuid not null references projects(id) on delete cascade,
  run_id uuid references agent_runs(id) on delete set null,
  platform text not null check (platform in ('x', 'reddit')),
  in_reply_to text,
  context text not null default '',
  subreddit text not null default '',
  title text not null default '',
  body text not null,
  reason text not null default '',
  -- posting: the poster claimed it and is sending it. A post never leaves posting on its own except to posted or failed,
  -- so one that may have gone out is never sent twice.
  status text not null default 'draft' check (status in ('draft', 'approved', 'posting', 'posted', 'failed', 'skipped')),
  account text not null default '',
  url text not null default '',
  error text,
  posted_at timestamptz,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);
create unique index social_posts_one_reply_idx on social_posts (project_id, in_reply_to);
create index social_posts_project_idx on social_posts (project_id, created_at desc);
create index social_posts_posted_idx on social_posts (platform, account, posted_at) where status = 'posted';

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social'));
