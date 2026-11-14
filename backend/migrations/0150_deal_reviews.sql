-- Win/loss reviews (agent/reviews.rs): once a deal is won or lost, the agent reads its whole story and says why, with the
-- turning point, the competitor if there was one, and a lesson. The pipeline's Win/loss view groups them.

-- One review per deal. A deal closed again after its review gets a new one in the same row.
-- reason is null until the run writes it.
create table deal_reviews (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null unique references deals(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    outcome text not null check (outcome in ('won', 'lost')),
    reason text check (reason in ('value', 'fit', 'champion', 'timing', 'price', 'proof', 'competitor', 'no_decision', 'went_dark', 'other')),
    story text not null default '',
    turning_point text not null default '',
    competitor text not null default '',
    lesson text not null default '',
    created_at timestamptz not null default now()
);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review'));
