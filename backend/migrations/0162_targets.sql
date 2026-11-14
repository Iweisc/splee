-- Targets (routes/targets.rs): what each teammate should win in a month, in the organization's currency. Null is no
-- target. The Pipeline page's Targets tab measures this month's wins against it, with the agent's forecast of the rest.
alter table memberships add column monthly_target_cents bigint check (monthly_target_cents > 0);
