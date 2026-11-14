-- Nudges on quiet deals (agent/nudges.rs): when an open deal goes 14 days without an email either way, a call or a stage
-- change, the agent drafts a short email in the thread with a reason to answer now. It waits for approval like any draft.

alter table project_settings add column nudge_quiet_deals boolean not null default true;

-- One row per nudge. quiet_since is the deal's last touch when it was queued, so the worker nudges once per quiet stretch.
-- email_id is the draft, and why is the agent's read on what stalled the deal.
create table deal_nudges (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null references deals(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    email_id uuid references email_messages(id) on delete set null,
    quiet_since timestamptz not null,
    auto boolean not null default true,
    why text not null default '',
    written_at timestamptz,
    created_at timestamptz not null default now()
);
create unique index deal_nudges_once_per_quiet on deal_nudges (deal_id, quiet_since) where auto;
create index deal_nudges_deal_idx on deal_nudges (deal_id, created_at desc);

alter table email_messages drop constraint email_messages_kind_check;
alter table email_messages add constraint email_messages_kind_check
    check (kind in ('initial', 'follow_up', 'auto_reply', 'manual_reply', 'inbound', 'meeting_follow_up', 'deal_nudge'));

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge'));
