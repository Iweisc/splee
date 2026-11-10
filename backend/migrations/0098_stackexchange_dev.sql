-- Stack Exchange answers on one site with the user's own app key and an access token with write_access. The key isn't a
-- secret by Stack Exchange's rules, so only the token is encrypted with APP_SECRET. stackexchange_site is the site's host,
-- like stackoverflow.com, and stackexchange_account is {site}/users/{user_id}, which the daily cap counts by.
-- stackexchange_backoff_until holds posting while the API's backoff from the last answer runs.
-- DEV publishes articles with an API key from dev.to's settings, encrypted with APP_SECRET.
alter table project_settings
  add column stackexchange_key text not null default '',
  add column stackexchange_token_enc text not null default '',
  add column stackexchange_site text not null default '',
  add column stackexchange_account text not null default '',
  add column stackexchange_user text not null default '',
  add column stackexchange_connected boolean generated always as (stackexchange_token_enc <> '') stored,
  add column stackexchange_error text,
  add column stackexchange_failed_at timestamptz,
  add column stackexchange_backoff_until timestamptz,
  add column dev_key_enc text not null default '',
  add column dev_username text not null default '',
  add column dev_connected boolean generated always as (dev_key_enc <> '') stored,
  add column dev_error text,
  add column dev_failed_at timestamptz;

-- A DEV article's tags, at most 4.
alter table social_posts add column tags text[] not null default '{}';

alter table social_posts drop constraint social_posts_platform_check;
alter table social_posts add constraint social_posts_platform_check
  check (platform in ('x', 'reddit', 'bluesky', 'mastodon', 'linkedin', 'threads', 'youtube', 'stackexchange', 'dev'));
