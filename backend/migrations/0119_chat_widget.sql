-- The chat widget a project embeds on its site with one script tag. It uses the visitors feature's site key and domains,
-- so one list of domains covers both scripts. See crate::chat.
create table chat_settings (
    project_id uuid primary key references projects(id) on delete cascade,
    enabled boolean not null default false,
    accent text not null default '#111111' check (accent ~ '^#[0-9a-f]{6}$'),
    greeting text not null default 'Hi! Ask me anything about what we do.',
    position text not null default 'right' check (position in ('right', 'left')),
    -- Path prefixes it shows on, matched like the Visitors tab's high-intent pages. Empty for every page.
    paths text[] not null default '{}',
    faq text not null default '',
    daily_messages int not null default 200 check (daily_messages between 1 and 5000),
    monthly_budget_micros bigint not null default 10000000 check (monthly_budget_micros between 0 and 1000000000),
    created_at timestamptz not null default now()
);

create table chat_conversations (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    -- sha256 of the token the widget keeps in the visitor's localStorage.
    token_hash text not null,
    ip_hash text not null default '',
    -- The visitors snippet's id for this browser, when the site has it.
    visitor_id text not null default '',
    -- The visiting company, from the visitors feature, when its network was known.
    domain citext,
    page text not null default '',
    email citext,
    name text not null default '',
    company text not null default '',
    lead_id uuid references leads(id) on delete set null,
    -- Someone on the team answers instead of the model from then on.
    taken_over_at timestamptz,
    taken_over_by uuid references users(id) on delete set null,
    last_message_at timestamptz not null default now(),
    created_at timestamptz not null default now()
);

create index chat_conversations_project_idx on chat_conversations (project_id, last_message_at desc);

create table chat_messages (
    id bigserial primary key,
    conversation_id uuid not null references chat_conversations(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    -- visitor: typed in the widget. bot: the model's answer, which the caps count. team: someone in Splee. notice: Splee's own
    -- fixed text, like the thanks after an email.
    role text not null check (role in ('visitor', 'bot', 'team', 'notice')),
    body text not null,
    cost_micros bigint not null default 0,
    user_id uuid references users(id) on delete set null,
    created_at timestamptz not null default now()
);

create index chat_messages_conversation_idx on chat_messages (conversation_id, id);
create index chat_messages_project_idx on chat_messages (project_id, created_at);

alter table leads drop constraint leads_source_check;
alter table leads add constraint leads_source_check check (source in ('agent', 'import', 'api', 'form', 'linkedin', 'chat'));

-- What a chat answer in progress may still spend, so concurrent messages can't pass the caps. Each row lives for one model
-- call. One left by a crash stops counting after 10 minutes.
create table chat_reservations (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    micros bigint not null,
    created_at timestamptz not null default now()
);

create index chat_reservations_project_idx on chat_reservations (project_id, created_at);
