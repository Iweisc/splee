-- Business cases (business_cases.rs, agent/business_cases.rs): for an open deal, the agent writes what the product is worth
-- to the buyer from the conversation, line by line with each number marked as theirs or our estimate. The buyer's
-- champion opens it at PUBLIC_URL/case/{token}, changes the numbers to their own and prints it for whoever signs.

-- business_case is {title, summary, problem, lines, outcomes, waiting}, null until the agent writes it. numbers are the
-- buyer's own [{quantity, unit_value, count}], one per line, null until they change any. skipped says why the agent
-- didn't write it.
create table business_cases (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null unique references deals(id) on delete cascade,
    token text not null unique default encode(gen_random_bytes(10), 'hex'),
    run_id uuid references agent_runs(id) on delete set null,
    business_case jsonb,
    skipped text not null default '',
    numbers jsonb,
    edited_by text not null default '',
    edited_at timestamptz,
    views int not null default 0,
    first_viewed_at timestamptz,
    last_viewed_at timestamptz,
    created_at timestamptz not null default now()
);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice', 'one_on_one', 'renewal', 'playbook', 'ramp', 'team_brief',
  'deal_desk', 'customer_story', 'deal_room', 'room_answer', 'feedback_scan', 'feature_shipped', 'business_case'));
