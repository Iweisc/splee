-- Website visitors. A project's site loads /api/v/{site_key}.js, which posts one hit per page view to /api/v/{site_key}/hit.
-- domains are the hosts hits may come from, with their subdomains, the project's own domain until someone edits them.
-- intent_paths are the pages that make a company's visit worth an alert, like /pricing, each matching itself and the pages
-- under it. Empty turns the alert off.
create table visitor_sites (
    project_id uuid primary key references projects(id) on delete cascade,
    site_key text not null unique default encode(gen_random_bytes(12), 'hex'),
    domains text[] not null default '{}',
    intent_paths text[] not null default '{}',
    last_hit_at timestamptz,
    created_at timestamptz not null default now()
);

-- The network behind a visitor's IP, by an HMAC of the IP with APP_SECRET, so the IP itself is never stored. Only business
-- networks show on the Visitors tab. Looked up again after 30 days and deleted then.
create table visitor_networks (
    ip_hash text primary key,
    kind text not null check (kind in ('business', 'isp', 'mobile', 'hosting', 'vpn', 'unknown')),
    org text not null default '',
    domain citext,
    source text not null check (source in ('ipinfo', 'rdap', 'none')),
    resolved_at timestamptz not null default now()
);

-- One row per page view, deleted after 180 days. domain and company are set once the IP's network is known to be a
-- business's. path has no query string, and referrer no query string either, since either can carry personal data.
-- source is the first utm_source or the referrer's host when it isn't the site itself, and empty for a direct visit.
create table visitor_hits (
    id bigint generated always as identity primary key,
    project_id uuid not null references projects(id) on delete cascade,
    visitor_id text not null default '',
    ip_hash text not null,
    domain citext,
    company text not null default '',
    path text not null,
    title text not null default '',
    referrer text not null default '',
    source text not null default '',
    utm_source text not null default '',
    utm_medium text not null default '',
    utm_campaign text not null default '',
    utm_term text not null default '',
    utm_content text not null default '',
    created_at timestamptz not null default now()
);
create index visitor_hits_project_idx on visitor_hits (project_id, created_at desc);
create index visitor_hits_company_idx on visitor_hits (project_id, domain, created_at) where domain is not null;
create index visitor_hits_ip_idx on visitor_hits (ip_hash, created_at);
create index visitor_hits_age_idx on visitor_hits (created_at);

-- One row per company and UTC day a hot-visitor alert went out for, so a company alerts once a day.
create table visitor_alerts (
    project_id uuid not null references projects(id) on delete cascade,
    domain citext not null,
    day date not null,
    primary key (project_id, domain, day)
);
