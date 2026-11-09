alter table project_settings
  add column finder text not null default '' constraint project_settings_finder_check check (finder in ('', 'hunter', 'findymail', 'prospeo')),
  add column finder_key_enc text not null default '',
  add column finder_error text,
  add column finder_failed_at timestamptz;
