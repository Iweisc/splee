alter table project_settings drop constraint project_settings_finder_check;
alter table project_settings add constraint project_settings_finder_check check (finder in ('', 'hunter', 'findymail', 'prospeo', 'anymailfinder', 'leadmagic'));
