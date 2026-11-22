-- Calls made and taken in Dialpad (dialpad/, routes/dialpad.rs). A project connects its Dialpad company with an API key, kept
-- encrypted with APP_SECRET. Splee creates a webhook signed with a secret of its own, kept the same way to check each event's
-- JWT, and a subscription to the call states it reads: webhook_id and subscription_id to delete them on disconnect. The
-- webhook URL is /api/dialpad-webhooks/{path_token}. A call with one of the project's leads is kept in calls with dialpad_id,
-- the same way as aircall_id (cloud_phone.rs).
create table dialpad_connections (
    project_id uuid primary key references projects(id) on delete cascade,
    org_id uuid not null references organizations(id) on delete cascade,
    api_key_enc text not null,
    webhook_id text not null default '',
    subscription_id text not null default '',
    secret_enc text not null default '',
    path_token text not null unique default encode(gen_random_bytes(16), 'hex'),
    last_event_at timestamptz,
    error text not null default '',
    created_at timestamptz not null default now()
);

alter table calls add column dialpad_id bigint;
create unique index calls_dialpad_idx on calls (project_id, dialpad_id) where dialpad_id is not null;
