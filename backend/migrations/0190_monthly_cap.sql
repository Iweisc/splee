-- The most a project may spend on emails in a calendar month (UTC), from its mailboxes' price per email. Null is no cap.
-- Once the month's sent emails reach it, cold emails wait for the 1st or a higher cap. Replies to leads still go out.
alter table project_settings add column monthly_cap_micros bigint check (monthly_cap_micros between 1000000 and 1000000000000);
