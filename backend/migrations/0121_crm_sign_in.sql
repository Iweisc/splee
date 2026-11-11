-- Salesforce, Zoho CRM and Dynamics 365 Sales connect by signing in (crm/signin.rs). Like every CRM's, {crm}_token_enc says
-- the CRM is connected and alerts match on it. It holds where the account is and only changes on reconnect, because the
-- refresh token, which Microsoft replaces each time it's used, and the access token have their own columns.
alter table project_settings
  add column salesforce_token_enc text not null default '',
  add column salesforce_connected boolean generated always as (salesforce_token_enc <> '') stored,
  add column salesforce_error text,
  add column salesforce_failed_at timestamptz,
  add column salesforce_refresh_enc text not null default '',
  add column salesforce_access_enc text not null default '',
  add column salesforce_expires_at timestamptz,
  -- Set when Salesforce turns down the refresh token, so it isn't sent again until someone signs in again.
  add column salesforce_needs_reconnect boolean not null default false,
  -- The host it's signed in to, shown on the integrations page.
  add column salesforce_site text not null default '',
  add column salesforce_sandbox boolean not null default false,
  -- The org's active Lead Status values as [{value, label}], and the one booked leads get. Empty leaves the status alone.
  add column salesforce_statuses jsonb not null default '[]',
  add column salesforce_booked_status text not null default '',
  add column zoho_token_enc text not null default '',
  add column zoho_connected boolean generated always as (zoho_token_enc <> '') stored,
  add column zoho_error text,
  add column zoho_failed_at timestamptz,
  add column zoho_refresh_enc text not null default '',
  add column zoho_access_enc text not null default '',
  add column zoho_expires_at timestamptz,
  add column zoho_needs_reconnect boolean not null default false,
  add column zoho_site text not null default '',
  add column dynamics_token_enc text not null default '',
  add column dynamics_connected boolean generated always as (dynamics_token_enc <> '') stored,
  add column dynamics_error text,
  add column dynamics_failed_at timestamptz,
  add column dynamics_refresh_enc text not null default '',
  add column dynamics_access_enc text not null default '',
  add column dynamics_expires_at timestamptz,
  add column dynamics_needs_reconnect boolean not null default false,
  add column dynamics_site text not null default '';

-- One per press of a CRM's sign-in button, deleted by the callback that uses it. Like social_oauth_states, the state sent to
-- the CRM is the nonce and an HMAC of it with the user and org, and only the nonce's hash is stored. `site` is the Dynamics
-- org's address, or 'sandbox' for a Salesforce sandbox.
create table crm_oauth_states (
    nonce_hash text primary key,
    user_id uuid not null references users(id) on delete cascade,
    org_id uuid not null references organizations(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    crm text not null check (crm in ('salesforce', 'zoho', 'dynamics')),
    code_verifier_enc text not null,
    site text not null default '',
    expires_at timestamptz not null,
    created_at timestamptz not null default now()
);
