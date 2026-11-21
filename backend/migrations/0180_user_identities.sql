-- Google and Microsoft accounts members sign in with (routes/sso.rs). subject is the provider's id for the account, which
-- stays the same when its address changes. email is what the provider said when the account was linked, for the Account page.
-- A user signed up this way has users.password_hash = auth::NO_PASSWORD until they set one.
create table user_identities (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null references users(id) on delete cascade,
    provider text not null check (provider in ('google', 'microsoft')),
    subject text not null,
    email text not null default '',
    created_at timestamptz not null default now(),
    unique (provider, subject),
    unique (user_id, provider)
);
