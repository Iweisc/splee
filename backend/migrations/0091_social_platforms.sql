-- Bluesky signs in with the handle and an app password, Mastodon with the instance's URL and an access token. LinkedIn posts
-- go out through the project's Unipile connection, so it needs no columns. The secrets are encrypted with APP_SECRET.
-- mastodon_handle is user@instance, mastodon_max_chars the instance's post limit, read on connect.
alter table project_settings
  add column bluesky_handle text not null default '',
  add column bluesky_password_enc text not null default '',
  add column bluesky_connected boolean generated always as (bluesky_password_enc <> '') stored,
  add column bluesky_error text,
  add column bluesky_failed_at timestamptz,
  add column mastodon_url text not null default '',
  add column mastodon_token_enc text not null default '',
  add column mastodon_handle text not null default '',
  add column mastodon_max_chars integer not null default 500,
  add column mastodon_connected boolean generated always as (mastodon_token_enc <> '') stored,
  add column mastodon_error text,
  add column mastodon_failed_at timestamptz;

-- For LinkedIn, social_posts.account is the Unipile account id, which the daily cap counts by.
alter table social_posts drop constraint social_posts_platform_check;
alter table social_posts add constraint social_posts_platform_check check (platform in ('x', 'reddit', 'bluesky', 'mastodon', 'linkedin'));
