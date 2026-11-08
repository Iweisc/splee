-- The last failed hot-lead alert per target, so the integrations page can show a broken endpoint.
-- Cleared when a delivery succeeds or the URL changes.
alter table project_settings
  add column webhook_error text,
  add column webhook_failed_at timestamptz,
  add column slack_error text,
  add column slack_failed_at timestamptz;
