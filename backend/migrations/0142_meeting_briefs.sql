-- Meeting briefs (agent/briefs.rs): before a lead's booked call, the agent writes the team a page on who they are, what
-- their company is up to, what was said so far and what to ask, and posts it to the project's chats and the lead's owner.

-- On by default: a brief costs a few cents and a booked call is worth far more.
alter table project_settings add column meeting_briefs boolean not null default true;

-- One brief per lead and meeting time, so a moved call gets a new one. meeting_at is null for a brief asked for by hand
-- without a call booked. brief is null until the run writes it. auto is false for one asked for from the lead's window,
-- which isn't posted anywhere. posted is whether this call's brief went to the chats and the owner, kept when one is
-- rewritten by hand, so the worker posts a brief for every call once.
create table meeting_briefs (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    lead_id uuid not null references leads(id) on delete cascade,
    meeting_at timestamptz,
    auto boolean not null default true,
    posted boolean not null default false,
    run_id uuid references agent_runs(id) on delete set null,
    brief jsonb,
    written_at timestamptz,
    created_at timestamptz not null default now(),
    unique (lead_id, meeting_at)
);
create index meeting_briefs_lead_idx on meeting_briefs (lead_id, created_at desc);

-- The worker looks for calls in the next day every few minutes.
create index leads_meeting_at_idx on leads (meeting_at) where meeting_at is not null;

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief'));
