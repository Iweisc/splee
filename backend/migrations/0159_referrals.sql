-- Referrals (agent/referrals.rs): about six weeks after a deal is won, the agent reads whether the buyer is happy, picks
-- companies like theirs from the company database, and drafts an email asking for an introduction, with a page of their own
-- to forward. Someone who leaves their details there becomes a lead credited to the customer.

alter table project_settings add column ask_for_referrals boolean not null default true;

-- One per won deal. companies is up to three [{name, domain, why}] the email names. views counts the page's opens.
create table deal_referrals (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null unique references deals(id) on delete cascade,
    token text not null unique default encode(gen_random_bytes(10), 'hex'),
    run_id uuid references agent_runs(id) on delete set null,
    email_id uuid references email_messages(id) on delete set null,
    why text not null default '',
    companies jsonb not null default '[]',
    skipped text not null default '',
    written_at timestamptz,
    dismissed_at timestamptz,
    views integer not null default 0,
    last_viewed_at timestamptz,
    created_at timestamptz not null default now()
);
create index deal_referrals_project_idx on deal_referrals (project_id, created_at desc);

-- The customer who sent a lead that came in through their referral page.
alter table leads add column referral_id uuid references deal_referrals(id) on delete set null;
create index leads_referral_idx on leads (referral_id) where referral_id is not null;

alter table leads drop constraint leads_source_check;
alter table leads add constraint leads_source_check check (source in ('agent', 'import', 'api', 'form', 'linkedin', 'chat', 'google_maps', 'referral'));

alter table email_messages drop constraint email_messages_kind_check;
alter table email_messages add constraint email_messages_kind_check
    check (kind in ('initial', 'follow_up', 'auto_reply', 'manual_reply', 'inbound', 'meeting_follow_up', 'deal_nudge', 'win_back', 'expansion',
                    'referral_ask'));

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask'));
