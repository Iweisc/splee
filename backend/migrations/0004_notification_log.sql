-- One row per digest actually sent, so a restart or a second tick never sends it twice.
create table notification_log (
    user_id uuid not null references users(id) on delete cascade,
    kind text not null check (kind in ('daily_digest', 'weekly_report')),
    sent_on date not null,
    primary key (user_id, kind, sent_on)
);
