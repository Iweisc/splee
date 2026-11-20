-- Two-factor sign-in (two_factor.rs). secret_enc is the TOTP key in hex, encrypted with APP_SECRET. A row without enabled_at
-- is a setup the member hasn't confirmed with a code yet. last_step is the newest 30-second step a code was taken for, so
-- the same code can't be used twice.
create table two_factor (
    user_id uuid primary key references users(id) on delete cascade,
    secret_enc text not null,
    enabled_at timestamptz,
    last_step bigint not null default 0
);

-- Keyed hashes (crypto::keyed_hash) of the one-time backup codes. Making new ones deletes the old.
create table backup_codes (
    user_id uuid not null references users(id) on delete cascade,
    code_hash text not null,
    used_at timestamptz,
    primary key (user_id, code_hash)
);

-- A correct password for an account with two-factor on, waiting for its code. token_hash is sha256 of the token the login
-- page holds. Used once, and expired rows are pruned on each sign-in.
create table pending_logins (
    token_hash text primary key,
    user_id uuid not null references users(id) on delete cascade,
    expires_at timestamptz not null
);
