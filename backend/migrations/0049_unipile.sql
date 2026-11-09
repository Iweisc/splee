-- Unipile sends due LinkedIn steps from the user's own account. unipile_dsn is the account's API base URL.
alter table project_settings
  add column unipile_dsn text not null default '',
  add column unipile_token_enc text not null default '',
  add column unipile_connected boolean generated always as (unipile_token_enc <> '') stored,
  add column unipile_account_id text not null default '',
  add column unipile_account_name text not null default '',
  add column unipile_error text,
  add column unipile_failed_at timestamptz;

-- sent_via is 'unipile' for a step Splee sent, '' for one the user ticked off. send_error keeps a step Splee couldn't send
-- on the page for the user, out of the automatic sends.
alter table linkedin_messages
  add column sent_via text not null default '',
  add column send_error text;
