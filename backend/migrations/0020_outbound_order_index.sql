-- The emails list shows a project's newest emails first. Without this it sorts every email the project has sent.
create index email_messages_outbound_order_idx on email_messages (project_id, (coalesce(sent_at, scheduled_for, created_at)) desc)
  where direction = 'outbound';
