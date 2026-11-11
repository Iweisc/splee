-- Phone calls to leads (calls.rs, routes/calls.rs). Numbered 0128 because 0124 to 0127 were taken by other branches.
--
-- A follow-up step with "kind": "call" in project_settings.follow_ups is a call instead of an email. When it comes due it
-- becomes an open call for the lead's owner, or for anyone when the lead has none, and the next step waits until someone
-- logs what happened or skips it. Its delay counts from then, like an email's from when it was sent.
--
-- Calls are made from the browser through the organization's Twilio account: twiml_app_sid is the TwiML App whose Voice URL
-- is /api/twilio/voice, and voice_from the number calls come from, a Twilio number in E.164. Tokens for the browser are
-- signed with the API key twilio_accounts already has. With call_transcription on, Twilio transcribes each call and the
-- agent summarizes it.
alter table twilio_accounts
    add column twiml_app_sid text not null default '',
    add column voice_from text not null default '',
    add column call_transcription boolean not null default false;

-- One row per call step, or per call logged without one ('manual', from the API or MCP). status: open until done or
-- skipped, or cancelled when the lead replies, books or goes on the do-not-contact list (cancel_reason says which).
-- twilio_sid is the browser leg's CallSid, which Twilio's status, recording and transcription callbacks name. A step that
-- couldn't be called at all, like a lead with no number, is stored skipped right away so the sequence goes on.
-- transcript_parts collects Twilio's final transcription segments as they arrive, [{"seq": 3, "who": "Lead", "text": "…"}],
-- and transcript is them in order once Twilio says it stopped.
create table calls (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    lead_id uuid not null references leads(id) on delete cascade,
    segment_id uuid references segments(id) on delete set null,
    kind text not null check (kind in ('sequence', 'manual')),
    step integer not null default 0,
    instructions text not null default '',
    owner_id uuid references users(id) on delete set null,
    status text not null check (status in ('open', 'done', 'skipped', 'cancelled')),
    due_at timestamptz not null default now(),
    closed_at timestamptz,
    cancel_reason text not null default '',
    to_number text not null default '',
    twilio_sid text unique,
    called_by uuid references users(id) on delete set null,
    called_at timestamptz,
    call_status text not null default '',
    duration_secs integer,
    recording_sid text not null default '',
    recording_url text not null default '',
    recording_secs integer,
    outcome text check (outcome in ('connected', 'voicemail', 'no_answer', 'wrong_number', 'not_interested', 'meeting_booked')),
    notes text not null default '',
    transcript_parts jsonb not null default '[]',
    transcript text not null default '',
    transcribed_at timestamptz,
    summary text not null default '',
    next_step text not null default '',
    created_at timestamptz not null default now()
);
create unique index calls_step_idx on calls (lead_id, step) where kind = 'sequence';
create index calls_open_idx on calls (project_id, due_at) where status = 'open';
create index calls_lead_idx on calls (lead_id);
create index calls_closed_idx on calls (project_id, closed_at) where closed_at is not null;

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call'));
