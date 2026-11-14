-- Ramp plans (agent/ramp.rs): the agent turns a project's playbook into a new teammate's first weeks, with practice calls
-- on the team's open deals that they pass by scoring at least the assignment's pass_score.

-- plan is {summary, weeks: [{title, goal, steps, practice: [{deal_id, company, scenario, skill, drill}]}]}. Writing a new
-- one for the same teammate and project replaces the old one and its practice.
create table ramps (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    user_id uuid not null references users(id) on delete cascade,
    requested_by uuid references users(id) on delete set null,
    run_id uuid references agent_runs(id) on delete set null,
    plan jsonb,
    created_at timestamptz not null default now()
);
create index ramps_project_user_idx on ramps (project_id, user_id, created_at desc);

-- An assignment comes from a 1:1 prep or a ramp. A 1:1's is done once it's scored, a ramp's once it scores pass_score.
alter table practice_assignments alter column one_on_one_id drop not null;
alter table practice_assignments add column ramp_id uuid references ramps(id) on delete cascade;
alter table practice_assignments add column pass_score int;
alter table practice_assignments add constraint practice_assignments_source check (num_nonnulls(one_on_one_id, ramp_id) = 1);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice', 'one_on_one', 'renewal', 'playbook', 'ramp'));
