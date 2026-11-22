-- tl;dv as a fifth meeting recorder (meetings/tldv.rs). Like Gong it's polled with an API key, since its webhooks aren't signed.
alter table meeting_connections drop constraint meeting_connections_provider_check;
alter table meeting_connections add constraint meeting_connections_provider_check check (provider in ('fireflies', 'fathom', 'gong', 'zoom', 'tldv'));
alter table meetings drop constraint meetings_provider_check;
alter table meetings add constraint meetings_provider_check check (provider in ('fireflies', 'fathom', 'gong', 'zoom', 'tldv'));
