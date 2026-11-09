-- Answers a user writes once and drops into inbox replies, like pricing or a "send me your calendar" note.
create table saved_replies (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    title text not null check (length(title) between 1 and 80),
    body text not null check (length(body) between 1 and 5000),
    created_at timestamptz not null default now()
);
create index saved_replies_project_idx on saved_replies (project_id);
