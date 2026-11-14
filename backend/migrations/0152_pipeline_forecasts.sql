-- Pipeline forecasts (agent/forecast.rs): every Monday the agent reads a project's open deals, sorts them into what will
-- close this month (commit), what could (best case) and what won't (later), and picks the deals to work on this week and
-- the ones slipping. The Pipeline page's Forecast tab shows the latest, and the project's chats get it.

-- On by default: a forecast costs a few cents a week, only for projects with open deals.
alter table project_settings add column pipeline_forecast boolean not null default true;

-- One row per forecast, the newest being the project's. forecast is null until the run writes it:
-- {headline, month_end, deals: [{deal_id, category, why}], focus: [{deal_id, action}], slipping: [{deal_id, why}]}.
create table pipeline_forecasts (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    forecast jsonb,
    auto boolean not null default false,
    created_at timestamptz not null default now()
);
create index pipeline_forecasts_project_idx on pipeline_forecasts (project_id, created_at desc);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast'));
