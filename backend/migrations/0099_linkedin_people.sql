-- LinkedIn people search pages run per Unipile account each day (UTC), which unipile::DAILY_SEARCH_PAGES caps.
create table linkedin_searches (
    account_id text not null,
    day date not null,
    pages integer not null default 0,
    primary key (account_id, day)
);

-- Leads found through LinkedIn people search, which come without an email.
alter table leads drop constraint leads_source_check;
alter table leads add constraint leads_source_check check (source in ('agent', 'import', 'api', 'form', 'linkedin'));
