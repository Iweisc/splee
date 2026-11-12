-- Open and click tracking for campaign emails. See tracking.rs.

alter table segments add column track_clicks boolean not null default true, add column track_opens boolean not null default false;

-- What the sender did for this email, since the campaign's settings can change after it went out. open_token names its
-- pixel, and is set only when it has one.
alter table email_messages add column track_clicks boolean not null default false, add column track_opens boolean not null default false,
  add column open_token text;
create unique index email_messages_open_token_idx on email_messages (open_token) where open_token is not null;

-- Each rewritten link of a sent email. The redirect only ever goes to url, never to anything in the request.
create table tracked_links (
  token text primary key,
  email_id uuid not null references email_messages(id) on delete cascade,
  position integer not null,
  url text not null,
  created_at timestamptz not null default now(),
  unique (email_id, position)
);

-- Every open and click, including the ones filter_reason says were a scanner or a bot, so the filtering can be checked.
-- Only rows without a filter_reason count.
create table tracking_hits (
  id uuid primary key default gen_random_uuid(),
  project_id uuid not null references projects(id) on delete cascade,
  email_id uuid not null references email_messages(id) on delete cascade,
  lead_id uuid not null references leads(id) on delete cascade,
  kind text not null check (kind in ('open', 'click')),
  link_token text references tracked_links(token) on delete cascade,
  filter_reason text,
  ip_hash text not null default '',
  user_agent text not null default '',
  created_at timestamptz not null default now()
);
create index tracking_hits_email_idx on tracking_hits (email_id, kind, created_at);
create index tracking_hits_project_idx on tracking_hits (project_id, created_at);
create index tracking_hits_lead_idx on tracking_hits (lead_id, created_at);

-- A sending domain's own tracking host, like track.acme.com, a CNAME to the Splee host. Emails from the domain's mailboxes
-- use it once a DNS check has found it.
create table tracking_domains (
  id uuid primary key default gen_random_uuid(),
  org_id uuid not null references organizations(id) on delete cascade,
  domain text not null,
  host text not null,
  verified_at timestamptz,
  checked_at timestamptz,
  error text not null default '',
  created_at timestamptz not null default now(),
  unique (org_id, domain)
);
create index tracking_domains_host_idx on tracking_domains (host) where verified_at is not null;
