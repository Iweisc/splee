-- Digests go out per organization, so a user in two organizations gets one from each. Keyed by user alone,
-- the first one sent each day blocked the other.
alter table notification_log
  add column org_id uuid references organizations(id) on delete cascade,
  drop constraint notification_log_pkey;
-- Old rows don't say which organization they were for. Counting them for all of the user's organizations
-- can't send anyone a second digest today.
insert into notification_log (user_id, org_id, kind, sent_on)
select l.user_id, m.org_id, l.kind, l.sent_on from notification_log l join memberships m on m.user_id = l.user_id;
delete from notification_log where org_id is null;
alter table notification_log
  alter column org_id set not null,
  add primary key (user_id, org_id, kind, sent_on);
