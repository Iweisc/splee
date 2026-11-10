-- A project can join several Discourse forums. Each has its address (a forum can live under a path, like
-- https://example.com/forum), the user it posts as, and that user's key, encrypted with APP_SECRET: a User API Key, or an API
-- key an admin made, which goes with the username (key_kind 'user' or 'admin'). account is the address and /u/{username},
-- lowercase, which the daily cap counts by. category_id is where new topics go, null for replies only. held_until holds
-- posting while the forum's Retry-After from a 429 runs.
create table discourse_forums (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    url text not null,
    username text not null,
    key_kind text not null check (key_kind in ('user', 'admin')),
    key_enc text not null,
    account text not null,
    category_id bigint,
    category_name text not null default '',
    -- The last failure, cleared when a post goes through.
    error text,
    failed_at timestamptz,
    held_until timestamptz,
    created_at timestamptz not null default now(),
    unique (project_id, account)
);

-- The forum a Discourse reply or topic goes to. Removing the forum leaves its posts with none, and they can't be approved.
alter table social_posts add column forum_id uuid references discourse_forums(id) on delete set null;

-- Lemmy signs in with the instance's address, the username and the password, encrypted with APP_SECRET, and keeps the JWT it
-- gets back, encrypted too, until the instance stops taking it. lemmy_handle is name@instance, which the daily cap counts by.
-- For Lemmy, social_posts.subreddit holds the community as name@instance.
alter table project_settings
  add column lemmy_url text not null default '',
  add column lemmy_username text not null default '',
  add column lemmy_password_enc text not null default '',
  add column lemmy_jwt_enc text not null default '',
  add column lemmy_handle text not null default '',
  add column lemmy_connected boolean generated always as (lemmy_password_enc <> '') stored,
  add column lemmy_error text,
  add column lemmy_failed_at timestamptz;

alter table social_posts drop constraint social_posts_platform_check;
alter table social_posts add constraint social_posts_platform_check
  check (platform in ('x', 'reddit', 'bluesky', 'mastodon', 'linkedin', 'threads', 'youtube', 'stackexchange', 'dev', 'discourse', 'lemmy'));
