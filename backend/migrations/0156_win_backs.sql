-- Win-backs (agent/win_backs.rs): once a deal is lost, the agent picks the day to try again from why it was lost, and on
-- that day, or sooner when the buyer comes back to our website, drafts the email for approval.

alter table project_settings add column win_back_lost_deals boolean not null default true;

-- One row per lost deal. lost_at is the loss the plan is for, so a deal lost again gets a new plan in the same row.
-- reach_out_on is null until the plan is written. why says why that day, angle what to lead with, and came_back what
-- brought the day forward. written_at is set once the email is drafted, or with skipped saying why it wasn't.
create table deal_win_backs (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null unique references deals(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    email_id uuid references email_messages(id) on delete set null,
    lost_at timestamptz not null,
    reach_out_on date,
    why text not null default '',
    angle text not null default '',
    came_back text not null default '',
    skipped text not null default '',
    written_at timestamptz,
    dismissed_at timestamptz,
    created_at timestamptz not null default now()
);
create index deal_win_backs_project_idx on deal_win_backs (project_id, reach_out_on);

alter table email_messages drop constraint email_messages_kind_check;
alter table email_messages add constraint email_messages_kind_check
    check (kind in ('initial', 'follow_up', 'auto_reply', 'manual_reply', 'inbound', 'meeting_follow_up', 'deal_nudge', 'win_back'));

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back'));
