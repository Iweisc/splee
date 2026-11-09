-- A project can post its alerts to several chats at once, like Slack for the team and Pushover for the founder's phone.
create table chat_channels (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    url text not null,
    -- The last failed post, cleared when one goes through.
    error text,
    failed_at timestamptz,
    created_at timestamptz not null default now(),
    unique (project_id, url)
);

insert into chat_channels (project_id, url, error, failed_at)
select project_id, trim(slack_webhook_url), slack_error, slack_failed_at from project_settings where trim(slack_webhook_url) <> '';

-- project_settings.slack_webhook_url, slack_error and slack_failed_at stay for now, unread, so a backend still running the
-- previous release against this database keeps working. Drop them in a later migration.
