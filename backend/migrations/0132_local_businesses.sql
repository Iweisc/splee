-- Local business search on Google Places (places.rs, local_businesses.rs, routes/local_businesses.rs): dentists, gyms or
-- restaurants by category and place, with the organization's own Google Maps Platform key. Adding one to a campaign reads its
-- website for a contact address (site_contacts.rs).

-- key_enc is encrypted with APP_SECRET. monthly_cap is how many calls to Google a calendar month (UTC) may make, after which
-- searches are refused until the next month.
create table places_keys (
    org_id uuid primary key references organizations(id) on delete cascade,
    key_enc text not null,
    monthly_cap integer not null default 2000 check (monthly_cap >= 0),
    created_at timestamptz not null default now()
);

-- Calls to Google's Text Search per organization and calendar month (UTC). Counted before each call goes out.
create table places_usage (
    org_id uuid not null references organizations(id) on delete cascade,
    month date not null,
    calls integer not null default 0,
    primary key (org_id, month)
);

-- One search of one place, like "dentists" in "Austin, TX", with the place ids Google returned in its order. what and place
-- are lowercased with their spaces collapsed, so the same search typed differently still finds it. Only searches that read
-- every page are kept, and they're used for 30 days.
create table places_searches (
    org_id uuid not null references organizations(id) on delete cascade,
    what text not null,
    place text not null,
    place_ids text[] not null,
    searched_at timestamptz not null default now(),
    primary key (org_id, what, place)
);

-- The places those searches returned, as Google described them then. phone is in E.164 when it could be read, else as Google
-- wrote it. domain is the website's, without www.
create table places (
    org_id uuid not null references organizations(id) on delete cascade,
    place_id text not null,
    name text not null,
    category text not null default '',
    types text[] not null default '{}',
    address text not null default '',
    city text not null default '',
    region text not null default '',
    country_code text not null default '',
    lat double precision,
    lng double precision,
    phone text not null default '',
    website text not null default '',
    domain citext,
    rating real,
    review_count integer not null default 0,
    maps_url text not null default '',
    fetched_at timestamptz not null default now(),
    primary key (org_id, place_id)
);

-- A company added from Google Maps keeps its phone, street address and place id.
alter table companies add column phone text not null default '';
alter table companies add column address text not null default '';
alter table companies add column place_id text not null default '';
create index companies_place_idx on companies (project_id, place_id) where place_id <> '';

alter table leads drop constraint leads_source_check;
alter table leads add constraint leads_source_check check (source in ('agent', 'import', 'api', 'form', 'linkedin', 'chat', 'google_maps'));
