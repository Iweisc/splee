-- Job changes (jobchanges.rs): people who bought, booked a meeting or replied hot are watched on LinkedIn, and a move to a new
-- company becomes a lead there. Numbered 0134 because 0133 is taken by another branch.

-- relationship is what they did with the project: won (bought), booked (a meeting) or hot (a hot reply). lead_id is the lead
-- they were, or null for someone added by hand or from a CSV. The company and title are the ones last seen, from the lead at
-- first and from LinkedIn after each check. linkedin_key is the lowercased public id from linkedin_url, so two links to one
-- profile are one person. moves counts the moves recorded for them, which numbers the next one. pending_profile is a profile
-- read whose move waits on the new company's page, kept so finishing it doesn't read the profile again.
create table watched_people (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    lead_id uuid references leads(id) on delete set null,
    relationship text not null check (relationship in ('won', 'booked', 'hot')),
    full_name text not null default '',
    email citext,
    linkedin_url text not null default '',
    linkedin_key text not null default '',
    company_name text not null default '',
    company_domain citext,
    company_linkedin_id text not null default '',
    title text not null default '',
    moves integer not null default 0,
    pending_profile jsonb,
    checked_at timestamptz,
    check_error text,
    created_at timestamptz not null default now()
);
create unique index watched_people_lead_idx on watched_people (project_id, lead_id) where lead_id is not null;
create unique index watched_people_email_idx on watched_people (project_id, email) where email is not null;
create unique index watched_people_linkedin_idx on watched_people (project_id, linkedin_key) where linkedin_key <> '';
create index watched_people_due_idx on watched_people (project_id, checked_at nulls first);

-- A move seen on LinkedIn. move_number is the person's moves after it, so each move is kept once however many backends see it,
-- while going back to an earlier company is a new move. new_lead_id is the lead Splee made or found there.
create table job_changes (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    watched_id uuid not null references watched_people(id) on delete cascade,
    old_company text not null default '',
    old_domain citext,
    new_company text not null,
    new_domain citext,
    move_number integer not null,
    new_title text not null default '',
    new_lead_id uuid references leads(id) on delete set null,
    segment_id uuid references segments(id) on delete set null,
    seen_at timestamptz not null default now(),
    dismissed_at timestamptz,
    unique (watched_id, move_number)
);
create index job_changes_project_idx on job_changes (project_id, seen_at desc);
create index job_changes_lead_idx on job_changes (new_lead_id) where new_lead_id is not null;

-- The campaign new leads from job changes join on their own. Null leaves them out of campaigns until someone adds them.
alter table project_settings add column job_change_segment_id uuid references segments(id) on delete set null;

-- LinkedIn profile and company pages read per Unipile account each day (UTC), which jobchanges::DAILY_READS caps.
create table linkedin_profile_reads (
    account_id text not null,
    day date not null,
    reads integer not null default 0,
    primary key (account_id, day)
);

-- Everyone who already bought, booked or replied hot, the strongest first. jobchanges::enroll keeps this up after.
with candidates as (
    select d.lead_id as id, 1 as rank from deals d where d.status = 'won'
    union all select l.id, 2 from leads l where l.status = 'meeting_booked' or l.meeting_at is not null
    union all select l.id, 3 from leads l where l.status = 'hot'
    union all select e.lead_id, 3 from email_messages e where e.status = 'received' and e.classification = 'hot'
), best as (
    select id, min(rank) as rank from candidates where id is not null group by id
)
insert into watched_people (project_id, lead_id, relationship, full_name, email, linkedin_url, linkedin_key, company_name, company_domain, title)
select l.project_id, l.id, case b.rank when 1 then 'won' when 2 then 'booked' else 'hot' end, l.full_name, l.email, l.linkedin_url,
       coalesce(lower(substring(l.linkedin_url from 'linkedin\.com/in/([^/?#]+)')), ''), l.company_name, l.company_domain, l.title
from best b join leads l on l.id = b.id join projects p on p.id = l.project_id and p.org_id is not null
on conflict do nothing;
