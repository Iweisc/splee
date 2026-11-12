-- Google Sheets, both ways (sheets/, routes/sheets.rs): leads import from a tab, once or every 15 minutes, and hot leads, booked
-- meetings and won deals add a row to another. A project signs in with the Google app mailboxes use, through the same callback,
-- so mailbox_oauth_states learns a third purpose and which project a sign-in is for. Numbered 0133 because 0132 is taken by
-- another branch.

-- Widened, not changed: every value the old check allowed still passes.
alter table mailbox_oauth_states drop constraint mailbox_oauth_states_purpose_check;
alter table mailbox_oauth_states add constraint mailbox_oauth_states_purpose_check check (purpose in ('mailbox', 'postmaster', 'sheets'));
alter table mailbox_oauth_states add column project_id uuid references projects(id) on delete cascade;

-- The Google account a project signed in with for Sheets, with its own tokens, apart from any mailbox's. Tokens are encrypted
-- with APP_SECRET. needs_reconnect is set once Google turns the refresh token down. held_until is set when Google said to slow
-- down, and nothing calls Sheets for the project until then, since the limit is per Google account. version goes up with each
-- sign-in, so a request that used the tokens of an earlier one can't sign out a newer one.
create table sheet_accounts (
    project_id uuid primary key references projects(id) on delete cascade,
    email text not null,
    refresh_token_enc text not null,
    access_token_enc text not null default '',
    expires_at timestamptz,
    needs_reconnect boolean not null default false,
    error text,
    held_until timestamptz,
    version integer not null default 1,
    created_at timestamptz not null default now()
);

-- A tab of a spreadsheet that leads import from (kind 'import') or that lead events are written to (kind 'export'). tab is the
-- tab's title, which is how Sheets addresses it. mapping is {field: column header}, as in a file import. events are the lead
-- events an export writes: hot_lead, meeting_booked and deal.won.
create table sheet_connections (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    kind text not null check (kind in ('import', 'export')),
    spreadsheet_id text not null,
    spreadsheet_name text not null default '',
    tab text not null,
    mapping jsonb not null default '{}',
    segment_id uuid references segments(id) on delete set null,
    keep_importing boolean not null default false,
    next_import_at timestamptz,
    events text[] not null default '{hot_lead,meeting_booked,deal.won}',
    last_sync_at timestamptz,
    rows_added bigint not null default 0,
    error text,
    failed_at timestamptz,
    created_at timestamptz not null default now()
);
create index sheet_connections_project on sheet_connections (project_id);
create index sheet_connections_due on sheet_connections (next_import_at) where kind = 'import' and keep_importing;
-- One export per tab: two would each queue a row for the same event.
create unique index sheet_connections_export_tab on sheet_connections (project_id, spreadsheet_id, tab) where kind = 'export';

-- The rows an import already read, by lowercased email, or by a hash of the row when it has no valid email, so reading the sheet
-- again only adds new rows.
create table sheet_import_rows (
    connection_id uuid not null references sheet_connections(id) on delete cascade,
    key text not null,
    primary key (connection_id, key)
);

-- A row an export writes: added when the event happens, with the cells as they were then, and marked written once Sheets took
-- it. The unique key is what keeps a lead's event from being queued twice. The row in the sheet ends with a Splee ID,
-- {lead_id}:{event}, which the worker reads back before appending, so a row Google took without Splee hearing so is marked
-- written instead of being sent again.
create table sheet_export_rows (
    id uuid primary key default gen_random_uuid(),
    connection_id uuid not null references sheet_connections(id) on delete cascade,
    lead_id uuid not null references leads(id) on delete cascade,
    event text not null,
    cells text[] not null,
    written_at timestamptz,
    -- When it was last sent without Google saying it took it. It waits export::SETTLE_SECS before the tab is read for it.
    sent_at timestamptz,
    created_at timestamptz not null default now(),
    unique (connection_id, lead_id, event)
);
create index sheet_export_rows_pending on sheet_export_rows (connection_id, created_at) where written_at is null;
