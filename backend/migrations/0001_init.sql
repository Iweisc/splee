create extension if not exists citext;
create extension if not exists pgcrypto;

create table users (
    id uuid primary key default gen_random_uuid(),
    email citext not null unique,
    name text not null default '',
    password_hash text not null,
    created_at timestamptz not null default now()
);

create table organizations (
    id uuid primary key default gen_random_uuid(),
    name text not null,
    referral_code text not null unique,
    referred_by uuid references organizations(id) on delete set null,
    referral_rewarded boolean not null default false,
    created_at timestamptz not null default now()
);

create table memberships (
    org_id uuid not null references organizations(id) on delete cascade,
    user_id uuid not null references users(id) on delete cascade,
    role text not null check (role in ('owner', 'admin', 'member')),
    created_at timestamptz not null default now(),
    primary key (org_id, user_id)
);

-- Session tokens are stored hashed. The raw token only lives in the cookie.
create table sessions (
    token_hash text primary key,
    user_id uuid not null references users(id) on delete cascade,
    org_id uuid not null references organizations(id) on delete cascade,
    expires_at timestamptz not null,
    created_at timestamptz not null default now()
);

create table invites (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    email citext not null,
    role text not null check (role in ('admin', 'member')),
    token text not null unique,
    invited_by uuid references users(id) on delete set null,
    accepted_at timestamptz,
    created_at timestamptz not null default now()
);

create table api_keys (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    name text not null,
    prefix text not null,
    key_hash text not null unique,
    last_used_at timestamptz,
    revoked_at timestamptz,
    created_at timestamptz not null default now()
);

-- Money is stored in micro-dollars (1 USD = 1_000_000). Positive = credit, negative = debit.
create table credit_ledger (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    amount_micros bigint not null,
    kind text not null check (kind in ('signup_bonus', 'topup', 'email_sent', 'referral_bonus', 'adjustment')),
    ref_id uuid,
    note text not null default '',
    created_at timestamptz not null default now()
);
create index credit_ledger_org_idx on credit_ledger (org_id, created_at desc);

create table billing_profiles (
    org_id uuid primary key references organizations(id) on delete cascade,
    company_name text not null default '',
    address_line1 text not null default '',
    address_line2 text not null default '',
    city text not null default '',
    postal_code text not null default '',
    country text not null default '',
    vat_id text not null default '',
    stripe_customer_id text,
    updated_at timestamptz not null default now()
);

create table notification_prefs (
    user_id uuid primary key references users(id) on delete cascade,
    hot_lead_email boolean not null default true,
    daily_digest boolean not null default true,
    low_balance boolean not null default true,
    weekly_report boolean not null default false
);

create table projects (
    id uuid primary key default gen_random_uuid(),
    -- null org_id = anonymous demo project created from the landing page.
    org_id uuid references organizations(id) on delete cascade,
    claim_token text,
    domain citext,
    business_description text,
    name text not null default '',
    status text not null default 'researching' check (status in ('researching', 'ready', 'active', 'paused', 'failed')),
    -- Output of the research step: name, description, employees, city, country_code, industry, products[], value_props[], language
    company jsonb not null default '{}'::jsonb,
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now()
);
create index projects_org_idx on projects (org_id);
create index projects_domain_idx on projects (domain) where org_id is null;

create table project_settings (
    project_id uuid primary key references projects(id) on delete cascade,
    daily_budget_cents integer not null default 500,
    timezone text not null default 'UTC',
    send_days integer[] not null default '{1,2,3,4,5}',
    send_start_hour integer not null default 9,
    send_end_hour integer not null default 17,
    -- [{"delay_days": 3, "enabled": true, "instructions": ""}, ...]
    follow_ups jsonb not null default '[{"delay_days":3,"enabled":true,"instructions":""},{"delay_days":5,"enabled":true,"instructions":""}]'::jsonb,
    booking_link text not null default '',
    auto_reply_mode text not null default 'approve' check (auto_reply_mode in ('off', 'approve', 'auto')),
    auto_reply_instructions text not null default '',
    sender_name text not null default '',
    signature text not null default '',
    tone text not null default 'friendly',
    email_language text not null default 'auto',
    email_instructions text not null default '',
    cta text not null default '',
    max_words integer not null default 90,
    channels jsonb not null default '{"email":true,"linkedin":false}'::jsonb,
    -- Project-wide ICP limits: {"countries":[],"exclude_countries":[],"company_sizes":[],"exclude_keywords":[]}
    targeting jsonb not null default '{}'::jsonb,
    webhook_url text not null default '',
    slack_webhook_url text not null default '',
    paused boolean not null default true
);

create table competitors (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    domain citext not null,
    name text not null default '',
    note text not null default '',
    created_at timestamptz not null default now(),
    unique (project_id, domain)
);

-- Explee calls these "campaigns" in the UI. Each one is a buyer segment with its own targeting.
create table segments (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    description text not null default '',
    icon text not null default 'building',
    est_size bigint not null default 0,
    fit_score integer not null default 0,
    status text not null default 'draft' check (status in ('draft', 'scaling', 'working', 'paused')),
    -- {"industries":[],"roles":[],"company_sizes":[],"countries":[],"keywords":[]}
    targeting jsonb not null default '{}'::jsonb,
    pain_points text not null default '',
    angle text not null default '',
    position integer not null default 0,
    created_at timestamptz not null default now()
);
create index segments_project_idx on segments (project_id);

create table companies (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    segment_id uuid references segments(id) on delete set null,
    domain citext not null,
    name text not null default '',
    description text not null default '',
    city text not null default '',
    country_code text not null default '',
    employees text not null default '',
    fit_score integer not null default 0,
    fit_reason text not null default '',
    source_url text not null default '',
    status text not null default 'new' check (status in ('new', 'people_found', 'no_people', 'excluded')),
    created_at timestamptz not null default now(),
    unique (project_id, domain)
);
create index companies_segment_idx on companies (segment_id);

create table leads (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    segment_id uuid references segments(id) on delete set null,
    company_id uuid references companies(id) on delete set null,
    full_name text not null default '',
    first_name text not null default '',
    last_name text not null default '',
    title text not null default '',
    email citext,
    email_status text not null default 'unknown' check (email_status in ('unknown', 'found', 'guessed', 'verified', 'invalid')),
    linkedin_url text not null default '',
    company_name text not null default '',
    company_domain citext,
    city text not null default '',
    country_code text not null default '',
    source text not null default 'agent' check (source in ('agent', 'import', 'api')),
    status text not null default 'new' check (status in ('new', 'queued', 'contacted', 'replied', 'hot', 'meeting_booked', 'not_interested', 'bounced', 'unsubscribed', 'do_not_contact')),
    -- Research notes the email writer uses: {"hook": "...", "facts": [...]}
    personalization jsonb not null default '{}'::jsonb,
    unsubscribe_token text not null default encode(gen_random_bytes(16), 'hex'),
    last_contacted_at timestamptz,
    created_at timestamptz not null default now(),
    unique (project_id, email)
);
create index leads_project_status_idx on leads (project_id, status);
create index leads_segment_idx on leads (segment_id);

create table mailboxes (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    email citext not null,
    from_name text not null default '',
    smtp_host text not null,
    smtp_port integer not null default 587,
    smtp_security text not null default 'starttls' check (smtp_security in ('tls', 'starttls', 'none')),
    smtp_username text not null,
    smtp_password_enc text not null,
    imap_host text not null default '',
    imap_port integer not null default 993,
    imap_username text not null default '',
    imap_password_enc text not null default '',
    daily_limit integer not null default 30,
    status text not null default 'active' check (status in ('active', 'paused', 'error')),
    last_error text not null default '',
    imap_last_uid bigint not null default 0,
    -- {"spf": {"ok": true, "record": "..."}, "dmarc": {...}, "mx": {...}, "checked_at": "..."}
    dns jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    unique (org_id, email)
);

create table email_messages (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    lead_id uuid not null references leads(id) on delete cascade,
    segment_id uuid references segments(id) on delete set null,
    mailbox_id uuid references mailboxes(id) on delete set null,
    direction text not null check (direction in ('outbound', 'inbound')),
    kind text not null check (kind in ('initial', 'follow_up', 'auto_reply', 'manual_reply', 'inbound')),
    step integer not null default 0,
    subject text not null default '',
    body text not null default '',
    status text not null check (status in ('draft', 'approved', 'scheduled', 'sending', 'sent', 'failed', 'cancelled', 'received')),
    scheduled_for timestamptz,
    sent_at timestamptz,
    message_id text,
    in_reply_to text,
    from_email citext,
    to_email citext,
    -- Inbound only: hot, question, not_interested, ooo, unsubscribe, bounce, other
    classification text,
    classification_reason text not null default '',
    error text not null default '',
    cost_micros bigint not null default 0,
    created_at timestamptz not null default now()
);
create index email_messages_lead_idx on email_messages (lead_id, created_at);
create index email_messages_project_status_idx on email_messages (project_id, status);
create index email_messages_message_id_idx on email_messages (message_id);
create index email_messages_due_idx on email_messages (scheduled_for) where status in ('approved', 'scheduled');

create table linkedin_messages (
    id uuid primary key default gen_random_uuid(),
    lead_id uuid not null references leads(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    step integer not null,
    kind text not null check (kind in ('connect', 'message')),
    body text not null,
    status text not null default 'todo' check (status in ('todo', 'done', 'skipped')),
    created_at timestamptz not null default now()
);

create table materials (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    kind text not null check (kind in ('case_study', 'doc', 'faq', 'link', 'note')),
    title text not null,
    content text not null default '',
    url text not null default '',
    created_at timestamptz not null default now()
);

create table suppressions (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    value citext not null,
    kind text not null check (kind in ('email', 'domain')),
    reason text not null check (reason in ('unsubscribe', 'manual', 'bounce', 'customer')),
    created_at timestamptz not null default now(),
    unique (org_id, value)
);

create table imports (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    segment_id uuid references segments(id) on delete set null,
    filename text not null,
    total integer not null default 0,
    imported integer not null default 0,
    skipped integer not null default 0,
    errors jsonb not null default '[]'::jsonb,
    created_at timestamptz not null default now()
);

create table agent_runs (
    id uuid primary key default gen_random_uuid(),
    project_id uuid references projects(id) on delete cascade,
    org_id uuid references organizations(id) on delete cascade,
    kind text not null check (kind in ('explore', 'research_company', 'find_companies', 'find_people', 'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt')),
    input jsonb not null default '{}'::jsonb,
    status text not null default 'queued' check (status in ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    step integer not null default 0,
    output jsonb not null default '{}'::jsonb,
    error text not null default '',
    cost_usd double precision not null default 0,
    created_at timestamptz not null default now(),
    started_at timestamptz,
    finished_at timestamptz
);
create index agent_runs_queue_idx on agent_runs (created_at) where status = 'queued';
create index agent_runs_project_idx on agent_runs (project_id, created_at desc);

create table agent_events (
    id bigserial primary key,
    run_id uuid not null references agent_runs(id) on delete cascade,
    step integer not null default 0,
    -- phase = step started/finished, log = short progress line, tool = tool call, done, error
    kind text not null check (kind in ('phase', 'log', 'tool', 'done', 'error')),
    message text not null default '',
    data jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);
create index agent_events_run_idx on agent_events (run_id, id);

create table stop_feedback (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    user_id uuid references users(id) on delete set null,
    reason text not null,
    details text not null default '',
    created_at timestamptz not null default now()
);

create table support_conversations (
    id uuid primary key default gen_random_uuid(),
    visitor_token text not null,
    user_id uuid references users(id) on delete set null,
    created_at timestamptz not null default now()
);

create table support_messages (
    id uuid primary key default gen_random_uuid(),
    conversation_id uuid not null references support_conversations(id) on delete cascade,
    role text not null check (role in ('user', 'assistant')),
    content text not null,
    created_at timestamptz not null default now()
);
