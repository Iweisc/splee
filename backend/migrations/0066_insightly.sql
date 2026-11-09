alter table project_settings
  add column insightly_token_enc text not null default '',
  add column insightly_connected boolean generated always as (insightly_token_enc <> '') stored,
  add column insightly_error text,
  add column insightly_failed_at timestamptz;
