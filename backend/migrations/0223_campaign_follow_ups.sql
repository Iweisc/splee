-- A campaign's own follow-ups, in the shape of project_settings.follow_ups: [{delay_days, enabled, instructions, kind?}].
-- Null follows the project's. A campaign brought over from another tool sends its segment_steps instead of either.
alter table segments add column follow_ups jsonb;
