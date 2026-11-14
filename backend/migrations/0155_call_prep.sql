-- Call prep (agent/call_prep.rs): before an open call comes due, the agent writes its talk track: an opener tied to what
-- the lead did, questions, likely objections with answers, a voicemail and a reminder from the rep's own coaching.

-- On by default: a talk track costs about a cent and a half, once per call.
alter table project_settings add column prep_calls boolean not null default true;

-- One per call. prep is null until the run writes it: {opener, questions, objections: [{objection, answer}], voicemail,
-- reminder}. Writing it again replaces the row's run and clears prep.
create table call_prep (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    call_id uuid not null unique references calls(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    prep jsonb,
    created_at timestamptz not null default now()
);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep'));
