alter table project_settings
  add column odoo_token_enc text not null default '',
  add column odoo_connected boolean generated always as (odoo_token_enc <> '') stored,
  add column odoo_error text,
  add column odoo_failed_at timestamptz;
