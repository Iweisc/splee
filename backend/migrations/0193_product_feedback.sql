-- Product feedback (agent/feedback.rs, routes/feedback.rs): the agent reads what buyers write and say, in replies, meeting
-- transcripts, win/loss reviews and deal room questions, and keeps the list of what they ask the product to do, with their
-- words and the deals behind each. When one ships, it drafts an email to each person who asked.

create table feedback_requests (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    title text not null,
    detail text not null default '',
    status text not null default 'open' check (status in ('open', 'planned', 'shipped', 'declined')),
    -- What shipped and where to read about it, which the emails to the people who asked use.
    shipped_note text not null default '',
    shipped_at timestamptz,
    announce_run_id uuid references agent_runs(id) on delete set null,
    created_at timestamptz not null default now()
);
create index feedback_requests_project_idx on feedback_requests (project_id, created_at);

-- source_id is the email_messages, meetings, deal_reviews or deal_room_questions row the quote is from.
create table feedback_mentions (
    id uuid primary key default gen_random_uuid(),
    request_id uuid not null references feedback_requests(id) on delete cascade,
    lead_id uuid not null references leads(id) on delete cascade,
    source text not null check (source in ('email', 'meeting', 'review', 'room')),
    source_id uuid not null,
    quote text not null,
    said_at timestamptz not null,
    created_at timestamptz not null default now(),
    unique (request_id, source, source_id)
);
create index feedback_mentions_lead_idx on feedback_mentions (lead_id);

-- One per person who asked, once the request ships: the email the agent drafted them, or why it didn't.
create table feedback_notices (
    id uuid primary key default gen_random_uuid(),
    request_id uuid not null references feedback_requests(id) on delete cascade,
    lead_id uuid not null references leads(id) on delete cascade,
    email_id uuid references email_messages(id) on delete set null,
    skipped text not null default '',
    created_at timestamptz not null default now(),
    unique (request_id, lead_id)
);

-- How far the agent has read each project's sources. A project starts 90 days back.
create table feedback_scans (
    project_id uuid primary key references projects(id) on delete cascade,
    read_to timestamptz not null default now() - interval '90 days',
    -- With read_to, the last source read, since sources can share a time, like emails imported together.
    read_id uuid not null default '00000000-0000-0000-0000-000000000000',
    run_id uuid references agent_runs(id) on delete set null,
    scanned_at timestamptz
);

alter table email_messages drop constraint email_messages_kind_check;
alter table email_messages add constraint email_messages_kind_check
    check (kind in ('initial', 'follow_up', 'auto_reply', 'manual_reply', 'inbound', 'meeting_follow_up', 'deal_nudge', 'win_back', 'expansion',
                    'referral_ask', 'retention', 'story_ask', 'feature_shipped'));

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice', 'one_on_one', 'renewal', 'playbook', 'ramp', 'team_brief',
  'deal_desk', 'customer_story', 'deal_room', 'room_answer', 'feedback_scan', 'feature_shipped'));
