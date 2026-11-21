-- Handoffs (handoffs.rs, agent/handoffs.rs): once a deal is won, the agent writes whoever takes the customer on what they
-- need from the sale, and the deal's close plan carries on as the buyer's onboarding plan at the same link.

-- One handoff per deal. handoff is null until the agent first writes it: {summary, bought, promises: [{promise, quote}],
-- people: [{name, role, note}], success: [], risks: []}. owner_id is the teammate who takes the customer on.
create table handoffs (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null unique references deals(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    handoff jsonb,
    owner_id uuid references users(id) on delete set null,
    handed_by uuid references users(id) on delete set null,
    handed_at timestamptz,
    created_at timestamptz not null default now()
);

-- A plan with an onboarding goal shows the buyer its onboarding steps, and the close steps as done history.
alter table close_plans add column onboarding_goal text;
alter table close_plan_steps add column phase text not null default 'close' check (phase in ('close', 'onboarding'));

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice', 'one_on_one', 'renewal', 'playbook', 'ramp', 'team_brief',
  'deal_desk', 'customer_story', 'deal_room', 'room_answer', 'feedback_scan', 'feature_shipped', 'business_case', 'questionnaire', 'handoff'));
