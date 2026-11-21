-- One row per lead someone deleted or forgot, with only the ids an integration got the lead with, so GET /v1/erased-leads can
-- tell a CRM or a Zap to delete its copy. Nothing here names the person. campaign_id has no foreign key because the campaign
-- may be deleted later, and the id is still what the integration stored.
create table erased_leads (
    lead_id uuid primary key,
    org_id uuid not null references organizations (id) on delete cascade,
    project_id uuid not null references projects (id) on delete cascade,
    campaign_id uuid,
    forgotten boolean not null,
    erased_at timestamptz not null default now()
);
create index erased_leads_org_erased_at_idx on erased_leads (org_id, erased_at, lead_id);
