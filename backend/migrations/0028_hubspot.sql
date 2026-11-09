alter table project_settings
  add column hubspot_token_enc text not null default '',
  add column hubspot_connected boolean generated always as (hubspot_token_enc <> '') stored,
  add column hubspot_error text,
  add column hubspot_failed_at timestamptz;
