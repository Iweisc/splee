-- A campaign's own cap on the first emails and follow-ups it sends a day, on top of the project's daily_email_limit that
-- every campaign shares. Null leaves it to the project's limit.

alter table segments add column daily_limit int check (daily_limit between 1 and 5000);
