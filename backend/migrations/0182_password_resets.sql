-- The links "Forgot your password?" emails (routes/auth.rs). token_hash is sha256 of the token in the link. A link works
-- for 30 minutes and once, and a reset uses up the user's other open links too. Links a day past expiry are pruned.
create table password_reset_tokens (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null references users(id) on delete cascade,
    token_hash text not null unique,
    created_at timestamptz not null default now(),
    expires_at timestamptz not null,
    used_at timestamptz
);
create index password_reset_tokens_user_idx on password_reset_tokens (user_id);

-- The last hour's reset requests that were let through, known emails or not, so the form can't flood an inbox: at most 3
-- per email and 10 per client address. Older rows are pruned on each request.
create table password_reset_requests (
    email citext not null,
    ip text not null,
    created_at timestamptz not null default now()
);
create index password_reset_requests_created on password_reset_requests (created_at);
