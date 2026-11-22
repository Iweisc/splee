-- Tables pointing at leads with no index on lead_id. linkedin_messages is read by lead on every reply (mail::STOP_LINKEDIN)
-- and on the LinkedIn page, and removing leads checks each of these tables once per lead.
create index linkedin_messages_lead_idx on linkedin_messages (lead_id, step);
create index sheet_export_rows_lead_idx on sheet_export_rows (lead_id);
create index campaign_import_rows_lead_idx on campaign_import_rows (lead_id) where lead_id is not null;
create index play_firings_lead_idx on play_firings (lead_id) where lead_id is not null;
create index watched_people_lead_id_idx on watched_people (lead_id) where lead_id is not null;
create index chat_conversations_lead_idx on chat_conversations (lead_id) where lead_id is not null;
create index buying_committees_lead_idx on buying_committees (lead_id) where lead_id is not null;
create index call_coaching_lead_idx on call_coaching (lead_id);
create index feedback_notices_lead_idx on feedback_notices (lead_id);
