alter table project_settings
  add column salesflare_token_enc text not null default '',
  add column salesflare_connected boolean generated always as (salesflare_token_enc <> '') stored,
  add column salesflare_error text,
  add column salesflare_failed_at timestamptz;
