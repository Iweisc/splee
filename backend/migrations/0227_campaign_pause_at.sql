-- When a campaign pauses itself (routes/segments.rs pause_scheduled), and the member or API key that set it, whom the audit
-- log names for the pause. Pausing or archiving it clears all three.
alter table segments
    add column pause_at timestamptz,
    add column pause_by_user uuid references users(id) on delete set null,
    add column pause_by_key uuid references api_keys(id) on delete set null;
create index segments_pause_at on segments (pause_at) where pause_at is not null;
