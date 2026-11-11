-- Discord and Telegram post as a bot, with its token encrypted with APP_SECRET. {platform}_bot is the bot's username, which
-- the daily cap counts by, and {platform}_held_until holds posting while a 429's retry_after runs. telegram_reads_all is
-- whether the bot's group privacy mode is off, so it sees every message in a group, and telegram_offset the update_id
-- getUpdates starts from next.
alter table project_settings
  add column discord_token_enc text not null default '',
  add column discord_bot_id text not null default '',
  add column discord_bot text not null default '',
  add column discord_connected boolean generated always as (discord_token_enc <> '') stored,
  add column discord_error text,
  add column discord_failed_at timestamptz,
  add column discord_held_until timestamptz,
  add column telegram_token_enc text not null default '',
  add column telegram_bot_id text not null default '',
  add column telegram_bot text not null default '',
  add column telegram_reads_all boolean not null default false,
  add column telegram_offset bigint not null default 0,
  add column telegram_connected boolean generated always as (telegram_token_enc <> '') stored,
  add column telegram_error text,
  add column telegram_failed_at timestamptz,
  add column telegram_held_until timestamptz;

-- The Discord text channels and Telegram groups and channels a project's bot can see. chat_id is Discord's channel id or
-- Telegram's chat id. guild_id and guild_name are the Discord server, empty for Telegram, and username a public Telegram
-- chat's @username, which its links use. kind is 'text' or 'announcement' on Discord and 'group', 'supergroup' or 'channel'
-- on Telegram. Only picked ones are read and posted in. last_message_id is the newest Discord message read, for `after`.
create table social_channels (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    platform text not null check (platform in ('discord', 'telegram')),
    chat_id text not null,
    guild_id text not null default '',
    guild_name text not null default '',
    name text not null,
    kind text not null,
    username text not null default '',
    can_read boolean not null default true,
    can_send boolean not null default true,
    picked boolean not null default false,
    last_message_id text not null default '',
    created_at timestamptz not null default now(),
    unique (project_id, platform, chat_id)
);

-- Recent messages in picked channels, which social runs draft replies to. A message is stored once however often it's read.
create table social_messages (
    channel_id uuid not null references social_channels(id) on delete cascade,
    message_id text not null,
    author text not null,
    body text not null,
    sent_at timestamptz not null,
    created_at timestamptz not null default now(),
    primary key (channel_id, message_id)
);

-- The channel a Discord or Telegram post goes to. None once the bot can't see it anymore.
alter table social_posts add column channel_id uuid references social_channels(id) on delete set null;

alter table social_posts drop constraint social_posts_platform_check;
alter table social_posts add constraint social_posts_platform_check
  check (platform in ('x', 'reddit', 'bluesky', 'mastodon', 'linkedin', 'threads', 'youtube', 'stackexchange', 'dev', 'discourse', 'lemmy',
                      'nostr', 'tumblr', 'hashnode', 'discord', 'telegram'));
