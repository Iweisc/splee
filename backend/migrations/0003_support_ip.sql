-- Support chat is public. The IP lets us cap how many Claude answers one network can request per hour.
alter table support_conversations add column ip text not null default '';
