-- The text a lead gets when they call one of the organization's numbers and nobody answers (calls.rs on_dialed), or '' for
-- none. It goes out from sms_from like a reply, outside the send window and the daily limit, at most once a day per lead.
alter table twilio_accounts add column missed_call_text text not null default '';

alter table text_messages drop constraint text_messages_kind_check;
alter table text_messages add constraint text_messages_kind_check check (kind in ('sequence', 'reply', 'missed_call'));
