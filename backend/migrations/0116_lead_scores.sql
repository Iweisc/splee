-- Lead scores (score.rs): 0-100, the sum of fit (0-50, how well the company matches the targeting) and intent (0-50, what
-- they did lately). score_reasons is a short list of {"text", "kind"} with kind 'fit', 'intent' or 'against', the
-- strongest first. scored_at is null until the score worker first reaches the row.
alter table leads
    add column score smallint not null default 0,
    add column score_fit smallint not null default 0,
    add column score_intent smallint not null default 0,
    add column score_reasons jsonb not null default '[]',
    add column scored_at timestamptz;

-- A company's is its best lead's, or its own fit and intent while it has no leads.
alter table companies
    add column score smallint not null default 0,
    add column score_fit smallint not null default 0,
    add column score_intent smallint not null default 0,
    add column score_reasons jsonb not null default '[]',
    add column scored_at timestamptz;

-- score_signals turns signals off by name, like {"visit": false}. A signal missing from it is on. scored_at is when the
-- worker last scored the whole project, and score_inputs_at when a setting the score reads last changed.
alter table project_settings
    add column score_signals jsonb not null default '{}',
    add column scored_at timestamptz,
    add column score_inputs_at timestamptz;

create index leads_score_idx on leads (project_id, score desc, created_at desc);
create index leads_score_intent_idx on leads (project_id, score_intent desc) where score_intent > 0;
-- The worker looks for leads added since it last scored a project, and the lists sort by age.
create index leads_project_created_idx on leads (project_id, created_at);
create index companies_score_idx on companies (project_id, score desc, created_at desc);
