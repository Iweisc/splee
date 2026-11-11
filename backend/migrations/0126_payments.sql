-- Stripe, Paddle and Lemon Squeezy (payments/, routes/payments.rs): a payment from a lead, or from someone at the lead's company,
-- wins their deal and counts as revenue on Analytics. Numbered 0126 because 0124 and 0125 were taken by other branches.

-- One per project and provider. api_key_enc and signing_secret_enc are encrypted with APP_SECRET, and the signing secret is
-- empty until the user pastes it from the provider's webhook settings. webhook_token is the secret in the webhook URL,
-- /api/payments/{provider}/{webhook_token}. backfill_status is the one-time look back over 90 days that connecting queues:
-- backfill_found is the payments the provider listed, backfill_matched those that matched a lead.
create table payment_connections (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    provider text not null check (provider in ('stripe', 'paddle', 'lemonsqueezy')),
    api_key_enc text not null,
    signing_secret_enc text not null default '',
    webhook_token text not null default encode(gen_random_bytes(16), 'hex'),
    backfill_status text not null default 'queued' check (backfill_status in ('queued', 'running', 'done', 'failed')),
    backfill_error text not null default '',
    backfill_found integer not null default 0,
    backfill_matched integer not null default 0,
    backfilled_at timestamptz,
    last_event_at timestamptz,
    created_at timestamptz not null default now(),
    unique (project_id, provider)
);
create unique index payment_connections_token_idx on payment_connections (webhook_token);

-- Webhook deliveries already handled, so a provider sending the same event twice changes nothing.
create table payment_events (
    connection_id uuid not null references payment_connections(id) on delete cascade,
    event_id text not null,
    received_at timestamptz not null default now(),
    primary key (connection_id, event_id)
);

-- Only payments that matched a lead are kept. amount_cents is in minor units of currency, negative for a refund. external_id
-- is the provider's charge, transaction, order or adjustment id. A provider that reports refunds as a running total gets a
-- row for each increase, "refund:{charge or order id}:{total so far}", so the webhook and the backfill never count one
-- twice. The rows stay when the connection is removed, since the money was still paid.
create table payments (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    lead_id uuid references leads(id) on delete set null,
    provider text not null check (provider in ('stripe', 'paddle', 'lemonsqueezy')),
    external_id text not null,
    kind text not null check (kind in ('payment', 'refund')),
    amount_cents bigint not null,
    currency text not null,
    customer_email text not null default '',
    paid_at timestamptz not null,
    created_at timestamptz not null default now(),
    unique (project_id, provider, external_id)
);
create index payments_project_idx on payments (project_id, paid_at);
create index payments_lead_idx on payments (lead_id);

-- A deal won by a payment has the provider as its source.
alter table deals drop constraint deals_source_check;
alter table deals add constraint deals_source_check check (source in ('manual', 'hot_reply', 'booking', 'api', 'stripe', 'paddle', 'lemonsqueezy'));
