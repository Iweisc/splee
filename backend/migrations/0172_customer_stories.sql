-- Customer stories (stories.rs, agent/stories.rs): the agent drafts a case study from a won deal's conversation and an email
-- asking the customer to look at it on their own page (/story/{token}). They fix the quote and add their numbers, choose
-- whether their name goes on it, and approve, and it becomes a case study in the project's Materials.

-- story is {title, anonymous_title, anonymous_name, summary, challenge, solution, results, quote}. The sections never name
-- the customer, so the anonymous version only swaps the title and who the quote is from.
create table customer_stories (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null unique references deals(id) on delete cascade,
    token text not null unique default encode(gen_random_bytes(10), 'hex'),
    run_id uuid references agent_runs(id) on delete set null,
    story jsonb,
    -- Why the agent wrote none, like a conversation too thin to tell the story.
    skipped text not null default '',
    email_id uuid references email_messages(id) on delete set null,
    views int not null default 0,
    last_viewed_at timestamptz,
    approved_at timestamptz,
    approved_name text not null default '',
    approved_title text not null default '',
    -- Whether the customer's company name goes on it. Null until they approve.
    named boolean,
    declined_at timestamptz,
    material_id uuid references materials(id) on delete set null,
    created_at timestamptz not null default now()
);

alter table email_messages drop constraint email_messages_kind_check;
alter table email_messages add constraint email_messages_kind_check
    check (kind in ('initial', 'follow_up', 'auto_reply', 'manual_reply', 'inbound', 'meeting_follow_up', 'deal_nudge', 'win_back', 'expansion',
                    'referral_ask', 'retention', 'story_ask'));

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice', 'one_on_one', 'renewal', 'playbook', 'ramp', 'team_brief',
  'deal_desk', 'customer_story'));
