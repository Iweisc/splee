-- Calls the lead made to the team (calls.rs): to one of the organization's Twilio numbers, rung in the browsers of the team, or
-- taken in Aircall or Dialpad (cloud_phone.rs). They're kept like the calls the team makes, and analytics leaves them out of
-- the calls made.
alter table calls add column inbound boolean not null default false;
-- The calls::tick sweep for calls the lead hung up on while they rang, which Twilio says nothing more about.
create index calls_ringing_idx on calls (called_at) where inbound and call_status = 'ringing';
