alter table project_settings
  add column nocodb_token_enc text not null default '',
  add column nocodb_connected boolean generated always as (nocodb_token_enc <> '') stored,
  add column nocodb_error text,
  add column nocodb_failed_at timestamptz;
