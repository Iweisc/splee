-- OAuth for the MCP server, so ChatGPT and Claude.ai connect with a sign-in instead of an API key (routes/oauth.rs).
-- Codes and tokens are stored hashed like API keys.

-- An app registered through POST /api/oauth/register, or one whose client_id is the https URL of its client ID metadata
-- document. fetched_at says when that document was last read, and is null for registered apps.
create table oauth_clients (
    id uuid primary key default gen_random_uuid(),
    client_id text not null unique,
    name text not null,
    redirect_uris text[] not null,
    secret_hash text,
    fetched_at timestamptz,
    created_at timestamptz not null default now()
);

-- One per Allow on the consent page, listed as a connected app. Revoking it ends every token it issued.
create table oauth_grants (
    id uuid primary key default gen_random_uuid(),
    client_id uuid not null references oauth_clients(id) on delete cascade,
    user_id uuid not null references users(id) on delete cascade,
    org_id uuid not null references organizations(id) on delete cascade,
    -- The MCP server's URL the tokens are bound to (RFC 8707).
    resource text not null,
    last_used_at timestamptz,
    revoked_at timestamptz,
    created_at timestamptz not null default now()
);
create index oauth_grants_org_idx on oauth_grants (org_id, created_at desc);

-- grant_id is set when the code is exchanged, so a second exchange can revoke what the first one got.
create table oauth_codes (
    code_hash text primary key,
    client_id uuid not null references oauth_clients(id) on delete cascade,
    user_id uuid not null references users(id) on delete cascade,
    org_id uuid not null references organizations(id) on delete cascade,
    redirect_uri text not null,
    code_challenge text not null,
    resource text not null,
    grant_id uuid references oauth_grants(id) on delete cascade,
    expires_at timestamptz not null,
    created_at timestamptz not null default now()
);

-- used_at is for refresh tokens: set when one is traded for a new pair. Trading it again revokes the grant.
create table oauth_tokens (
    token_hash text primary key,
    grant_id uuid not null references oauth_grants(id) on delete cascade,
    kind text not null check (kind in ('access', 'refresh')),
    expires_at timestamptz not null,
    used_at timestamptz,
    created_at timestamptz not null default now()
);
create index oauth_tokens_grant_idx on oauth_tokens (grant_id);
