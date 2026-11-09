alter table project_settings
  add column pipedrive_token_enc text not null default '',
  add column pipedrive_connected boolean generated always as (pipedrive_token_enc <> '') stored,
  add column pipedrive_error text,
  add column pipedrive_failed_at timestamptz;
