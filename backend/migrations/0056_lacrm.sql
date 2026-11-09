alter table project_settings
  add column lacrm_token_enc text not null default '',
  add column lacrm_connected boolean generated always as (lacrm_token_enc <> '') stored,
  add column lacrm_error text,
  add column lacrm_failed_at timestamptz;
