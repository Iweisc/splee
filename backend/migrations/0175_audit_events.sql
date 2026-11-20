-- The organization's audit log (audit.rs): who changed what, for its admins. A member's actor_user_id or an API key's
-- actor_key_id says who did it. target_label is the target's name when it happened, so the log still reads after it's
-- renamed or removed. details never hold secrets, tokens or email bodies. Rows go after 365 days.
create table audit_events (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    actor_user_id uuid references users(id) on delete set null,
    actor_key_id uuid references api_keys(id) on delete set null,
    action text not null,
    target_type text not null default '',
    target_id uuid,
    target_label text not null default '',
    details jsonb not null default '{}',
    ip text,
    created_at timestamptz not null default now()
);
create index audit_events_org_idx on audit_events (org_id, created_at desc);
