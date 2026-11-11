-- Text messages (SMS and WhatsApp) through the organization's own Twilio account (twilio.rs, routes/twilio.rs).
-- Splee calls Twilio with an API key, and checks the X-Twilio-Signature on the webhooks with the auth token, which is what
-- Twilio signs them with. Both secrets are encrypted with APP_SECRET. sms_from is a number in E.164 or a Messaging Service
-- SID (MG...), whatsapp_from a WhatsApp sender number. A WhatsApp text outside the 24-hour window after the lead last wrote
-- can only be an approved template: whatsapp_template_sid is a Content SID (HX...), whatsapp_template_body its text as
-- Twilio returned it, for previews, and whatsapp_template_vars maps its variables to lead fields, like {"1": "first_name"}.
create table twilio_accounts (
    org_id uuid primary key references organizations(id) on delete cascade,
    account_sid text not null unique,
    account_name text not null default '',
    api_key_sid text not null,
    api_secret_enc text not null,
    auth_token_enc text not null,
    sms_from text not null default '',
    whatsapp_from text not null default '',
    whatsapp_template_sid text not null default '',
    whatsapp_template_body text not null default '',
    whatsapp_template_vars jsonb not null default '{}',
    -- The last time Twilio turned Splee away, which holds the org's texts for an hour.
    error text,
    failed_at timestamptz,
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now()
);

-- In E.164, like +447700900123, or '' for none.
alter table leads add column phone text not null default '';
create index leads_phone_idx on leads (phone) where phone <> '';

-- text_steps: [{"channel": "sms" | "whatsapp", "delay_days": 2, "instructions": ""}]. The first is due its
-- delay after the first email was sent, each later one its delay after the text before it.
alter table project_settings
    add column text_steps jsonb not null default '[]',
    add column daily_text_limit integer not null default 50;

-- Texts Splee sends: sequence steps, and replies typed in the inbox ('reply'), which skip the send window and the cap like
-- email replies. Their answers come in as email_messages rows with channel 'sms' or 'whatsapp', so the inbox, the
-- classifier and the stats see them like email replies.
create table text_messages (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    lead_id uuid not null references leads(id) on delete cascade,
    segment_id uuid references segments(id) on delete set null,
    channel text not null check (channel in ('sms', 'whatsapp')),
    kind text not null check (kind in ('sequence', 'reply')),
    step integer not null default 0,
    to_number text not null,
    body text not null default '',
    template_sid text not null default '',
    template_vars jsonb not null default '{}',
    status text not null check (status in ('draft', 'approved', 'sending', 'sent', 'delivered', 'failed', 'cancelled')),
    due_at timestamptz,
    sent_at timestamptz,
    twilio_sid text unique,
    error text not null default '',
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now()
);
create unique index text_messages_step_idx on text_messages (lead_id, step) where kind = 'sequence';
create index text_messages_due_idx on text_messages (due_at) where status = 'approved';
create index text_messages_sent_idx on text_messages (project_id, sent_at) where sent_at is not null;

alter table email_messages
    add column channel text not null default 'email' check (channel in ('email', 'sms', 'whatsapp')),
    -- An inbound text's MessageSid, which makes a webhook Twilio sends twice land once.
    add column twilio_sid text unique;

alter table suppressions drop constraint suppressions_kind_check;
alter table suppressions add constraint suppressions_kind_check check (kind in ('email', 'domain', 'phone'));

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts'));
