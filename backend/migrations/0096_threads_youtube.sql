-- Threads posts with a long-lived access token, as the user id and username it belongs to, read on connect. YouTube trades a
-- refresh token for access tokens with the user's own Google OAuth client, and comments as the account's channel: its id,
-- which the daily cap counts by, and youtube_channel, the @handle or title shown. The secrets are encrypted with APP_SECRET.
alter table project_settings
  add column threads_token_enc text not null default '',
  add column threads_user_id text not null default '',
  add column threads_username text not null default '',
  add column threads_connected boolean generated always as (threads_token_enc <> '') stored,
  add column threads_error text,
  add column threads_failed_at timestamptz,
  add column youtube_client_id text not null default '',
  add column youtube_secret_enc text not null default '',
  add column youtube_token_enc text not null default '',
  add column youtube_channel_id text not null default '',
  add column youtube_channel text not null default '',
  add column youtube_connected boolean generated always as (youtube_token_enc <> '') stored,
  add column youtube_error text,
  add column youtube_failed_at timestamptz;

alter table social_posts drop constraint social_posts_platform_check;
alter table social_posts add constraint social_posts_platform_check
  check (platform in ('x', 'reddit', 'bluesky', 'mastodon', 'linkedin', 'threads', 'youtube'));
