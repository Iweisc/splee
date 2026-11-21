-- The browsers each member turned push notifications on in (push.rs). endpoint is the push service's URL for that browser,
-- and p256dh and auth are the keys its messages are encrypted to, all as the browser's PushSubscription gave them. A browser
-- has one endpoint, so signing in there as someone else moves it to them. user_agent is a label like "Chrome on macOS".
-- failures counts the sends in a row that didn't go through. A push service's 404 or 410 deletes the row instead.
create table push_subscriptions (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null references users(id) on delete cascade,
    endpoint text not null unique,
    p256dh text not null,
    auth text not null,
    user_agent text not null default '',
    created_at timestamptz not null default now(),
    last_success_at timestamptz,
    failures int not null default 0
);
create index push_subscriptions_user_idx on push_subscriptions (user_id);

-- The server's VAPID key pair, made the first time it's needed unless VAPID_PRIVATE_KEY and VAPID_PUBLIC_KEY are set. One row.
-- private_key_enc is the PKCS#8 key encrypted with APP_SECRET.
create table push_vapid_keys (
    id boolean primary key default true check (id),
    private_key_enc text not null,
    created_at timestamptz not null default now()
);

-- When each member last got the push about emails waiting for approval in an organization, which goes out at most hourly.
create table push_approval_digests (
    user_id uuid not null references users(id) on delete cascade,
    org_id uuid not null references organizations(id) on delete cascade,
    sent_at timestamptz not null default now(),
    primary key (user_id, org_id)
);
