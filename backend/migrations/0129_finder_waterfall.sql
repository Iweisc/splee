-- The email waterfall's finders, as a list the project orders (waterfall.rs, routes/finders.rs), instead of the single finder
-- in project_settings.finder. Numbered 0129 because 0124 to 0128 are taken or reserved by other branches.

-- key_enc is encrypted with APP_SECRET. Snov.io's is "{client_id}:{client_secret}". paused_until is set when the provider
-- said it's out of credits or past its rate limit, and the waterfall skips it until then.
create table project_finders (
    project_id uuid not null references projects(id) on delete cascade,
    provider text not null check (provider in ('hunter', 'findymail', 'prospeo', 'anymailfinder', 'leadmagic', 'apollo', 'voilanorbert',
        'skrapp', 'lusha', 'peopledatalabs', 'rocketreach', 'snov', 'dropcontact', 'contactout', 'kaspr')),
    key_enc text not null,
    position integer not null,
    error text,
    failed_at timestamptz,
    paused_until timestamptz,
    created_at timestamptz not null default now(),
    primary key (project_id, provider)
);

-- The old single finder becomes the first of the list. Its columns stay, unused, so the migration only adds.
insert into project_finders (project_id, provider, key_enc, position, error, failed_at)
select project_id, finder, finder_key_enc, 0, finder_error, finder_failed_at from project_settings where finder <> '' and finder_key_enc <> '';

-- "Find phone numbers too": finders that return mobile numbers fill an empty phone. Off by default, since it costs more credits.
alter table project_settings add column find_phones boolean not null default false;

-- The finder that found the lead's phone, like 'RocketReach', or '' when it came some other way.
alter table leads add column phone_source text not null default '';
