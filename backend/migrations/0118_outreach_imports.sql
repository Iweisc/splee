-- Campaigns brought over from another outreach tool (Instantly, Smartlead or lemlist) with its API key (outreach/). The key is
-- encrypted with APP_SECRET while the import waits or runs, and blanked when it ends, done or failed.
-- campaigns holds the user's picks and, as the worker gets to each one, what it did:
-- [{"id": "...", "name": "...", "leads": 120, "status": "queued" | "running" | "done" | "failed", "segment_id": uuid | null,
--   "steps": 3, "added": 0, "duplicates": 0, "suppressed": 0, "error": ""}]
create table outreach_imports (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    tool text not null check (tool in ('instantly', 'smartlead', 'lemlist')),
    status text not null default 'queued' check (status in ('queued', 'running', 'done', 'failed')),
    api_key_enc text not null default '',
    campaigns jsonb not null default '[]',
    include_blocklist boolean not null default true,
    -- Entries of the tool's account-wide blocklist that were new to the suppression list.
    blocklist_added integer not null default 0,
    error text not null default '',
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now(),
    finished_at timestamptz
);
create index outreach_imports_project_idx on outreach_imports (project_id, created_at desc);
create unique index outreach_imports_one_active on outreach_imports (project_id) where status in ('queued', 'running');

-- A campaign brought over from a tool keeps the tool's name and its id there, so importing it again fills the same draft.
alter table segments add column imported_from text, add column imported_id text;
create unique index segments_imported_idx on segments (project_id, imported_from, imported_id) where imported_id is not null;

-- The emails of a campaign brought over from another tool, as they were written there, with the tool's variables turned into
-- Splee's fields like {first_name} where they map. Position 0 is the first email and n is follow-up n, sent delay_days after
-- the email before it. The writer keeps to the step that matches the email it writes.
create table segment_steps (
    segment_id uuid not null references segments(id) on delete cascade,
    position integer not null check (position >= 0),
    delay_days integer not null default 0,
    subject text not null default '',
    body text not null default '',
    primary key (segment_id, position)
);
