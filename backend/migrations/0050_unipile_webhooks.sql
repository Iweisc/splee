-- Unipile posts LinkedIn messages and new connections to /api/unipile/{unipile_webhook_token}. unipile_webhook_ids are the
-- webhooks Splee made there, removed on disconnect.
alter table project_settings
  add column unipile_webhook_token text not null default encode(gen_random_bytes(16), 'hex'),
  add column unipile_webhook_ids text[] not null default '{}';
create unique index project_settings_unipile_webhook_token_idx on project_settings (unipile_webhook_token);

-- LinkedIn's id for the lead, saved when Splee looks their profile up, which is how Unipile names who wrote.
alter table leads add column linkedin_provider_id text not null default '';
