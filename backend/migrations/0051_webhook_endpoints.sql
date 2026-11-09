-- A project can send its lead events to several endpoints at once, like a CRM sync and a Zapier zap. They share the
-- project's signing secret (webhook_secret) and events (webhook_events).
create table webhook_endpoints (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    url text not null,
    -- The last failed delivery, cleared when one goes through.
    error text,
    failed_at timestamptz,
    created_at timestamptz not null default now(),
    unique (project_id, url)
);

insert into webhook_endpoints (project_id, url, error, failed_at)
select project_id, trim(webhook_url), webhook_error, webhook_failed_at from project_settings where trim(webhook_url) <> '';

-- Where each delivery went, so a resend goes back to the same endpoint. Older ones went to the URL saved then.
alter table webhook_deliveries add column url text not null default '';
update webhook_deliveries d set url = trim(s.webhook_url) from project_settings s where s.project_id = d.project_id;

-- project_settings.webhook_url, webhook_error and webhook_failed_at stay for now, unread, so a backend still running the
-- previous release against this database keeps working. Drop them in a later migration.
