-- Failed sign-ins per email, so password guessing can be slowed down. Rows older than the window are pruned on insert.
create table login_failures (
  email citext not null,
  created_at timestamptz not null default now()
);
create index login_failures_email_created on login_failures (email, created_at);
create index login_failures_created on login_failures (created_at);
