-- Renewals: a won deal renews on its renews_on, or else on the anniversary of the win, a year at a time. 90 days before the
-- next one the agent reads the account and writes how likely they are to renew, the steps to get there, and an email to open
-- the conversation. One row per deal and renewal date.
alter table deals add column renews_on date;
alter table project_settings add column prep_renewals boolean not null default true;

create table renewals (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null references deals(id) on delete cascade,
    renews_on date not null,
    run_id uuid references agent_runs(id) on delete set null,
    risk text check (risk in ('high', 'medium', 'low')),
    why text not null default '',
    steps jsonb not null default '[]',
    email_id uuid references email_messages(id) on delete set null,
    note text not null default '',
    skipped text not null default '',
    written_at timestamptz,
    created_at timestamptz not null default now(),
    unique (deal_id, renews_on)
);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice', 'one_on_one', 'renewal'));
