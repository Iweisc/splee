-- The least time between two emails a mailbox sends, so cold email goes out spaced rather than in bursts (sender::mailboxes_with_room).
-- 0, the default, keeps sending as fast as the daily limit allows.
alter table mailboxes add column min_gap_minutes integer not null default 0 check (min_gap_minutes between 0 and 60);
