-- Which lead events a project's webhook gets. Endpoints set up before this only ever got hot leads, so that stays the default.
alter table project_settings add column webhook_events text[] not null default '{hot_lead}';
