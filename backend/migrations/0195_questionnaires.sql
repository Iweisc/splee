-- Questionnaires (questionnaires.rs, agent/questionnaires.rs, routes/questionnaires.rs): a buyer's security questionnaire
-- or RFP, uploaded as a CSV or spreadsheet or pasted. The agent answers each question from the project's Materials and
-- the answers the team approved before, and says which it couldn't source. The team edits and approves the answers,
-- which join the project's answer library, and downloads the file back with them filled in.

-- headers and rows are the file's own, every row in order, so the download keeps every column and the rows without a
-- question, like section headings and instructions. question_column is where the question is, and answer_column is the
-- file's own column for answers, when it has one.
create table questionnaires (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    headers jsonb not null,
    rows jsonb not null,
    question_column int not null,
    answer_column int,
    run_id uuid references agent_runs(id) on delete set null,
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now()
);
create index questionnaires_project_idx on questionnaires (project_id, created_at);

-- confidence is library when an approved answer to the same question filled it, sourced or partial when the agent found
-- it in Materials, unknown when it couldn't, and null before it's answered.
create table questionnaire_items (
    id uuid primary key default gen_random_uuid(),
    questionnaire_id uuid not null references questionnaires(id) on delete cascade,
    -- The question's row in rows.
    position int not null,
    question text not null,
    section text not null default '',
    answer text not null default '',
    confidence text check (confidence in ('library', 'sourced', 'partial', 'unknown')),
    sources jsonb not null default '[]',
    approved_by uuid references users(id) on delete set null,
    approved_at timestamptz,
    unique (questionnaire_id, position)
);

-- Approved answers, one per question as written lowercase with only letters, digits and single spaces (key).
create table answer_library (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    key text not null,
    question text not null,
    answer text not null,
    approved_by uuid references users(id) on delete set null,
    updated_at timestamptz not null default now(),
    unique (project_id, key)
);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice', 'one_on_one', 'renewal', 'playbook', 'ramp', 'team_brief',
  'deal_desk', 'customer_story', 'deal_room', 'room_answer', 'feedback_scan', 'feature_shipped', 'business_case', 'questionnaire'));
