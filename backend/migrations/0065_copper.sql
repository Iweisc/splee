alter table project_settings
  add column copper_token_enc text not null default '',
  add column copper_connected boolean generated always as (copper_token_enc <> '') stored,
  add column copper_error text,
  add column copper_failed_at timestamptz;
