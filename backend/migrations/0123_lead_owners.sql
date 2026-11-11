-- Lead ownership for teams (ownership.rs): each lead can belong to one member, set by hand or by the project's assignment rule
-- when the lead first replies, books or is captured by the website chat.
alter table leads add column owner_id uuid references users(id) on delete set null;
create index leads_owner_idx on leads (project_id, owner_id);
-- When the rule looked at the lead, which it does only once. Leads that already replied or booked count as looked at, so a rule
-- turned on now starts with new leads.
alter table leads add column assign_ran_at timestamptz;
update leads set assign_ran_at = now() where status not in ('new', 'queued', 'contacted');

-- A deal's owner is its lead's from now on, so deals.owner_id is no longer read. Leads take the owner of their open deal, or
-- else of their newest one, while that person is still on the team.
update leads l set owner_id = d.owner_id
from (select distinct on (lead_id) lead_id, owner_id, org_id from deals where owner_id is not null
      order by lead_id, status = 'open' desc, created_at desc) d
where d.lead_id = l.id and l.owner_id is null
  and exists (select 1 from memberships m where m.org_id = d.org_id and m.user_id = d.owner_id);

-- Who a mailbox belongs to, for the "by mailbox" rule. Mailboxes from before have none until an admin picks one.
alter table mailboxes add column owner_id uuid references users(id) on delete set null;

-- away: round robin skips them. owner_filters: the Everyone, Mine or Unassigned each of Inbox, Leads and Pipeline was left on.
alter table memberships
  add column away boolean not null default false,
  add column owner_filters jsonb not null default '{}';

-- assign_members in the order round robin goes through them. assign_last is who got the last lead, so the next goes to the one
-- after. Round robin locks this row, which is what keeps two leads replying at once from going to the same person.
alter table project_settings
  add column assign_rule text not null default 'off' check (assign_rule in ('off', 'round_robin', 'mailbox')),
  add column assign_members uuid[] not null default '{}',
  add column assign_last uuid;

-- Each member's Slack user in the workspace the org added Splee to, from users.lookupByEmail, so a hot reply can mention its
-- owner. slack_user_id is null when Slack had nobody with that address. email is the address looked up, so a changed one is
-- looked up again.
create table slack_user_ids (
    team_id text not null,
    user_id uuid not null references users(id) on delete cascade,
    email citext not null,
    slack_user_id text,
    looked_up_at timestamptz not null default now(),
    primary key (team_id, user_id)
);
