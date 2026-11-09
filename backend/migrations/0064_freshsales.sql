alter table project_settings
  add column freshsales_token_enc text not null default '',
  add column freshsales_connected boolean generated always as (freshsales_token_enc <> '') stored,
  add column freshsales_error text,
  add column freshsales_failed_at timestamptz;
