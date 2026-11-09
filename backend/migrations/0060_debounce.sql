alter table project_settings drop constraint project_settings_verifier_check;
alter table project_settings add constraint project_settings_verifier_check
  check (verifier in ('', 'neverbounce', 'millionverifier', 'hunter', 'kickbox', 'bouncer', 'emailable', 'clearout', 'reoon', 'debounce'));
