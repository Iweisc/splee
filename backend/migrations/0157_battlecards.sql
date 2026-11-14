-- Battlecards (agent/battlecards.rs): for each competitor, the agent reads their website, the deals we won or lost against
-- them and what buyers said about them, and writes how to sell against them. The Competitors page and the window of a
-- deal that mentions them show it.

alter table project_settings add column write_battlecards boolean not null default true;

-- One card per competitor. card is null until the run writes it, and asked_at is when it was last asked for.
create table battlecards (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    competitor_id uuid not null unique references competitors(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    card jsonb,
    asked_at timestamptz not null default now(),
    written_at timestamptz
);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard'));
