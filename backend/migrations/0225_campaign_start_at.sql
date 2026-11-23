-- When a campaign starts itself (routes/segments.rs start_scheduled), and the member or API key that set it, whom the audit
-- log names for the start. Starting or archiving it clears all three.
alter table segments
    add column start_at timestamptz,
    add column start_by_user uuid references users(id) on delete set null,
    add column start_by_key uuid references api_keys(id) on delete set null;
create index segments_start_at on segments (start_at) where start_at is not null;
