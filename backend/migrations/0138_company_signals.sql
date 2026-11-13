-- Funding and company news (signals/): Form D filings from SEC EDGAR and posts from each company's own blog or newsroom feed.
-- Numbered 0138 because 0137 is taken by another branch.

-- A signal at one of the project's company domains. happened_on is the filing's or the post's date. key tells signals apart
-- within a project: "sec:{cik}:{file number}" for a filing, so an amendment (D/A) updates the offering's row instead of adding
-- one, and "feed:{link}" for a post. amount is US dollars, for filings only: the amount sold, else the amount offered.
create table company_signals (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    domain citext not null,
    kind text not null check (kind in ('funding_filing', 'funding_post', 'launch', 'acquisition', 'expansion', 'leadership')),
    source text not null check (source in ('sec', 'feed')),
    key text not null,
    happened_on date not null,
    title text not null default '',
    amount bigint,
    offered bigint,
    industry text not null default '',
    url text not null,
    first_seen timestamptz not null default now(),
    unique (project_id, key)
);
create index company_signals_project_idx on company_signals (project_id, happened_on desc);
create index company_signals_domain_idx on company_signals (project_id, domain, happened_on desc);
-- For the score worker's check for signals found since a project was last scored.
create index company_signals_seen_idx on company_signals (project_id, first_seen);

-- When each source last checked each of the project's domains, which the worker spaces 7 days apart and the News tab shows.
create table company_signal_checks (
    project_id uuid not null references projects(id) on delete cascade,
    domain citext not null,
    source text not null check (source in ('sec', 'feed')),
    updated_at timestamptz not null default now(),
    primary key (project_id, domain, source)
);
create index company_signal_checks_due_idx on company_signal_checks (project_id, source, updated_at);

-- A domain's blog or newsroom feed, found once from its homepage and shared by every project. An empty feed_url means none was
-- found, and it's looked for again after 30 days.
create table company_feeds (
    domain citext primary key,
    feed_url text not null default '',
    found_at timestamptz not null default now()
);

-- Off until the team turns it on from the News tab: a chat alert when a company in a running campaign files a Form D.
alter table project_settings add column funding_alerts boolean not null default false;
