//! Autopilot: keeps each working campaign stocked with written emails for the next few days, one agent run at a time,
//! within the month's Claude budget and cap on new leads contacted. Poor campaigns are left to the hourly optimizer,
//! which pauses them, and a paused campaign gets no more runs.

use chrono::{Datelike, TimeZone, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::agent::{self, kind};
use crate::hooks::ChatAlert;
use crate::models::Ts;
use crate::state::AppState;

/// How many days of sending each campaign keeps written ahead.
const DAYS_AHEAD: i64 = 3;
/// A campaign whose autopilot run failed waits this long before autopilot tries it again, so a broken setup doesn't
/// start a run every tick.
const RETRY_AFTER_HOURS: i32 = 6;

/// Every 10 minutes, so the three stages of a top-up follow each other without waiting an hour apiece.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    let projects: Vec<Uuid> = sqlx::query_scalar(
        "select s.project_id from project_settings s join projects p on p.id = s.project_id where s.autopilot and p.org_id is not null",
    )
    .fetch_all(&state.db)
    .await?;
    for project_id in projects {
        if let Err(e) = check(state, project_id, Utc::now()).await {
            tracing::error!(%project_id, error = ?e, "autopilot failed");
        }
    }
    Ok(())
}

pub fn month_start(now: Ts) -> Ts {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0).single().expect("the first of a month at midnight UTC exists")
}

fn next_month(now: Ts) -> Ts {
    let (year, month) = if now.month() == 12 { (now.year() + 1, 1) } else { (now.year(), now.month() + 1) };
    Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0).single().expect("the first of a month at midnight UTC exists")
}

/// What Claude cost the project since `since`, in micro-dollars, counting the runs people started by hand too.
pub async fn spent_since(db: &PgPool, project_id: Uuid, since: Ts) -> sqlx::Result<i64> {
    sqlx::query_scalar("select (coalesce(sum(cost_usd), 0) * 1000000)::bigint from agent_runs where project_id = $1 and created_at >= $2")
        .bind(project_id)
        .bind(since)
        .fetch_one(db)
        .await
}

/// Leads whose first email went out since `since`.
pub async fn contacted_since(db: &PgPool, project_id: Uuid, since: Ts) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "select count(distinct lead_id) from email_messages
         where project_id = $1 and direction = 'outbound' and kind = 'initial' and status = 'sent' and sent_at >= $2",
    )
    .bind(project_id)
    .bind(since)
    .fetch_one(db)
    .await
}

/// What a run of this kind will likely cost the project: the average of its last 5 that cost anything, else of the last
/// 20 across Splee, else a guess for a project that has never run one.
/// What the project's queued and running runs, autopilot's or the user's, will likely spend on top of what they have so far.
async fn still_to_spend(db: &PgPool, project_id: Uuid) -> sqlx::Result<i64> {
    let going: Vec<(String, f64)> =
        sqlx::query_as("select kind, cost_usd::float8 from agent_runs where project_id = $1 and status in ('queued', 'running')").bind(project_id).fetch_all(db).await?;
    let mut total = 0;
    for (kind, so_far) in going {
        total += (estimate_cost(db, project_id, &kind).await? - (so_far * 1_000_000.0) as i64).max(0);
    }
    Ok(total)
}

pub async fn estimate_cost(db: &PgPool, project_id: Uuid, run_kind: &str) -> sqlx::Result<i64> {
    let usd: Option<f64> = sqlx::query_scalar(
        "select coalesce(
           (select avg(cost_usd) from (select cost_usd from agent_runs where project_id = $1 and kind = $2 and status = 'succeeded'
                                        and cost_usd > 0 order by created_at desc limit 5) r),
           (select avg(cost_usd) from (select cost_usd from agent_runs where kind = $2 and status = 'succeeded' and cost_usd > 0
                                        order by created_at desc limit 20) r))",
    )
    .bind(project_id)
    .bind(run_kind)
    .fetch_one(db)
    .await?;
    let guess = match run_kind {
        kind::FIND_COMPANIES => 0.5,
        kind::FIND_PEOPLE => 1.0,
        _ => 0.3,
    };
    Ok((usd.unwrap_or(guess) * 1_000_000.0).round() as i64)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    WriteEmails,
    FindPeople,
    FindCompanies(i64),
}

impl Step {
    fn kind(&self) -> &'static str {
        match self {
            Step::WriteEmails => kind::WRITE_EMAILS,
            Step::FindPeople => kind::FIND_PEOPLE,
            Step::FindCompanies(_) => kind::FIND_COMPANIES,
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct CampaignQueue {
    pub id: Uuid,
    pub name: String,
    /// Leads with a first email written and not sent yet, drafts waiting for approval included.
    pub queued: i64,
    /// Leads with an address and no first email.
    pub unwritten: i64,
    /// Companies not searched for people yet.
    pub new_companies: i64,
    /// A run for this campaign is queued or going, whoever started it.
    pub busy: bool,
    pub failed_lately: bool,
}

/// What each campaign should hold, when `campaigns` working campaigns share the daily limit and the leads left this month.
pub fn need(daily_limit: i64, leads_left: i64, campaigns: usize) -> i64 {
    let n = campaigns.max(1) as i64;
    let per_campaign = |total: i64| (total + n - 1) / n;
    per_campaign(daily_limit * DAYS_AHEAD).min(per_campaign(leads_left)).max(1)
}

/// The next stage for a campaign short of `need`: write what it has, else find people at the companies it has, else find
/// companies, about one per missing lead, since small companies often have one findable person.
pub fn next_step(c: &CampaignQueue, need: i64) -> Option<Step> {
    if c.queued >= need {
        None
    } else if c.unwritten > 0 {
        Some(Step::WriteEmails)
    } else if c.new_companies > 0 {
        Some(Step::FindPeople)
    } else {
        Some(Step::FindCompanies((need - c.queued).clamp(10, 50)))
    }
}

/// The campaign furthest below `need` that's free to run, scaling ones first on a tie (they come first in `campaigns`).
pub fn pick(campaigns: &[CampaignQueue], need: i64) -> Option<(&CampaignQueue, Step)> {
    campaigns
        .iter()
        .filter(|c| !c.busy && !c.failed_lately)
        .filter_map(|c| next_step(c, need).map(|step| (c, step)))
        .min_by_key(|(c, _)| c.queued)
}

async fn campaigns(db: &PgPool, project_id: Uuid, now: Ts) -> sqlx::Result<Vec<CampaignQueue>> {
    sqlx::query_as(
        "select s.id, s.name,
           (select count(*) from leads l where l.segment_id = s.id and exists (
              select 1 from email_messages e where e.lead_id = l.id and e.kind = 'initial' and e.status in ('draft', 'approved', 'scheduled'))
           ) as queued,
           (select count(*) from leads l where l.segment_id = s.id and l.email is not null and l.status in ('new', 'queued')
              and not exists (select 1 from email_messages e where e.lead_id = l.id and e.kind = 'initial' and e.status <> 'cancelled')
           ) as unwritten,
           (select count(*) from companies c where c.segment_id = s.id and c.status = 'new') as new_companies,
           exists (select 1 from agent_runs r where r.project_id = s.project_id and r.status in ('queued', 'running')
                   and r.input->>'segment_id' = s.id::text) as busy,
           exists (select 1 from agent_runs r where r.project_id = s.project_id and r.status = 'failed' and r.input ? 'autopilot'
                   and r.input->>'segment_id' = s.id::text and r.finished_at > $2 - make_interval(hours => $3)) as failed_lately
         from segments s
         where s.project_id = $1 and s.status in ('working', 'scaling')
         order by s.status = 'scaling' desc, s.position, s.created_at",
    )
    .bind(project_id)
    .bind(now)
    .bind(RETRY_AFTER_HOURS)
    .fetch_all(db)
    .await
}

#[derive(FromRow)]
struct Settings {
    org_id: Uuid,
    name: String,
    daily_email_limit: i32,
    autopilot_budget_micros: i64,
    autopilot_lead_cap: i32,
}

/// One look at a project: starts the next run it needs, or records why it waits. Returns the run it started.
pub async fn check(state: &AppState, project_id: Uuid, now: Ts) -> anyhow::Result<Option<Uuid>> {
    let db = &state.db;
    // Held until the run is queued, so two checks of the same project can't both find it idle and both start one.
    let mut lock = db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('autopilot:' || $1::text, 0))").bind(project_id).execute(&mut *lock).await?;
    let busy: bool = sqlx::query_scalar("select exists (select 1 from agent_runs where project_id = $1 and status in ('queued', 'running') and input ? 'autopilot')")
        .bind(project_id)
        .fetch_one(db)
        .await?;
    if busy {
        return Ok(None);
    }
    let s = sqlx::query_as::<_, Settings>(
        "select p.org_id, coalesce(nullif(p.name, ''), p.domain, 'your project') as name, s.daily_email_limit, s.autopilot_budget_micros,
                s.autopilot_lead_cap
         from project_settings s join projects p on p.id = s.project_id where s.project_id = $1 and p.org_id is not null",
    )
    .bind(project_id)
    .fetch_one(db)
    .await?;

    let month = month_start(now);
    let resumes = next_month(now).format("%B 1").to_string();
    let spent = spent_since(db, project_id, month).await?;
    let contacted = contacted_since(db, project_id, month).await?;
    let leads_left = i64::from(s.autopilot_lead_cap) - contacted;
    if leads_left <= 0 {
        return note(db, project_id, now, &format!("Reached this month's cap of {} new leads. It starts again on {resumes}.", s.autopilot_lead_cap)).await;
    }
    if spent >= s.autopilot_budget_micros {
        alert_budget(state, project_id, s.org_id, &s.name, s.autopilot_budget_micros, month, &resumes).await?;
        return note(db, project_id, now, &format!("Spent this month's budget. It starts again on {resumes}, or raise the budget.")).await;
    }

    let campaigns = campaigns(db, project_id, now).await?;
    if campaigns.is_empty() {
        return note(db, project_id, now, "No campaign is sending. Start one and autopilot keeps it stocked.").await;
    }
    let need = need(i64::from(s.daily_email_limit), leads_left, campaigns.len());
    let Some((campaign, step)) = pick(&campaigns, need) else {
        let waiting = if campaigns.iter().any(|c| c.busy || c.failed_lately) {
            "Every campaign has enough emails or a run of its own going."
        } else {
            "Every campaign has enough emails for the next few days."
        };
        return note(db, project_id, now, waiting).await;
    };

    let cost = estimate_cost(db, project_id, step.kind()).await?;
    let spent = spent + still_to_spend(db, project_id).await?;
    if spent + cost > s.autopilot_budget_micros {
        alert_budget(state, project_id, s.org_id, &s.name, s.autopilot_budget_micros, month, &resumes).await?;
        let why = format!("The next run would likely cost {}, more than the {} left this month. It starts again on {resumes}.", usd(cost), usd(s.autopilot_budget_micros - spent));
        return note(db, project_id, now, &why).await;
    }

    let mut input = json!({ "segment_id": campaign.id, "autopilot": true });
    if let Step::FindCompanies(count) = step {
        input["count"] = json!(count);
    }
    let run_id = agent::enqueue(&mut *lock, step.kind(), Some(project_id), Some(s.org_id), input).await?;
    lock.commit().await?;
    tracing::info!(%project_id, %run_id, kind = step.kind(), campaign = %campaign.name, "autopilot started a run");
    note(db, project_id, now, "").await?;
    Ok(Some(run_id))
}

async fn note(db: &PgPool, project_id: Uuid, now: Ts, note: &str) -> anyhow::Result<Option<Uuid>> {
    sqlx::query("update project_settings set autopilot_note = $2, autopilot_checked_at = $3 where project_id = $1")
        .bind(project_id)
        .bind(note)
        .bind(now)
        .execute(db)
        .await?;
    Ok(None)
}

/// Tells the team once a month, in the project's chats and by email to whoever gets the daily digest.
async fn alert_budget(state: &AppState, project_id: Uuid, org_id: Uuid, name: &str, budget: i64, month: Ts, resumes: &str) -> anyhow::Result<()> {
    let first = sqlx::query("update project_settings set autopilot_alerted_on = $2 where project_id = $1 and autopilot_alerted_on is distinct from $2")
        .bind(project_id)
        .bind(month.date_naive())
        .execute(&state.db)
        .await?;
    if first.rows_affected() == 0 {
        return Ok(());
    }
    let summary = format!("{name} spent its {} autopilot budget for {}. Autopilot starts again on {resumes}, or raise the budget to keep going.", usd(budget), month.format("%B"));
    let link = format!("{}/app/p/{project_id}/autopilot", state.cfg.public_url);
    let alert = ChatAlert { label: "Autopilot budget spent", summary: &summary, quote: None, link: &link, link_text: "Open autopilot" };
    crate::mail::notify_team(state, project_id, org_id, &alert).await;
    Ok(())
}

fn usd(micros: i64) -> String {
    format!("${:.2}", micros as f64 / 1_000_000.0)
}

#[derive(Serialize, FromRow)]
pub struct AutopilotRun {
    pub id: Uuid,
    pub kind: String,
    pub status: String,
    pub campaign: Option<String>,
    pub created_at: Ts,
    pub finished_at: Option<Ts>,
}

/// The overview card's numbers.
#[derive(Serialize)]
pub struct Status {
    pub enabled: bool,
    pub auto_send: bool,
    pub budget_micros: i64,
    pub spent_micros: i64,
    pub lead_cap: i32,
    pub contacted: i64,
    /// First emails in sending campaigns that wait for approval.
    pub awaiting_approval: i64,
    /// Why it's waiting, from its last check. Empty right after it started a run.
    pub note: String,
    pub checked_at: Option<Ts>,
    /// Its run going now, else its last one.
    pub run: Option<AutopilotRun>,
}

pub async fn status(db: &PgPool, project_id: Uuid) -> sqlx::Result<Status> {
    let (enabled, auto_send, budget_micros, lead_cap, note, checked_at): (bool, bool, i64, i32, String, Option<Ts>) = sqlx::query_as(
        "select autopilot, autopilot_auto_send, autopilot_budget_micros, autopilot_lead_cap, autopilot_note, autopilot_checked_at
         from project_settings where project_id = $1",
    )
    .bind(project_id)
    .fetch_one(db)
    .await?;
    let month = month_start(Utc::now());
    let awaiting_approval: i64 = sqlx::query_scalar(
        "select count(*) from email_messages e join segments s on s.id = e.segment_id
         where e.project_id = $1 and e.kind = 'initial' and e.status = 'draft' and s.status in ('working', 'scaling')",
    )
    .bind(project_id)
    .fetch_one(db)
    .await?;
    let run = sqlx::query_as::<_, AutopilotRun>(
        "select r.id, r.kind, r.status, s.name as campaign, r.created_at, r.finished_at
         from agent_runs r left join segments s on s.id::text = r.input->>'segment_id'
         where r.project_id = $1 and r.input ? 'autopilot'
         order by r.status in ('queued', 'running') desc, r.created_at desc limit 1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await?;
    Ok(Status {
        enabled,
        auto_send,
        budget_micros,
        spent_micros: spent_since(db, project_id, month).await?,
        lead_cap,
        contacted: contacted_since(db, project_id, month).await?,
        awaiting_approval,
        note,
        checked_at,
        run,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use chrono::Duration;
    use sqlx::PgPool;

    use super::*;
    use crate::config::Config;

    fn queue(queued: i64, unwritten: i64, new_companies: i64) -> CampaignQueue {
        CampaignQueue { id: Uuid::new_v4(), name: "C".into(), queued, unwritten, new_companies, busy: false, failed_lately: false }
    }

    #[test]
    fn a_campaign_short_of_three_days_writes_then_finds_people_then_companies() {
        // 20 a day for 3 days, shared by 2 campaigns.
        assert_eq!(need(20, 1000, 2), 30);
        // Only 7 new leads left this month, so neither needs more than 4.
        assert_eq!(need(20, 7, 2), 4);

        assert_eq!(next_step(&queue(30, 5, 5), 30), None);
        assert_eq!(next_step(&queue(10, 5, 5), 30), Some(Step::WriteEmails));
        assert_eq!(next_step(&queue(10, 0, 5), 30), Some(Step::FindPeople));
        assert_eq!(next_step(&queue(10, 0, 0), 30), Some(Step::FindCompanies(20)));
        assert_eq!(next_step(&queue(28, 0, 0), 30), Some(Step::FindCompanies(10)));
        assert_eq!(next_step(&queue(0, 0, 0), 300), Some(Step::FindCompanies(50)));

        let mut busy = queue(0, 0, 0);
        busy.busy = true;
        let campaigns = [queue(25, 1, 0), busy, queue(12, 0, 3), queue(12, 0, 0)];
        let (picked, step) = pick(&campaigns, 30).unwrap();
        assert_eq!((picked.id, step), (campaigns[2].id, Step::FindPeople), "the emptiest free campaign, the first on a tie");
    }

    pub(crate) fn state(db: PgPool) -> AppState {
        let cfg = Config {
            database_url: String::new(),
            app_secret: String::new(),
            public_url: "http://localhost:3349".into(),
            unsubscribe_url: "http://localhost:3349".into(),
            bind_addr: String::new(),
            agent_model: String::new(),
            agent_concurrency: 0,
            sending_enabled: false,
            bird_bin: None,
            open_signup: true,
            public_agent: false,
            porkbun_api_url: String::new(),
            twilio_api_url: String::new(),
            bots_api_url: String::new(),
            outreach_api_url: String::new(),
            postmaster_api_url: String::new(),
            operator_emails: Vec::new(),
            google: None,
            microsoft: None,
            slack: None,
            tumblr: None,
            salesforce: None,
            zoho: None,
            meta: None,
            graph_api_url: String::new(),
            crm_api_url: String::new(),
            payments_api_url: String::new(),
            ipinfo_token: None,
        };
        AppState { db, cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(16).0 }
    }

    struct Seeded {
        state: AppState,
        project: Uuid,
        campaign: Uuid,
    }

    /// A project on autopilot with $5 a month, 20 new leads a month, 10 emails a day and one working campaign.
    async fn seed(db: &PgPool) -> Seeded {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Fieldnote') returning id").bind(org).fetch_one(db).await.unwrap();
        sqlx::query(
            "insert into project_settings (project_id, daily_email_limit, autopilot, autopilot_budget_micros, autopilot_lead_cap)
             values ($1, 10, true, 5000000, 20)",
        )
        .bind(project)
        .execute(db)
        .await
        .unwrap();
        let campaign = add_campaign(db, project, "Physiotherapists").await;
        Seeded { state: state(db.clone()), project, campaign }
    }

    async fn add_campaign(db: &PgPool, project: Uuid, name: &str) -> Uuid {
        sqlx::query_scalar("insert into segments (project_id, name, status) values ($1, $2, 'working') returning id")
            .bind(project)
            .bind(name)
            .fetch_one(db)
            .await
            .unwrap()
    }

    async fn run(db: &PgPool, project: Uuid, run_kind: &str, status: &str, cost: f64, at: Ts) -> Uuid {
        sqlx::query_scalar("insert into agent_runs (project_id, kind, status, cost_usd, input, created_at) values ($1, $2, $3, $4, '{}', $5) returning id")
            .bind(project)
            .bind(run_kind)
            .bind(status)
            .bind(cost)
            .bind(at)
            .fetch_one(db)
            .await
            .unwrap()
    }

    async fn sent_first_emails(db: &PgPool, project: Uuid, n: i32, at: Ts) {
        sqlx::query(
            "with l as (insert into leads (project_id) select $1 from generate_series(1, $2) returning id)
             insert into email_messages (project_id, lead_id, direction, kind, status, sent_at) select $1, id, 'outbound', 'initial', 'sent', $3 from l",
        )
        .bind(project)
        .bind(n)
        .bind(at)
        .execute(db)
        .await
        .unwrap();
    }

    async fn started(db: &PgPool, run_id: Uuid) -> (String, serde_json::Value) {
        sqlx::query_as("select kind, input from agent_runs where id = $1").bind(run_id).fetch_one(db).await.unwrap()
    }

    async fn note_of(db: &PgPool, project: Uuid) -> String {
        sqlx::query_scalar("select autopilot_note from project_settings where project_id = $1").bind(project).fetch_one(db).await.unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn it_tops_up_one_stage_at_a_time_and_one_run_at_a_time(db: PgPool) {
        let s = seed(&db).await;
        let now = Utc::now();

        // An empty campaign needs companies, one per lead: 3 days at 10 a day, but only 20 new leads are left this month.
        let first = check(&s.state, s.project, now).await.unwrap().unwrap();
        let (run_kind, input) = started(&db, first).await;
        assert_eq!(run_kind, kind::FIND_COMPANIES);
        assert_eq!(input, json!({"segment_id": s.campaign, "autopilot": true, "count": 20}));

        // While it runs, nothing else starts, even for another campaign.
        add_campaign(&db, s.project, "Dentists").await;
        assert_eq!(check(&s.state, s.project, now).await.unwrap(), None);
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(first).execute(&db).await.unwrap();

        // With a company to search, the first campaign finds people. Both are empty, and it comes first.
        sqlx::query("insert into companies (project_id, segment_id, domain) values ($1, $2, 'physio.example')")
            .bind(s.project)
            .bind(s.campaign)
            .execute(&db)
            .await
            .unwrap();
        let second = check(&s.state, s.project, now).await.unwrap().unwrap();
        assert_eq!(started(&db, second).await, (kind::FIND_PEOPLE.to_string(), json!({"segment_id": s.campaign, "autopilot": true})));
        // A run someone started by hand keeps autopilot off that campaign, so the Dentists get companies.
        sqlx::query("update agent_runs set status = 'running', input = input - 'autopilot' where id = $1").bind(second).execute(&db).await.unwrap();
        let third = check(&s.state, s.project, now).await.unwrap().unwrap();
        let (run_kind, input) = started(&db, third).await;
        assert_eq!(run_kind, kind::FIND_COMPANIES);
        assert_ne!(input["segment_id"], json!(s.campaign));
        sqlx::query("update agent_runs set status = 'succeeded'").execute(&db).await.unwrap();

        // People found: write them, as drafts while the project approves its emails.
        sqlx::query("update companies set status = 'people_found'").execute(&db).await.unwrap();
        sqlx::query("insert into leads (project_id, segment_id, email) values ($1, $2, 'ana@physio.example')")
            .bind(s.project)
            .bind(s.campaign)
            .execute(&db)
            .await
            .unwrap();
        let fourth = check(&s.state, s.project, now).await.unwrap().unwrap();
        assert_eq!(started(&db, fourth).await, (kind::WRITE_EMAILS.to_string(), json!({"segment_id": s.campaign, "autopilot": true})));
        sqlx::query("update agent_runs set status = 'succeeded'").execute(&db).await.unwrap();

        // A failed autopilot run sets its campaign aside for a few hours. With both campaigns resting it waits.
        sqlx::query("update agent_runs set status = 'failed', finished_at = $1 where input ? 'autopilot'").bind(now).execute(&db).await.unwrap();
        assert_eq!(check(&s.state, s.project, now).await.unwrap(), None);
        assert_eq!(note_of(&db, s.project).await, "Every campaign has enough emails or a run of its own going.");
        assert!(check(&s.state, s.project, now + Duration::hours(7)).await.unwrap().is_some());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_stocked_campaign_waits_and_auto_send_writes_straight_to_the_queue(db: PgPool) {
        let s = seed(&db).await;
        sqlx::query("update project_settings set autopilot_auto_send = true, autopilot_lead_cap = 1000").execute(&db).await.unwrap();
        // 30 written emails is 3 days at 10 a day. One more lead without an email doesn't matter yet.
        sqlx::query(
            "with l as (insert into leads (project_id, segment_id, email) select $1, $2, 'x' || g || '@physio.example' from generate_series(1, 31) g returning id)
             insert into email_messages (project_id, lead_id, segment_id, direction, kind, status)
             select $1, id, $2, 'outbound', 'initial', 'draft' from l limit 30",
        )
        .bind(s.project)
        .bind(s.campaign)
        .execute(&db)
        .await
        .unwrap();
        assert_eq!(check(&s.state, s.project, Utc::now()).await.unwrap(), None);
        assert_eq!(note_of(&db, s.project).await, "Every campaign has enough emails for the next few days.");

        sqlx::query("delete from email_messages where id = (select id from email_messages limit 1)").execute(&db).await.unwrap();
        let run_id = check(&s.state, s.project, Utc::now()).await.unwrap().unwrap();
        assert_eq!(started(&db, run_id).await.1, json!({"segment_id": s.campaign, "autopilot": true}));
        assert_eq!(note_of(&db, s.project).await, "");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn it_stops_at_the_budget_and_the_lead_cap_and_starts_again_next_month(db: PgPool) {
        let s = seed(&db).await;
        let now = Utc::now();
        let last_month = month_start(now) - Duration::days(3);
        let alerted = || async {
            sqlx::query_scalar::<_, Option<chrono::NaiveDate>>("select autopilot_alerted_on from project_settings where project_id = $1")
                .bind(s.project)
                .fetch_one(&db)
                .await
                .unwrap()
        };

        // Last month's spend and sends belong to last month.
        run(&db, s.project, kind::EXPLORE, "succeeded", 9.0, last_month).await;
        sent_first_emails(&db, s.project, 25, last_month).await;
        sqlx::query("update project_settings set autopilot_alerted_on = $2 where project_id = $1")
            .bind(s.project)
            .bind((month_start(now) - Duration::days(1)).date_naive())
            .execute(&db)
            .await
            .unwrap();
        let run_id = check(&s.state, s.project, now).await.unwrap().unwrap();
        sqlx::query("delete from agent_runs where id = $1").bind(run_id).execute(&db).await.unwrap();

        // A $5 research run this month, started by hand, uses the budget up.
        run(&db, s.project, kind::EXPLORE, "succeeded", 5.0, now).await;
        assert_eq!(check(&s.state, s.project, now).await.unwrap(), None);
        assert!(note_of(&db, s.project).await.starts_with("Spent this month's budget."));
        assert_eq!(alerted().await, Some(month_start(now).date_naive()));
        assert_eq!(status(&db, s.project).await.unwrap().spent_micros, 5_000_000);

        // With $1 left, a find_companies run that has cost $1.50 on average doesn't fit.
        sqlx::query("update project_settings set autopilot_budget_micros = 6000000").execute(&db).await.unwrap();
        run(&db, s.project, kind::FIND_COMPANIES, "succeeded", 1.5, last_month).await;
        assert_eq!(check(&s.state, s.project, now).await.unwrap(), None);
        assert_eq!(
            note_of(&db, s.project).await,
            format!("The next run would likely cost $1.50, more than the $1.00 left this month. It starts again on {}.", next_month(now).format("%B 1"))
        );

        // $3 left fits it, until a research run started by hand is still going: research has cost $7 on average, so it holds that.
        sqlx::query("update project_settings set autopilot_budget_micros = 8000000").execute(&db).await.unwrap();
        let manual = run(&db, s.project, kind::EXPLORE, "running", 0.0, now).await;
        assert_eq!(check(&s.state, s.project, now).await.unwrap(), None);
        assert!(note_of(&db, s.project).await.starts_with("The next run would likely cost $1.50"));
        sqlx::query("delete from agent_runs where id = $1").bind(manual).execute(&db).await.unwrap();
        let fits = check(&s.state, s.project, now).await.unwrap().unwrap();
        sqlx::query("delete from agent_runs where id = $1").bind(fits).execute(&db).await.unwrap();

        // 20 new leads emailed this month is the cap, whatever the budget.
        sqlx::query("update project_settings set autopilot_budget_micros = 100000000").execute(&db).await.unwrap();
        sent_first_emails(&db, s.project, 20, now).await;
        assert_eq!(check(&s.state, s.project, now).await.unwrap(), None);
        assert!(note_of(&db, s.project).await.starts_with("Reached this month's cap of 20 new leads."));

        // Next month both start over.
        let next = next_month(now) + Duration::hours(1);
        assert!(check(&s.state, s.project, next).await.unwrap().is_some());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_run_costs_what_its_kind_cost_lately(db: PgPool) {
        let s = seed(&db).await;
        let other = seed(&db).await;
        let now = Utc::now();
        // Never run anywhere: a guess.
        assert_eq!(estimate_cost(&db, s.project, kind::FIND_PEOPLE).await.unwrap(), 1_000_000);
        // Another project's runs stand in until this one has its own.
        run(&db, other.project, kind::FIND_PEOPLE, "succeeded", 2.0, now).await;
        assert_eq!(estimate_cost(&db, s.project, kind::FIND_PEOPLE).await.unwrap(), 2_000_000);
        // Its own last 5 that cost something, ignoring failures and free runs.
        for (i, cost) in [9.0, 1.0, 1.0, 2.0, 2.0, 4.0].into_iter().enumerate() {
            run(&db, s.project, kind::FIND_PEOPLE, "succeeded", cost, now - Duration::days(6 - i as i64)).await;
        }
        run(&db, s.project, kind::FIND_PEOPLE, "failed", 50.0, now).await;
        run(&db, s.project, kind::FIND_PEOPLE, "succeeded", 0.0, now).await;
        assert_eq!(estimate_cost(&db, s.project, kind::FIND_PEOPLE).await.unwrap(), 2_000_000);
    }
}
