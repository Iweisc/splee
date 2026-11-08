-- Signs hot-lead webhooks so receivers can check a request came from this Splee. Each existing row gets its own value.
alter table project_settings add column webhook_secret text not null default encode(gen_random_bytes(24), 'hex');
