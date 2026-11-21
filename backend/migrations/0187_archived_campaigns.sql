-- When a campaign was archived, or null for one that isn't (routes/segments.rs archive). Archiving pauses it and drops it from
-- the Campaigns list, the pickers that add leads, autopilot, the review window and the sender. Its leads, emails and stats stay.
alter table segments add column archived_at timestamptz;
