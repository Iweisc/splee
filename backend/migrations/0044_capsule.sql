alter table project_settings
  add column capsule_token_enc text not null default '',
  add column capsule_connected boolean generated always as (capsule_token_enc <> '') stored,
  add column capsule_error text,
  add column capsule_failed_at timestamptz;
