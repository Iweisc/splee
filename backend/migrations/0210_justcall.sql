-- Calls made and taken in JustCall (justcall/, routes/justcall.rs). A project connects its JustCall account with an API key and
-- secret, kept encrypted with APP_SECRET. JustCall signs each event with the secret. Splee adds its webhook URL,
-- /api/justcall-webhooks/{path_token}, to the call events it reads, and keeps each URL's id in url_ids to remove them on
-- disconnect. A call with one of the project's leads is kept in calls with justcall_id, the same way as dialpad_id (cloud_phone.rs).
create table justcall_connections (
    project_id uuid primary key references projects(id) on delete cascade,
    org_id uuid not null references organizations(id) on delete cascade,
    api_key_enc text not null,
    api_secret_enc text not null,
    url_ids text[] not null default '{}',
    path_token text not null unique,
    last_event_at timestamptz,
    error text not null default '',
    created_at timestamptz not null default now()
);

alter table calls add column justcall_id bigint;
create unique index calls_justcall_idx on calls (project_id, justcall_id) where justcall_id is not null;
