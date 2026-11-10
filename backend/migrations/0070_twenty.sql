alter table project_settings
  add column twenty_token_enc text not null default '',
  add column twenty_connected boolean generated always as (twenty_token_enc <> '') stored,
  add column twenty_error text,
  add column twenty_failed_at timestamptz;
