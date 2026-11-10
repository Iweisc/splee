alter table project_settings
  add column espocrm_token_enc text not null default '',
  add column espocrm_connected boolean generated always as (espocrm_token_enc <> '') stored,
  add column espocrm_error text,
  add column espocrm_failed_at timestamptz;
