-- The company database: one row per company website, imported from Overture Maps' places with `splee import-companies`.
-- It's shared by every organization, like a public dataset, and holds no customer data.
create table directory_companies (
    domain text primary key,
    name text not null,
    website text not null,
    phone text,
    email text,
    address text,
    city text,
    region text,
    postcode text,
    country_code text not null,
    category text,
    categories text[] not null default '{}',
    confidence real not null default 0,
    source_id text not null,
    release text not null,
    -- Categories are snake_case, like italian_restaurant, and the parser splits them into words, so "restaurant" finds them.
    search tsvector generated always as (to_tsvector('simple'::regconfig, name || ' ' || coalesce(category, ''))) stored,
    updated_at timestamptz not null default now()
);

create index directory_companies_place on directory_companies (country_code, lower(city));
create index directory_companies_category on directory_companies (country_code, category);
create index directory_companies_categories on directory_companies using gin (categories);
create index directory_companies_search on directory_companies using gin (search);
