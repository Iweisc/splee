-- Practice calls (agent/practice.rs): a rep rehearses the next call on an open deal with the agent playing the buyer, built
-- from what Splee knows about them, then gets scored.

-- persona is the buyer the agent plays: {name, title, company, situation, cares_about, objections, agrees_if}, kept from
-- the rep until the session is scored. turns are [{who: "buyer" | "rep", text}], oldest first. ended is set when the buyer
-- hangs up or the rep asks to be scored. score is {score, summary, went_well, try_next, moments: [{you_said, try}]}. A step
-- that failed says why on its run.
create table practice_sessions (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null references deals(id) on delete cascade,
    user_id uuid references users(id) on delete set null,
    run_id uuid references agent_runs(id) on delete set null,
    scenario text not null check (scenario in ('next_call', 'pricing', 'competitor')),
    persona jsonb,
    turns jsonb not null default '[]',
    ended boolean not null default false,
    score jsonb,
    created_at timestamptz not null default now()
);
create index practice_sessions_deal_idx on practice_sessions (deal_id, created_at desc);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice'));
