-- Ad audiences on LinkedIn, Google Ads and Meta (ads/, routes/ads.rs): a campaign's contacted leads, and optionally the
-- project's hot leads, kept in sync every hour as a Matched Audience, a Customer Match list or a Custom Audience.

-- One per project and platform, from signing in. The tokens are encrypted with APP_SECRET. Meta has no refresh token, so its
-- refresh_enc stays empty and expires_at null. ad_accounts is what the sign-in can use, [{"id": "...", "name": "..."}], and
-- ad_account_id the one picked from it. held_until is how long the platform asked Splee to slow down for.
create table ad_connections (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    platform text not null check (platform in ('linkedin', 'google', 'meta')),
    refresh_enc text not null default '',
    access_enc text not null default '',
    expires_at timestamptz,
    ad_accounts jsonb not null default '[]'::jsonb,
    ad_account_id text not null default '',
    ad_account_name text not null default '',
    needs_reconnect boolean not null default false,
    error text,
    failed_at timestamptz,
    held_until timestamptz,
    created_at timestamptz not null default now(),
    unique (project_id, platform)
);

-- A campaign's audience on one platform, or the project's hot leads when kind is 'hot'. external_id is the platform's id for
-- it, empty until it's made there. size is what the platform last said it holds. tried_at schedules the hourly sync and
-- synced_at is the last one that went through.
create table ad_audiences (
    id uuid primary key default gen_random_uuid(),
    connection_id uuid not null references ad_connections(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    segment_id uuid references segments(id) on delete cascade,
    kind text not null check (kind in ('campaign', 'hot')),
    name text not null,
    external_id text not null default '',
    size bigint,
    synced_at timestamptz,
    tried_at timestamptz,
    claimed_at timestamptz,
    error text,
    created_at timestamptz not null default now(),
    check ((kind = 'campaign') = (segment_id is not null))
);
create unique index ad_audiences_campaign_idx on ad_audiences (connection_id, segment_id) where kind = 'campaign';
create unique index ad_audiences_hot_idx on ad_audiences (connection_id) where kind = 'hot';
create index ad_audiences_segment_idx on ad_audiences (segment_id);

-- Everyone who may be in each audience on the platform, as the hashes that were sent, so a sync only sends the difference and
-- can remove a lead after it's deleted. lead_id has no foreign key for that reason. A row is written as 'adding' before the
-- lead is sent and becomes 'added' once the platform confirms it, so a lead from an upload that partly failed is still taken
-- out later. 'removing' is a removal the platform hasn't confirmed yet, and the row goes once it has. job is the Google
-- offline user data job the change went in, until a sync reads how it went.
create table ad_audience_members (
    audience_id uuid not null references ad_audiences(id) on delete cascade,
    lead_id uuid not null,
    email_hash text not null,
    phone_hash text not null default '',
    state text not null default 'adding' check (state in ('adding', 'added', 'removing')),
    job text not null default '',
    added_at timestamptz not null default now(),
    primary key (audience_id, lead_id)
);

create table ad_oauth_states (
    nonce_hash text primary key,
    user_id uuid not null references users(id) on delete cascade,
    org_id uuid not null references organizations(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    platform text not null,
    code_verifier_enc text not null,
    expires_at timestamptz not null
);
