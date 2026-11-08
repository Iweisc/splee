-- Splee no longer has referrals, so new organizations don't need a referral code.
-- The old codes, the ledger and billing profiles stay in place so nothing already recorded is lost.
alter table organizations alter column referral_code drop not null;
