alter table project_settings
  add column teable_token_enc text not null default '',
  add column teable_connected boolean generated always as (teable_token_enc <> '') stored,
  add column teable_error text,
  add column teable_failed_at timestamptz;
