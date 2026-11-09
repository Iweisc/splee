alter table project_settings
  add column zendesk_sell_token_enc text not null default '',
  add column zendesk_sell_connected boolean generated always as (zendesk_sell_token_enc <> '') stored,
  add column zendesk_sell_error text,
  add column zendesk_sell_failed_at timestamptz;
