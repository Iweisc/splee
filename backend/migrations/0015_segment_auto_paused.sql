-- Set when the optimizer pauses a campaign for costing too much per hot lead. It only does that once, so a campaign
-- you resume afterwards keeps running instead of being paused again on the next hourly pass.
alter table segments add column auto_paused_at timestamptz;
