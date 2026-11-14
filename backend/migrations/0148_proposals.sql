-- Proposals (proposals.rs, agent/proposals.rs): the agent writes a deal's proposal from the conversation, the team sets the
-- prices, and the buyer reads it at PUBLIC_URL/proposal/{token} and accepts it with their name, which wins the deal.

-- One proposal per deal. title is null until the agent first writes it. sections is [{title, body}]. items is
-- [{name, detail, quantity, unit_price, period}], unit_price in the deal's currency's main unit and period one of
-- one_time, month or year. Once accepted_at is set the proposal no longer changes.
create table proposals (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null unique references deals(id) on delete cascade,
    token text not null unique,
    run_id uuid references agent_runs(id) on delete set null,
    title text,
    intro text not null default '',
    sections jsonb not null default '[]',
    items jsonb not null default '[]',
    valid_until date,
    views int not null default 0,
    first_viewed_at timestamptz,
    last_viewed_at timestamptz,
    accepted_at timestamptz,
    accepted_name text not null default '',
    accepted_title text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal'));
