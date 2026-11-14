-- Deal desk (deal_desk.rs, agent/deal_desk.rs): the organization's price list, a limit on how far a proposal may go below
-- it, and approvals for proposals past the limit, with the agent's read on each.

-- price is in the organization's currency's main unit, like proposal items. Archived ones stay for the proposals that use them.
create table products (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    name text not null,
    detail text not null default '',
    price numeric(14, 2) not null check (price >= 0),
    period text not null check (period in ('one_time', 'month', 'year')),
    archived_at timestamptz,
    created_at timestamptz not null default now()
);
create index products_org_idx on products (org_id) where archived_at is null;

-- Null means proposals need no approval.
alter table organizations add column discount_limit_pct int check (discount_limit_pct between 0 and 100);

-- The proposal's deepest discount off the price list, kept when its items are saved.
alter table proposals add column discount_pct numeric(5, 2) not null default 0;

-- read is the agent's {verdict, counter_pct, summary, reasons, ask_for}. A proposal is held from the buyer while its discount is
-- past the limit and no approval covers it.
create table discount_approvals (
    id uuid primary key default gen_random_uuid(),
    proposal_id uuid not null references proposals(id) on delete cascade,
    discount_pct numeric(5, 2) not null,
    reason text not null default '',
    asked_by uuid references users(id) on delete set null,
    run_id uuid references agent_runs(id) on delete set null,
    read jsonb,
    status text not null default 'asked' check (status in ('asked', 'approved', 'declined')),
    decided_by uuid references users(id) on delete set null,
    decided_at timestamptz,
    note text not null default '',
    created_at timestamptz not null default now()
);
create index discount_approvals_proposal_idx on discount_approvals (proposal_id, created_at desc);
create index discount_approvals_asked_idx on discount_approvals (created_at) where status = 'asked';

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice', 'one_on_one', 'renewal', 'playbook', 'ramp', 'team_brief',
  'deal_desk'));
