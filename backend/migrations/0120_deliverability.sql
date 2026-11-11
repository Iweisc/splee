-- The Deliverability page (deliverability/): blocklist lookups every 6 hours, Google Postmaster Tools numbers, and a health
-- verdict per mailbox that holds an at-risk one from sending until someone resumes it.

-- The latest answer for each domain or IPv4 address an org sends from, on each list. Replaced on every check.
create table blocklist_checks (
    org_id uuid not null references organizations(id) on delete cascade,
    target text not null,
    kind text not null check (kind in ('domain', 'ip')),
    list text not null,
    status text not null check (status in ('clear', 'listed', 'unknown')),
    -- What a listing means, like "Spam domain", or why the list couldn't be asked.
    detail text not null default '',
    checked_at timestamptz not null default now(),
    primary key (org_id, target, list)
);

alter table mailboxes
    -- The addresses Splee checks on the IP blocklists: where another Splee mailbox saw its warmup emails come from, else its
    -- SMTP host's A records.
    add column sending_ips text[] not null default '{}',
    -- The sending server's address from the Received headers of its latest warmup email another Splee mailbox read.
    add column received_ip text not null default '',
    add column received_ip_at timestamptz,
    -- Set while Splee holds an at-risk mailbox (status paused, with why in last_error). Resuming clears it.
    add column health_held_at timestamptz,
    -- The at-risk problems the mailbox was resumed through, so the next check doesn't hold it again for the same ones.
    add column health_ack text[] not null default '{}';

-- One Google Postmaster Tools sign-in per org, through the Google app mailboxes sign in with.
create table postmaster_connections (
    org_id uuid primary key references organizations(id) on delete cascade,
    email text not null,
    refresh_token_enc text not null,
    access_token_enc text not null default '',
    expires_at timestamptz,
    -- The domains verified in Postmaster Tools, from the last sync.
    domains text[] not null default '{}',
    error text not null default '',
    synced_at timestamptz,
    created_at timestamptz not null default now()
);

-- Postmaster Tools' numbers for one domain and day. Google leaves a number out on days with too little Gmail traffic.
create table postmaster_days (
    org_id uuid not null references organizations(id) on delete cascade,
    domain text not null,
    day date not null,
    -- HIGH, MEDIUM, LOW or BAD, or empty when Google gave none.
    reputation text not null default '',
    spam_rate double precision,
    spf_rate double precision,
    dkim_rate double precision,
    dmarc_rate double precision,
    primary key (org_id, domain, day)
);

-- What a Google sign-in is for. Postmaster Tools reuses the mailbox callback, so the operator registers no new redirect URI.
alter table mailbox_oauth_states add column purpose text not null default 'mailbox' check (purpose in ('mailbox', 'postmaster'));
