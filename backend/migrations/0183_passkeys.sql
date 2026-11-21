-- Passkeys members sign in with (routes/passkeys.rs, webauthn.rs). credential_id is the authenticator's id for the passkey,
-- public_key the SPKI DER the browser gave when it was added, and alg its COSE algorithm, -7 (ES256) or -257 (RS256).
-- sign_count is the authenticator's counter at the last sign-in, 0 for authenticators that don't count. user_verified says
-- whether adding it checked a PIN, fingerprint or face. A password reset leaves passkeys alone.
create table passkeys (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null references users(id) on delete cascade,
    credential_id bytea not null unique,
    public_key bytea not null,
    alg integer not null check (alg in (-7, -257)),
    sign_count bigint not null default 0,
    transports text[] not null default '{}',
    name text not null,
    user_verified boolean not null,
    created_at timestamptz not null default now(),
    last_used_at timestamptz
);
create index passkeys_user_idx on passkeys (user_id);

-- Challenges for adding a passkey, with the member's user_id, and for signing in with one, with none. challenge_hash is
-- sha256 of the base64url challenge. Each works once, for 5 minutes, and expired ones are pruned when one is made.
create table passkey_challenges (
    challenge_hash text primary key,
    user_id uuid references users(id) on delete cascade,
    expires_at timestamptz not null
);
