-- Research runs that were cancelled or failed after step 1 used to leave their project in `researching` for good.
-- Same rule as agent::settle_research: `ready` when step 1 saved a profile, else `failed`.
update projects p
set status = case when p.company = '{}'::jsonb then 'failed' else 'ready' end
where p.status = 'researching'
  and not exists (
    select 1 from agent_runs r where r.project_id = p.id and r.kind = 'explore' and r.status in ('queued', 'running')
  );
