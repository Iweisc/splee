-- Research questions (agent/research.rs): questions the team asks about every company, like "Do they sell to hospitals?",
-- which the agent answers from each company's website and the web. Each question is a column on the Companies page.

-- answer_type yes_no gets "yes" or "no", text a short answer. use_in_emails puts the answers in the writer's lead briefs.
create table research_questions (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    label text not null,
    question text not null,
    answer_type text not null default 'yes_no' check (answer_type in ('yes_no', 'text')),
    use_in_emails boolean not null default false,
    position int not null default 0,
    created_at timestamptz not null default now()
);
create index research_questions_project_idx on research_questions (project_id, position, created_at);

-- One answer per question and company domain, so a company in two campaigns is researched once. A queued row whose run
-- stopped without answering it reads as failed (research::ANSWER_STATUS), so a cancelled run never leaves a spinner.
create table research_answers (
    question_id uuid not null references research_questions(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    domain citext not null,
    status text not null check (status in ('queued', 'answered', 'unknown', 'failed')),
    answer text not null default '',
    evidence text not null default '',
    source_url text not null default '',
    run_id uuid references agent_runs(id) on delete set null,
    updated_at timestamptz not null default now(),
    primary key (question_id, domain)
);
create index research_answers_domain_idx on research_answers (project_id, domain);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies'));
