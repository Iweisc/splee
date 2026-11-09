alter table project_settings
  add column brevo_token_enc text not null default '',
  add column brevo_connected boolean generated always as (brevo_token_enc <> '') stored,
  add column brevo_error text,
  add column brevo_failed_at timestamptz;
