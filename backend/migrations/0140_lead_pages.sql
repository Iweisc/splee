-- Lead pages (lead_pages.rs): a short personal page for each lead of a campaign that turns them on, written with the
-- lead's first email and linked from it, at PUBLIC_URL/for/{token}.

alter table segments add column lead_pages boolean not null default false;

-- One page per lead. A rewrite of the first email rewrites the page and keeps its token, so a link already sent still works.
-- points is [{title, body}]. views counts only views that weren't filtered out, and first_viewed_at is the first of those.
create table lead_pages (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    lead_id uuid not null unique references leads(id) on delete cascade,
    token text not null unique,
    headline text not null,
    intro text not null default '',
    points jsonb not null default '[]',
    proof text not null default '',
    views int not null default 0,
    first_viewed_at timestamptz,
    last_viewed_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);
create index lead_pages_project_idx on lead_pages (project_id);

-- Every view the page's script reported, with why it doesn't count when it doesn't: scanner_agent, cloud, security_gateway
-- (tracking::filter_reason) or repeat, a view from the same address within REPEAT_MINUTES of the last.
create table lead_page_views (
    id uuid primary key default gen_random_uuid(),
    page_id uuid not null references lead_pages(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    lead_id uuid not null references leads(id) on delete cascade,
    ip_hash text not null default '',
    user_agent text not null default '',
    filter_reason text,
    created_at timestamptz not null default now()
);
create index lead_page_views_lead_idx on lead_page_views (lead_id, created_at) where filter_reason is null;
create index lead_page_views_project_idx on lead_page_views (project_id, created_at) where filter_reason is null;
