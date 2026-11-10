alter table project_settings
  add column airtable_token_enc text not null default '',
  add column airtable_connected boolean generated always as (airtable_token_enc <> '') stored,
  add column airtable_error text,
  add column airtable_failed_at timestamptz;
