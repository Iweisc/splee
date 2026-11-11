-- A booking page per mailbox at /book/<slug>, for teams without Calendly or Cal.com. See crate::booking.
create table booking_pages (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    mailbox_id uuid not null unique references mailboxes(id) on delete cascade,
    slug text not null unique check (slug ~ '^[a-z0-9][a-z0-9-]{1,47}$'),
    name text not null default 'Intro call',
    duration_minutes int not null default 30 check (duration_minutes in (15, 30, 45, 60)),
    timezone text not null default 'UTC',
    -- Seven entries, Monday first: null for a day off, else [start, end] in minutes after midnight.
    hours jsonb not null default '[[540, 1020], [540, 1020], [540, 1020], [540, 1020], [540, 1020], null, null]'::jsonb,
    buffer_minutes int not null default 0 check (buffer_minutes between 0 and 120),
    notice_minutes int not null default 240 check (notice_minutes between 0 and 20160),
    days_ahead int not null default 14 check (days_ahead between 1 and 90),
    location text not null default '',
    -- The calendar's secret iCal address, encrypted like mailbox passwords. Empty for none.
    calendar_url_enc text not null default '',
    calendar_error text not null default '',
    created_at timestamptz not null default now()
);

create table bookings (
    id uuid primary key default gen_random_uuid(),
    page_id uuid not null references booking_pages(id) on delete cascade,
    starts_at timestamptz not null,
    ends_at timestamptz not null check (ends_at > starts_at),
    name text not null,
    email citext not null,
    note text not null default '',
    -- The guest's time zone, for the times in their emails.
    timezone text not null default 'UTC',
    -- sha256 of the token in the cancel and reschedule links.
    token_hash text not null unique,
    -- Counts the invite's updates, so calendars replace the event instead of adding a second one.
    sequence int not null default 0,
    ip text not null default '',
    cancelled_at timestamptz,
    created_at timestamptz not null default now()
);

-- Two people picking the same time at once: the second insert fails. Overlaps that don't start together are caught
-- under the page's advisory lock in booking::book.
create unique index bookings_page_start_idx on bookings (page_id, starts_at) where cancelled_at is null;
create index bookings_ip_idx on bookings (ip, created_at);
