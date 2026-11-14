-- Call coaching (agent/coaching.rs): after a sales meeting or a call is summarized, the agent scores how it went and says
-- what to keep and what to do differently, with the moment and what to say instead. How much we talked and how many
-- questions we asked are counted from the transcript, never estimated. The Coaching page groups them by rep.

-- On by default: coaching a call costs about two cents, once per call.
alter table project_settings add column coach_calls boolean not null default true;

-- One per meeting or call. coaching is null until the run writes it: {score, next_step_agreed, next_step, strengths,
-- improve: [{moment, quote, instead}], covered}. owner_id is the lead's owner when it was queued, the rep it coaches.
create table call_coaching (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    lead_id uuid not null references leads(id) on delete cascade,
    meeting_id uuid unique references meetings(id) on delete cascade,
    call_id uuid unique references calls(id) on delete cascade,
    owner_id uuid references users(id) on delete set null,
    run_id uuid references agent_runs(id) on delete set null,
    talk_share int check (talk_share between 0 and 100),
    questions int not null default 0,
    coaching jsonb,
    created_at timestamptz not null default now(),
    check ((meeting_id is null) <> (call_id is null))
);
create index call_coaching_project_idx on call_coaching (project_id, created_at desc);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call'));
