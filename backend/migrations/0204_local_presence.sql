-- Local presence (calls::caller_id): the account's numbers that can make calls, read from Twilio when calling is saved with
-- local presence on. Each call goes out from the one in the lead's area code, for the US and Canada, or else in their
-- country, before voice_from. Empty is off.
alter table twilio_accounts add column local_numbers text[] not null default '{}';
