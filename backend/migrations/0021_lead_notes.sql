-- Your team's own notes on a lead. The agent never reads or writes them.
alter table leads add column notes text not null default '';
