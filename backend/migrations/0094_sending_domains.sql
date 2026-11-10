-- Lookalike sending domains bought through the org's registrar account, with the DNS Splee set on them.

-- One registrar account per org. Porkbun is the only one for now. The key and secret are encrypted with APP_SECRET.
create table registrar_accounts (
  org_id uuid primary key references organizations(id) on delete cascade,
  provider text not null default 'porkbun' check (provider in ('porkbun')),
  -- The Porkbun username the key belongs to, from /ping.
  account text not null default '',
  api_key_enc text not null,
  secret_enc text not null,
  created_at timestamptz not null default now()
);

-- A row is written before the registrar is asked to buy the domain, and the unique (org_id, domain) keeps a second
-- request from buying it again. queued: waiting its turn. registering: the create call went out. uncertain: no clear
-- answer came back, so it may have been bought and is never retried on its own. failed: the registrar said no and
-- charged nothing, so it can be bought again with a new attempt_key, which is the create call's Idempotency-Key.
create table sending_domains (
  id uuid primary key default gen_random_uuid(),
  org_id uuid not null references organizations(id) on delete cascade,
  domain text not null,
  status text not null default 'queued' check (status in ('queued', 'registering', 'registered', 'uncertain', 'failed')),
  price_micros bigint not null default 0,
  attempt_key uuid not null default gen_random_uuid(),
  order_id text not null default '',
  error text not null default '',
  expires_on date,
  registered_at timestamptz,
  -- The mail provider whose MX and SPF Splee sets: google, microsoft, zoho, zoho_eu, migadu or other.
  provider text not null default 'other',
  -- Where the domain's root forwards, usually the main site.
  forward_to text not null default '',
  dmarc_email text not null default '',
  dns_status text not null default '' check (dns_status in ('', 'setting', 'set', 'error')),
  -- [{type, name, content, prio?, ok, error?}] for every record Splee set or tried to, name relative to the domain.
  records jsonb not null default '[]'::jsonb,
  -- The last DNS check, as mailboxes.dns.
  dns jsonb not null default '{}'::jsonb,
  created_at timestamptz not null default now(),
  unique (org_id, domain)
);
