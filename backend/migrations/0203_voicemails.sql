-- Voicemail drop (routes/voicemail.rs): each team member records the voicemail they leave, once per organization, and a call
-- from the Calls page that reaches the lead's voicemail can play it to them. Twilio fetches the audio from a link with
-- `token` in it, and recording it again makes a new token.
create table voicemails (
  org_id uuid not null references organizations(id) on delete cascade,
  user_id uuid not null references users(id) on delete cascade,
  audio bytea not null,
  secs int not null,
  token text not null unique,
  created_at timestamptz not null default now(),
  primary key (org_id, user_id)
);
