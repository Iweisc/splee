-- Named lists of company domains, like a CRM's accounts or a competitor's clients, that company database searches leave out
-- (routes/dedup_lists.rs). They only hide search results. Unlike suppression_lists they never stop an email or an import.
create table dedup_lists (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    name citext not null,
    -- Null covers every project of the org, including ones made later.
    project_ids uuid[] check (project_ids is null or cardinality(project_ids) > 0),
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now(),
    unique (org_id, name)
);

-- Lowercased without www. on the way in, like directory_companies.domain, and matched exactly.
create table dedup_list_entries (
    list_id uuid not null references dedup_lists(id) on delete cascade,
    -- Copied from the list, so the search finds a domain with one index lookup per org.
    org_id uuid not null references organizations(id) on delete cascade,
    domain text not null,
    primary key (list_id, domain)
);

create index dedup_list_entries_org_domain on dedup_list_entries (org_id, domain);
