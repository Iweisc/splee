alter table project_settings
  add column close_token_enc text not null default '',
  add column close_connected boolean generated always as (close_token_enc <> '') stored,
  add column close_error text,
  add column close_failed_at timestamptz;
