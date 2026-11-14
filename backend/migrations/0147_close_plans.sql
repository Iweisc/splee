-- Close plans (close_plans.rs, agent/close_plans.rs): a deal's steps to a signed deal, the buyer's and ours, drafted by the
-- agent from the conversation and shared with the buyer at PUBLIC_URL/plan/{token}, where they tick off their own steps.

-- One plan per deal. goal is null until the agent first writes it. views counts loads of the public page that the
-- team's own preview didn't make.
create table close_plans (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null unique references deals(id) on delete cascade,
    token text not null unique,
    run_id uuid references agent_runs(id) on delete set null,
    goal text,
    views int not null default 0,
    first_viewed_at timestamptz,
    last_viewed_at timestamptz,
    created_at timestamptz not null default now()
);

-- side is whose step it is. done_by is who ticked it, the team in the app or the buyer on the page.
create table close_plan_steps (
    id uuid primary key default gen_random_uuid(),
    plan_id uuid not null references close_plans(id) on delete cascade,
    position int not null,
    title text not null,
    detail text not null default '',
    side text not null check (side in ('us', 'them')),
    due_date date,
    done_at timestamptz,
    done_by text check (done_by in ('us', 'them')),
    created_at timestamptz not null default now()
);
create index close_plan_steps_plan_idx on close_plan_steps (plan_id, position);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan'));
