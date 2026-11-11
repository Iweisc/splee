-- Client reports (reports/): read-only public links to one project's Analytics, for agencies and teams that show a client
-- their results, and the weekly or monthly summary email each link can send.

create table report_links (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    -- sha256 of the token in the link. The token is shown once, when the link is made.
    token_hash text not null unique,
    title text not null default '',
    intro text not null default '',
    -- Whose name and logo the page shows. logo_url replaces the logo, which is the project website's icon by default.
    brand text not null default 'project' check (brand in ('project', 'org')),
    logo_url text not null default '',
    range_preset text not null default 'last_30' check (range_preset in ('last_7', 'last_30', 'last_90', 'month_to_date')),
    show_spend boolean not null default false,
    show_mailboxes boolean not null default false,
    -- The meetings booked and hot replies, by first name and company.
    show_leads boolean not null default false,
    -- weekly emails on Monday morning, monthly on the 1st, in the project's time zone.
    email_schedule text not null default 'none' check (email_schedule in ('none', 'weekly', 'monthly')),
    -- The first day of the last period emailed. Claimed before sending, so two backends never both send one.
    emailed_period date,
    expires_at timestamptz,
    revoked_at timestamptz,
    last_viewed_at timestamptz,
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now()
);

create index report_links_project on report_links (project_id) where revoked_at is null;

create table report_recipients (
    id uuid primary key default gen_random_uuid(),
    link_id uuid not null references report_links(id) on delete cascade,
    email text not null,
    -- Stops this link's emails only.
    unsubscribed_at timestamptz,
    created_at timestamptz not null default now(),
    unique (link_id, email)
);

-- Each recipient's copy of a period's email. The backend sending it holds the row locked until it has, and one with no
-- sent_at goes out at the next check, so a failed send is retried instead of being lost.
create table report_deliveries (
    recipient_id uuid not null references report_recipients(id) on delete cascade,
    period date not null,
    sent_at timestamptz,
    primary key (recipient_id, period)
);
