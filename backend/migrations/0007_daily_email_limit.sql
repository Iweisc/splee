-- Splee is free now, so a project's daily cap counts emails instead of dollars.
-- Every email cost $0.04, so a budget of N cents allowed N / 4 emails a day. This keeps each project's pace.
alter table project_settings add column daily_email_limit integer not null default 125 check (daily_email_limit between 0 and 5000);
update project_settings set daily_email_limit = least(5000, daily_budget_cents / 4);
alter table project_settings drop column daily_budget_cents;

-- Spend is now what a project really costs: Claude usage from agent_runs plus what the mailbox's provider charges per email.
-- Past sends recorded Splee's old $0.04 credit price here. That charge is still in credit_ledger, so these become unknown, which is 0.
alter table mailboxes add column cost_per_email_micros bigint not null default 0 check (cost_per_email_micros between 0 and 1000000);
update email_messages set cost_micros = 0 where cost_micros <> 0;
