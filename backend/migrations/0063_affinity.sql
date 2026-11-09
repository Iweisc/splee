alter table project_settings
  add column affinity_token_enc text not null default '',
  add column affinity_connected boolean generated always as (affinity_token_enc <> '') stored,
  add column affinity_error text,
  add column affinity_failed_at timestamptz;
