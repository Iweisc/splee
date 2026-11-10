alter table project_settings
  add column grist_token_enc text not null default '',
  add column grist_connected boolean generated always as (grist_token_enc <> '') stored,
  add column grist_error text,
  add column grist_failed_at timestamptz;
