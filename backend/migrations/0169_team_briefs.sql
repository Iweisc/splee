-- Team briefs (agent/team_brief.rs): every Monday the agent reads each teammate's month, deals, coached and practice calls,
-- assigned practice and ramp, and writes owners and admins who needs what this week, in the order to talk to them.

-- brief is {headline, people: [{user_id, name, note, action}], risks}. auto is a Monday one, emailed to owners and admins.
create table team_briefs (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    brief jsonb,
    auto boolean not null default false,
    created_at timestamptz not null default now()
);
create index team_briefs_org_idx on team_briefs (org_id, created_at desc);

alter table organizations add column team_brief boolean not null default true;

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice', 'one_on_one', 'renewal', 'playbook', 'ramp', 'team_brief'));
