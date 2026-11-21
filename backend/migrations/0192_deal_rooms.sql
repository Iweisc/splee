-- Deal rooms (deal_rooms.rs, agent/deal_rooms.rs): one page per open deal for the buyer, at PUBLIC_URL/room/{token}, with a
-- welcome the agent writes, the close plan and the proposal, the case studies from Materials that fit them best, and
-- questions the buyer asks, which the agent drafts an answer to for the team to publish.

-- room is {headline, intro, proof: [{material_id, why}]}, null until the agent first writes it. views counts loads of the
-- page that the team's own preview didn't make.
create table deal_rooms (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null unique references deals(id) on delete cascade,
    token text not null unique default encode(gen_random_bytes(10), 'hex'),
    run_id uuid references agent_runs(id) on delete set null,
    room jsonb,
    views int not null default 0,
    first_viewed_at timestamptz,
    last_viewed_at timestamptz,
    created_at timestamptz not null default now()
);

-- draft is the agent's answer, answer what the team published, and the page shows only answered questions.
create table deal_room_questions (
    id uuid primary key default gen_random_uuid(),
    room_id uuid not null references deal_rooms(id) on delete cascade,
    name text not null,
    question text not null,
    run_id uuid references agent_runs(id) on delete set null,
    draft text not null default '',
    answer text,
    answered_by uuid references users(id) on delete set null,
    answered_at timestamptz,
    created_at timestamptz not null default now()
);
create index deal_room_questions_room_idx on deal_room_questions (room_id, created_at);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice', 'one_on_one', 'renewal', 'playbook', 'ramp', 'team_brief',
  'deal_desk', 'customer_story', 'deal_room', 'room_answer'));
