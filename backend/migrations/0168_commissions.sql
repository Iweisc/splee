-- Commissions (routes/commissions.rs): what each teammate earns on the deals they won in a month, in basis points of the
-- won value. Won value above their monthly target earns accelerator_pct of the rate. Null commission_bps on a membership
-- uses the organization's, and null there is no commission plan.
alter table organizations add column commission_bps int check (commission_bps between 0 and 10000);
alter table organizations add column accelerator_pct int not null default 100 check (accelerator_pct between 100 and 500);
alter table memberships add column commission_bps int check (commission_bps between 0 and 10000);
