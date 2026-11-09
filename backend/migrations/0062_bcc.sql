alter table project_settings
  add column bcc_email text not null default '',
  add column bcc_error text,
  add column bcc_failed_at timestamptz;
