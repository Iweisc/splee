-- A review window (review_window.rs): with review_window_hours set, a first email or follow-up draft nobody edited is approved
-- that many hours after it was written and goes out like any other. Null, the default, keeps every draft waiting for a person.
alter table project_settings add column review_window_hours integer check (review_window_hours between 1 and 72);

-- Who approved an email: 'person' (in the app, from Slack or with an API key) or 'review_window'. Null for one that never waited,
-- like an email written straight into a sending campaign's queue. edited_at is the last time someone changed its subject or body,
-- which keeps a draft out of the review window. written_at is when the agent last rewrote it, so a regenerated draft gets a full
-- window again. Null means created_at.
alter table email_messages
  add column approved_via text check (approved_via in ('person', 'review_window')),
  add column edited_at timestamptz,
  add column written_at timestamptz;
