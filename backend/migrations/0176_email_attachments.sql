-- Files on emails (mail/attachments.rs): the ones a lead attached to a reply, and the ones someone uploads to send with
-- theirs. An upload has no message_id until a reply takes it, and the hourly cleanup deletes one still unused after a day.
-- stored is false for a lead's file over 25 MB, which is listed by name and size but not kept.
create table email_attachments (
    id uuid primary key default gen_random_uuid(),
    org_id uuid not null references organizations(id) on delete cascade,
    message_id uuid references email_messages(id) on delete cascade,
    filename text not null,
    content_type text not null,
    size_bytes bigint not null,
    inline boolean not null default false,
    stored boolean not null default true,
    created_at timestamptz not null default now()
);
create index email_attachments_message_idx on email_attachments (message_id);
create index email_attachments_unused_idx on email_attachments (created_at) where message_id is null;

-- The bytes, apart from the list above so reading a thread's attachments never loads them.
create table email_attachment_data (
    attachment_id uuid primary key references email_attachments(id) on delete cascade,
    data bytea not null
);

-- The CC addresses of a lead's reply as it came in, or of our email as it goes out.
alter table email_messages add column cc_emails text[] not null default '{}';
