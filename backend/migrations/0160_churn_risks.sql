-- Customers at risk (agent/churn.rs): when the person who bought leaves the company, a customer gets a refund, their
-- payments stop coming, or they write that they're unhappy, the agent reads the account, says how likely they are to leave
-- and what to do, and drafts an email in the thread when one would help.

alter table project_settings add column watch_churn boolean not null default true;

-- One per look at a won deal. signal is the warning signs that started it, empty when the team asked. risk is high, medium
-- or low once the agent read it. play is what the team should do. note says why there's no email when there's a risk but no
-- draft: the agent saw nothing to write, or one is waiting, or the lead can't be emailed.
create table churn_risks (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null references deals(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    email_id uuid references email_messages(id) on delete set null,
    signal text not null default '',
    risk text check (risk in ('high', 'medium', 'low')),
    why text not null default '',
    play text not null default '',
    note text not null default '',
    skipped text not null default '',
    written_at timestamptz,
    dismissed_at timestamptz,
    created_at timestamptz not null default now()
);
create index churn_risks_project_idx on churn_risks (project_id, created_at desc);
create index churn_risks_deal_idx on churn_risks (deal_id, created_at desc);

alter table email_messages drop constraint email_messages_kind_check;
alter table email_messages add constraint email_messages_kind_check
    check (kind in ('initial', 'follow_up', 'auto_reply', 'manual_reply', 'inbound', 'meeting_follow_up', 'deal_nudge', 'win_back', 'expansion',
                    'referral_ask', 'retention'));

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk'));
