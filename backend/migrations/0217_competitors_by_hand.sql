-- Competitors someone added on the Competitors page. Re-running research keeps them as they are (agent/explore.rs). Ones the
-- research found before this are taken as found by it.
alter table competitors add column added_by_hand boolean not null default false;
