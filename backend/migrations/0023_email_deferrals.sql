-- How many times a receiving server asked us to try an email again later (an SMTP 4xx).
alter table email_messages add column deferrals integer not null default 0;
