alter table project_settings
  add column baserow_token_enc text not null default '',
  add column baserow_connected boolean generated always as (baserow_token_enc <> '') stored,
  add column baserow_error text,
  add column baserow_failed_at timestamptz;
