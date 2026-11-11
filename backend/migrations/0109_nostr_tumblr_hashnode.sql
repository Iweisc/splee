-- Nostr posts with the project's own secret key, encrypted with APP_SECRET, to the relays in nostr_relays (wss:// addresses).
-- nostr_npub is the public key as an npub, which the daily cap counts by.
-- Tumblr signs in with the operator's Tumblr app (TUMBLR_CLIENT_ID and TUMBLR_CLIENT_SECRET) and keeps the access and refresh
-- tokens encrypted, the refresh token replaced each time it's used. tumblr_username is the account, which the daily cap counts
-- by, tumblr_blogs the blogs it can post to as [{name, title, url, uuid}], and tumblr_blog the one it posts as.
-- Hashnode posts with a personal access token, encrypted, as hashnode_username, which the daily cap counts by, in
-- hashnode_publication_id, one of hashnode_publications as [{id, title, url}].
alter table project_settings
  add column nostr_key_enc text not null default '',
  add column nostr_pubkey text not null default '',
  add column nostr_npub text not null default '',
  add column nostr_relays text[] not null default '{}',
  add column nostr_connected boolean generated always as (nostr_key_enc <> '') stored,
  add column nostr_error text,
  add column nostr_failed_at timestamptz,
  add column tumblr_token_enc text not null default '',
  add column tumblr_refresh_enc text not null default '',
  add column tumblr_expires_at timestamptz,
  add column tumblr_username text not null default '',
  add column tumblr_blogs jsonb not null default '[]',
  add column tumblr_blog text not null default '',
  add column tumblr_connected boolean generated always as (tumblr_refresh_enc <> '') stored,
  add column tumblr_error text,
  add column tumblr_failed_at timestamptz,
  add column hashnode_token_enc text not null default '',
  add column hashnode_username text not null default '',
  add column hashnode_publications jsonb not null default '[]',
  add column hashnode_publication_id text not null default '',
  add column hashnode_connected boolean generated always as (hashnode_token_enc <> '') stored,
  add column hashnode_error text,
  add column hashnode_failed_at timestamptz;

-- One per press of Connect Tumblr, deleted by the callback that uses it. Like mailbox_oauth_states, the state sent to Tumblr
-- is the nonce and an HMAC of it with the user and org, and only the nonce's hash is stored.
create table social_oauth_states (
    nonce_hash text primary key,
    user_id uuid not null references users(id) on delete cascade,
    org_id uuid not null references organizations(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    platform text not null check (platform in ('tumblr')),
    expires_at timestamptz not null,
    created_at timestamptz not null default now()
);

alter table social_posts drop constraint social_posts_platform_check;
alter table social_posts add constraint social_posts_platform_check
  check (platform in ('x', 'reddit', 'bluesky', 'mastodon', 'linkedin', 'threads', 'youtube', 'stackexchange', 'dev', 'discourse', 'lemmy',
                      'nostr', 'tumblr', 'hashnode'));
