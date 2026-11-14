-- 1:1 prep: the agent reads a teammate's month against their target, their open deals, recent wins and losses, call coaching
-- and practice calls, and writes what a manager should bring to their one-on-one. Org-wide, since a teammate's deals span projects.
create table one_on_ones (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    user_id uuid not null references users(id) on delete cascade,
    requested_by uuid references users(id) on delete set null,
    run_id uuid references agent_runs(id) on delete set null,
    prep jsonb,
    created_at timestamptz not null default now()
);
create index one_on_ones_user_idx on one_on_ones (org_id, user_id, created_at desc);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice', 'one_on_one'));
