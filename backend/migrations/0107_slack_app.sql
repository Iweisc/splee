-- The Splee Slack app (slack.rs, routes/slack.rs): an organization adds it to one Slack workspace, each project picks a
-- channel, and hot replies and emails waiting for approval are acted on from there.

-- One workspace per organization and one organization per workspace, so an interaction's team id names the organization.
create table slack_installs (
    org_id uuid primary key references organizations(id) on delete cascade,
    team_id text not null unique,
    team_name text not null default '',
    bot_user_id text not null default '',
    bot_token_enc text not null,
    installed_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now()
);

-- One per press of Add to Slack, deleted by the callback that uses it, like mailbox_oauth_states.
create table slack_oauth_states (
    nonce_hash text primary key,
    user_id uuid not null references users(id) on delete cascade,
    org_id uuid not null references organizations(id) on delete cascade,
    code_verifier_enc text not null,
    expires_at timestamptz not null,
    created_at timestamptz not null default now()
);

create table slack_channels (
    project_id uuid primary key references projects(id) on delete cascade,
    channel_id text not null,
    channel_name text not null default '',
    -- The day the approval digest last went out, so a second check in the digest hour doesn't post it again.
    digest_on date,
    error text,
    failed_at timestamptz,
    created_at timestamptz not null default now()
);

-- Who acted on a reply or an email from Slack. The primary key makes a second click, or a second person, lose the race.
create table slack_actions (
    target_id uuid primary key,
    org_id uuid not null references organizations(id) on delete cascade,
    action text not null,
    user_id uuid references users(id) on delete set null,
    slack_user_id text not null,
    created_at timestamptz not null default now()
);
