alter table project_settings
  add column twenty_self_token_enc text not null default '',
  add column twenty_self_connected boolean generated always as (twenty_self_token_enc <> '') stored,
  add column twenty_self_error text,
  add column twenty_self_failed_at timestamptz;
