alter table project_settings
  add column verifier text not null default '' check (verifier in ('', 'neverbounce', 'millionverifier')),
  add column verifier_key_enc text not null default '',
  add column verifier_error text,
  add column verifier_failed_at timestamptz;
