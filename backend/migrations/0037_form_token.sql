-- The secret in a project's form webhook URL, which adds whoever fills in a website form as a lead.
alter table project_settings add column form_token text not null default encode(gen_random_bytes(16), 'hex');
create unique index project_settings_form_token_idx on project_settings (form_token);
alter table leads drop constraint leads_source_check;
alter table leads add constraint leads_source_check check (source in ('agent', 'import', 'api', 'form'));
