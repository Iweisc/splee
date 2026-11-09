alter table project_settings
  add column attio_token_enc text not null default '',
  add column attio_connected boolean generated always as (attio_token_enc <> '') stored,
  add column attio_error text,
  add column attio_failed_at timestamptz;
