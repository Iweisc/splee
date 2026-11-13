-- Companies like your best customers (lookalikes.rs). The seeds are the companies of the project's won deals, plus the
-- domains in `added`, minus the ones in `removed`. `off` lists the profile parts switched off, by key, like 'tech'.
create table lookalike_settings (
  project_id uuid primary key references projects(id) on delete cascade,
  added text[] not null default '{}',
  removed text[] not null default '{}',
  off text[] not null default '{}',
  updated_at timestamptz not null default now()
);

-- A company that shares only its country or region with the customers can still be a match. These give the ranking each
-- country's and region's best companies in its tie order, so it reads a few hundred rows instead of the whole country.
create index directory_companies_country_rank on directory_companies (country_code, confidence desc, domain);
create index directory_companies_region_rank on directory_companies (country_code, lower(region), confidence desc, domain);
