-- Facebook Pages and Instagram business accounts, through one Meta sign-in per project (routes/meta.rs). The user's own token
-- is only used during the sign-in, to list the Pages they manage. meta_user_id is who signed in, empty when nobody is, and
-- stands in for the token where hooks::record matches an account. meta_name stays when Meta signs Splee out, so the card can
-- say who to sign in as again, which meta_reconnect reads. meta_held_until holds posting and reading while Meta's usage
-- headers say the app or a Page is near its rate limit.
alter table project_settings
  add column meta_user_id text not null default '',
  add column meta_name text not null default '',
  add column meta_connected boolean generated always as (meta_user_id <> '') stored,
  add column meta_reconnect boolean generated always as (meta_user_id = '' and meta_name <> '') stored,
  add column meta_held_until timestamptz,
  add column facebook_error text,
  add column facebook_failed_at timestamptz,
  add column instagram_error text,
  add column instagram_failed_at timestamptz;

-- A Page is a 'facebook' channel with its Page id as chat_id, and an Instagram account an 'instagram' one with its Instagram
-- user id as chat_id and its Page as guild_id and guild_name. token_enc is the Page's access token, encrypted with APP_SECRET,
-- which an Instagram account posts with too. Empty once Meta signed Splee out.
alter table social_channels add column token_enc text not null default '';
alter table social_channels drop constraint social_channels_platform_check;
alter table social_channels add constraint social_channels_platform_check check (platform in ('discord', 'telegram', 'facebook', 'instagram'));

-- link is a Facebook post's optional link and image_url an Instagram post's picture. container_id is the Instagram media
-- container a post was made into, kept while Instagram still processes it so the next try publishes that one.
alter table social_posts
  add column link text not null default '',
  add column image_url text not null default '',
  add column container_id text not null default '';

alter table social_posts drop constraint social_posts_platform_check;
alter table social_posts add constraint social_posts_platform_check
  check (platform in ('x', 'reddit', 'bluesky', 'mastodon', 'linkedin', 'threads', 'youtube', 'stackexchange', 'dev', 'discourse', 'lemmy',
                      'nostr', 'tumblr', 'hashnode', 'discord', 'telegram', 'facebook', 'instagram'));

alter table social_oauth_states drop constraint social_oauth_states_platform_check;
alter table social_oauth_states add constraint social_oauth_states_platform_check check (platform in ('tumblr', 'meta'));
