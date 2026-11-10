-- Autopilot keeps a project's working campaigns stocked with written emails, within a monthly Claude budget and a monthly
-- cap on new leads contacted. Off for every existing project. autopilot_auto_send lets its emails go out without approval.
-- autopilot_note is what the worker decided on its last check, and autopilot_alerted_on the month whose budget alert went out.
alter table project_settings
  add column autopilot boolean not null default false,
  add column autopilot_budget_micros bigint not null default 20000000 check (autopilot_budget_micros >= 0),
  add column autopilot_lead_cap integer not null default 300 check (autopilot_lead_cap >= 0),
  add column autopilot_auto_send boolean not null default false,
  add column autopilot_note text not null default '',
  add column autopilot_checked_at timestamptz,
  add column autopilot_alerted_on date;
