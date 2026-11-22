-- Territories (ownership.rs). With assign_rule 'territory', a lead goes to whoever covers its country: the lead's own, else
-- its company's, else its phone number's. assign_territories is [{"owner": user id, "countries": ["US", "CA"]}], an entry
-- per owner and a country in one entry at most. A lead from anywhere else, or whose territory's owner is away or left the
-- team, takes its turn among assign_members as round robin gives it.
alter table project_settings add column assign_territories jsonb not null default '[]';
alter table project_settings drop constraint project_settings_assign_rule_check;
alter table project_settings add constraint project_settings_assign_rule_check check (assign_rule in ('off', 'round_robin', 'mailbox', 'territory'));
