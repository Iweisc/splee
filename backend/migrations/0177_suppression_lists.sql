-- Named do-not-contact lists, like "CRM customers" or "Partner agencies". They sit beside the flat suppressions table, which
-- keeps unsubscribes and bounces, so a whole list can cover only some projects and be deleted in one go.
create table suppression_lists (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    name citext not null,
    kind text not null check (kind in ('people', 'companies')),
    -- Null covers every project of the org, including ones made later.
    project_ids uuid[] check (project_ids is null or cardinality(project_ids) > 0),
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now(),
    unique (org_id, name)
);

-- An email on a people list, a domain on a companies list. Lowercased on the way in and matched exactly.
create table suppression_list_entries (
    list_id uuid not null references suppression_lists(id) on delete cascade,
    -- Copied from the list, so the send check finds a value with one index lookup per org.
    org_id uuid not null references organizations(id) on delete cascade,
    value text not null,
    primary key (list_id, value)
);

create index suppression_list_entries_org_value on suppression_list_entries (org_id, value);
