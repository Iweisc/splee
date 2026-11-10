-- A/B tests: a campaign's angles. Each new lead gets one round-robin and keeps it for its first email and follow-ups.
create table segment_variants (
    id uuid primary key default gen_random_uuid(),
    segment_id uuid not null references segments(id) on delete cascade,
    name text not null,
    -- Added to the writer's prompt: what the email leads with, its call to action and its subject style.
    instructions text not null default '',
    position integer not null default 0,
    -- New leads skip a paused variant. Leads that already have it keep it.
    paused_at timestamptz,
    created_at timestamptz not null default now()
);
create index segment_variants_segment_idx on segment_variants (segment_id, position);

alter table leads add column variant_id uuid references segment_variants(id) on delete set null;
alter table email_messages add column variant_id uuid references segment_variants(id) on delete set null;
create index email_messages_variant_idx on email_messages (variant_id) where variant_id is not null;

-- "After N sends per variant, keep the winner". Null is off. The pick is recorded once, when it pauses the others.
alter table segments add column variant_pick_after integer;
alter table segments add column variant_winner_id uuid references segment_variants(id) on delete set null;
alter table segments add column variant_picked_at timestamptz;
alter table segments add column variant_pick_note text not null default '';
