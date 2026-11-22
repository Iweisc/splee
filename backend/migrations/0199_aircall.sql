-- Calls made and taken in Aircall (aircall.rs, routes/aircall.rs). A project connects its Aircall company with an API ID and
-- token, kept as "{api_id}:{api_token}" encrypted with APP_SECRET, and Splee creates the webhook itself: webhook_id to delete
-- it on disconnect, and webhook_token_hash, crypto::keyed_hash of the token Aircall puts in each event's body to show it came
-- from that webhook. The webhook URL is /api/aircall-webhooks/{path_token}. A call with one of the project's leads, by their
-- number, is kept in calls with aircall_id, its length and outcome, and with Aircall's transcript the agent summarizes it like
-- a Twilio call.
create table aircall_connections (
    project_id uuid primary key references projects(id) on delete cascade,
    org_id uuid not null references organizations(id) on delete cascade,
    credentials_enc text not null,
    webhook_id text not null default '',
    webhook_token_hash text not null default '',
    path_token text not null unique default encode(gen_random_bytes(16), 'hex'),
    last_event_at timestamptz,
    error text not null default '',
    created_at timestamptz not null default now()
);

alter table calls add column aircall_id bigint;
create unique index calls_aircall_idx on calls (project_id, aircall_id) where aircall_id is not null;
