-- Mailboxes connected by signing in to Google or Microsoft, which send and read over SMTP and IMAP with XOAUTH2 instead of a
-- password (mail/oauth.rs). 'password' is every mailbox from before.
alter table mailboxes
    add column provider text not null default 'password' check (provider in ('password', 'google', 'microsoft')),
    add column oauth_refresh_token_enc text not null default '',
    -- The last access token and when it runs out, so each connection doesn't trade the refresh token again.
    add column oauth_access_token_enc text not null default '',
    add column oauth_expires_at timestamptz,
    -- The provider refused the refresh token (invalid_grant), so nothing tries it again until someone signs in again.
    add column needs_reconnect boolean not null default false;

-- One per press of Connect Google or Connect Microsoft, deleted by the callback that uses it. The state sent to the provider
-- is the nonce and an HMAC of it with the user and org, and only the nonce's hash is stored.
create table mailbox_oauth_states (
    nonce_hash text primary key,
    user_id uuid not null references users(id) on delete cascade,
    org_id uuid not null references organizations(id) on delete cascade,
    provider text not null check (provider in ('google', 'microsoft')),
    code_verifier_enc text not null,
    expires_at timestamptz not null,
    created_at timestamptz not null default now()
);
