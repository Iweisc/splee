alter table project_settings
  add column folk_token_enc text not null default '',
  add column folk_connected boolean generated always as (folk_token_enc <> '') stored,
  add column folk_error text,
  add column folk_failed_at timestamptz;
