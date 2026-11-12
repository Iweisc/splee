-- Sales meetings from Fireflies, Fathom, Gong and Zoom (meetings/, routes/meetings.rs, agent/meetings.rs). A meeting with a lead
-- is kept with its transcript, and the agent summarizes it, drafts the follow-up and suggests the deal's next stage.

-- One per project and provider. credentials_enc is encrypted with APP_SECRET: the API key for Fireflies and Fathom,
-- "{access key}:{secret}" for Gong, and {"account_id", "client_id", "client_secret"} as JSON for Zoom's Server-to-Server
-- OAuth app. signing_secret_enc checks the webhooks: Fireflies' webhook secret, Fathom's whsec_ secret and Zoom's secret
-- token. Gong has no webhook, so it's polled: sync_cursor is the end of the last window read, and the next poll reads from
-- a few hours before it. webhook_token is the secret in the webhook URL, /api/meetings/{provider}/{webhook_token}.
create table meeting_connections (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    provider text not null check (provider in ('fireflies', 'fathom', 'gong', 'zoom')),
    credentials_enc text not null,
    signing_secret_enc text not null default '',
    webhook_token text not null default encode(gen_random_bytes(16), 'hex'),
    sync_cursor timestamptz,
    synced_at timestamptz,
    last_event_at timestamptz,
    error text not null default '',
    created_at timestamptz not null default now(),
    unique (project_id, provider)
);
create unique index meeting_connections_token_idx on meeting_connections (webhook_token);

-- Webhook deliveries already handled, so the same event twice changes nothing.
create table meeting_events (
    connection_id uuid not null references meeting_connections(id) on delete cascade,
    event_id text not null,
    received_at timestamptz not null default now(),
    primary key (connection_id, event_id)
);

-- Only meetings with a lead are kept, one per provider's meeting. attendees is [{"name", "email"}] without the team.
-- booking_id is the Splee booking it was, when one was booked for the lead within 30 minutes of its start.
-- transcript is "Speaker: text" lines, at most 200,000 characters. provider_summary is the provider's own, when it has one.
-- The agent fills summary, next_steps (a list of strings) and follow_up_email_id, the drafted email waiting for approval,
-- and may suggest a stage: stage_status is pending until someone accepts or dismisses it, none when it had no suggestion.
create table meetings (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    lead_id uuid not null references leads(id) on delete cascade,
    connection_id uuid references meeting_connections(id) on delete set null,
    provider text not null check (provider in ('fireflies', 'fathom', 'gong', 'zoom')),
    external_id text not null,
    booking_id uuid references bookings(id) on delete set null,
    title text not null default '',
    started_at timestamptz not null,
    duration_secs integer,
    attendees jsonb not null default '[]',
    provider_summary text not null default '',
    recording_url text not null default '',
    transcript text not null default '',
    summary text not null default '',
    next_steps jsonb not null default '[]',
    follow_up_email_id uuid references email_messages(id) on delete set null,
    suggested_stage_id uuid references deal_stages(id) on delete set null,
    stage_reason text not null default '',
    stage_status text not null default 'none' check (stage_status in ('none', 'pending', 'accepted', 'dismissed')),
    summarized_at timestamptz,
    created_at timestamptz not null default now(),
    unique (project_id, provider, external_id)
);
create index meetings_lead_idx on meetings (lead_id, started_at);

-- The follow-up the agent drafts after a meeting. It waits for approval like any draft.
alter table email_messages drop constraint email_messages_kind_check;
alter table email_messages add constraint email_messages_kind_check
    check (kind in ('initial', 'follow_up', 'auto_reply', 'manual_reply', 'inbound', 'meeting_follow_up'));

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes'));
