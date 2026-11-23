-- The most first emails a project sends to one company (company row or website) a day. Null for no limit.
alter table project_settings add column company_daily_limit int check (company_daily_limit between 1 and 50);
