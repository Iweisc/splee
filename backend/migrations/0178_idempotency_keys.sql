-- The public API's Idempotency-Key: the first request with a key claims it here before it runs, and its response is kept for a
-- day so a retry gets the same answer without running again. A null status means the first request is still running.
create table idempotency_keys (
    org_id uuid not null references organizations(id) on delete cascade,
    key text not null,
    method text not null,
    path text not null,
    -- Hex SHA-256 of the request body, so the same key with a different body is refused.
    body_hash text not null,
    -- Which request holds the key, so a late release from one request can't delete a retry's claim.
    token uuid not null,
    status int,
    content_type text,
    response_body bytea,
    created_at timestamptz not null default now(),
    expires_at timestamptz not null,
    primary key (org_id, key)
);

create index idempotency_keys_expires_at on idempotency_keys (expires_at);
