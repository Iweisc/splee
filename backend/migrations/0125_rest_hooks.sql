-- REST hook subscriptions (rest_hooks.rs): Zapier, Make and n8n subscribe a URL to one lead event with an API key, and
-- unsubscribe it when the zap or scenario is turned off. Each gets the same signed JSON as the project's webhooks.
create table hook_subscriptions (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    -- A revoked key's subscriptions stop getting events.
    api_key_id uuid not null references api_keys(id) on delete cascade,
    -- Null for every project of the organization.
    project_id uuid references projects(id) on delete cascade,
    event text not null,
    target_url text not null,
    -- Only lead_added reads these: the last lead posted, so each new lead goes out once.
    cursor_at timestamptz not null default now(),
    cursor_id uuid not null default '00000000-0000-0000-0000-000000000000',
    error text,
    failed_at timestamptz,
    -- Set when the receiver answered 410 Gone, which is how Zapier says the zap was deleted.
    disabled_at timestamptz,
    created_at timestamptz not null default now()
);
create index hook_subscriptions_org_idx on hook_subscriptions (org_id, event) where disabled_at is null;
