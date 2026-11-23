-- When a lead at a company replies, the sequence stops for the project's other leads at that company too: their unsent emails,
-- LinkedIn steps, texts and calls are cancelled and no new ones are written (agent/replies.rs, mail::COMPANY_REPLIED). Off
-- until a project turns it on.
alter table project_settings add column stop_company_on_reply boolean not null default false;
