alter table project_settings
  add column notion_token_enc text not null default '',
  add column notion_connected boolean generated always as (notion_token_enc <> '') stored,
  add column notion_error text,
  add column notion_failed_at timestamptz;
