-- Campaign stats and the optimizer count and sum each campaign's sent emails. Without this they scan every email.
create index email_messages_segment_sent_idx on email_messages (segment_id) include (cost_micros)
  where direction = 'outbound' and status = 'sent';
