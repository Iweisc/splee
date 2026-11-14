//! Close plans: a deal's steps to a signed deal, the buyer's and ours, with dates. The agent drafts one from the conversation
//! (agent/close_plans.rs), the team edits it in the deal's window, and the buyer gets it at PUBLIC_URL/plan/{token}, where
//! they tick off their own steps. Their first look and each step they finish ping the project's chats.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::hooks::{self, ChatAlert};
use crate::models::{Lead, Ts};

/// Steps a plan holds. The agent writes at most 8, which leaves room for the team's own.
pub const MAX_STEPS: i64 = 20;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Step {
    pub id: Uuid,
    pub title: String,
    pub detail: String,
    /// "us" or "them".
    pub side: String,
    pub due_date: Option<NaiveDate>,
    pub done_at: Option<Ts>,
    pub done_by: Option<String>,
}

/// The deal's plan for the app.
#[derive(Debug, Serialize, FromRow)]
pub struct Plan {
    pub id: Uuid,
    pub token: String,
    pub goal: Option<String>,
    /// writing while the agent drafts it, ready once it has a goal, else failed.
    pub status: String,
    pub views: i32,
    pub first_viewed_at: Option<Ts>,
    pub last_viewed_at: Option<Ts>,
    #[sqlx(skip)]
    pub url: String,
    #[sqlx(skip)]
    pub steps: Vec<Step>,
}

pub fn url(public_url: &str, token: &str) -> String {
    format!("{}/plan/{token}", public_url.trim_end_matches('/'))
}

async fn steps(db: &PgPool, plan_id: Uuid) -> sqlx::Result<Vec<Step>> {
    sqlx::query_as("select id, title, detail, side, due_date, done_at, done_by from close_plan_steps where plan_id = $1 order by position, created_at")
        .bind(plan_id)
        .fetch_all(db)
        .await
}

pub async fn for_deal(db: &PgPool, public_url: &str, deal_id: Uuid) -> sqlx::Result<Option<Plan>> {
    let plan = sqlx::query_as::<_, Plan>(
        "select p.id, p.token, p.goal, p.views, p.first_viewed_at, p.last_viewed_at,
                case when r.status in ('queued', 'running') then 'writing' when p.goal is not null then 'ready' else 'failed' end as status
         from close_plans p left join agent_runs r on r.id = p.run_id where p.deal_id = $1",
    )
    .bind(deal_id)
    .fetch_optional(db)
    .await?;
    let Some(plan) = plan else { return Ok(None) };
    Ok(Some(Plan { url: url(public_url, &plan.token), steps: steps(db, plan.id).await?, ..plan }))
}

/// The deal's plan, made empty the first time.
pub async fn ensure(tx: &mut PgConnection, project_id: Uuid, deal_id: Uuid) -> sqlx::Result<Uuid> {
    let token: String = crate::crypto::random_token().chars().take(20).collect();
    sqlx::query_scalar(
        "insert into close_plans (project_id, deal_id, token) values ($1, $2, $3)
         on conflict (deal_id) do update set deal_id = excluded.deal_id returning id",
    )
    .bind(project_id)
    .bind(deal_id)
    .bind(token)
    .fetch_one(tx)
    .await
}

fn side(raw: &str) -> AppResult<&'static str> {
    match raw {
        "us" => Ok("us"),
        "them" => Ok("them"),
        _ => Err(AppError::BadRequest("side is us or them".into())),
    }
}

fn title(raw: &str) -> AppResult<String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(AppError::BadRequest("A step needs a title".into()));
    }
    Ok(t.chars().take(120).collect())
}

#[derive(Deserialize)]
pub struct NewStep {
    pub title: String,
    #[serde(default)]
    pub detail: String,
    pub side: String,
    pub due_date: Option<NaiveDate>,
}

/// Adds a step at the end of the plan, making the plan first if the deal has none.
pub async fn add_step(db: &PgPool, project_id: Uuid, deal_id: Uuid, step: NewStep) -> AppResult<()> {
    let (title, side) = (title(&step.title)?, side(&step.side)?);
    let mut tx = db.begin().await?;
    let plan = ensure(&mut tx, project_id, deal_id).await?;
    let count: i64 = sqlx::query_scalar("select count(*) from close_plan_steps where plan_id = $1").bind(plan).fetch_one(&mut *tx).await?;
    if count >= MAX_STEPS {
        return Err(AppError::BadRequest(format!("A plan holds up to {MAX_STEPS} steps")));
    }
    sqlx::query(
        "insert into close_plan_steps (plan_id, position, title, detail, side, due_date)
         values ($1, (select coalesce(max(position) + 1, 0) from close_plan_steps where plan_id = $1), $2, $3, $4, $5)",
    )
    .bind(plan)
    .bind(title)
    .bind(step.detail.trim().chars().take(300).collect::<String>())
    .bind(side)
    .bind(step.due_date)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Deserialize)]
pub struct StepChanges {
    pub title: Option<String>,
    pub side: Option<String>,
    /// Null clears the date. Left out keeps it.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    pub due_date: Option<Option<NaiveDate>>,
    pub done: Option<bool>,
}

/// The deal a step is on, when it's in the organization.
pub async fn deal_of_step(db: &PgPool, org_id: Uuid, step_id: Uuid) -> AppResult<Uuid> {
    sqlx::query_scalar("select p.deal_id from close_plan_steps s join close_plans p on p.id = s.plan_id join deals d on d.id = p.deal_id where s.id = $1 and d.org_id = $2")
        .bind(step_id)
        .bind(org_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// The team's change to a step. Ticking one marks it done by us, whichever side it's on.
pub async fn update_step(db: &PgPool, step_id: Uuid, changes: StepChanges) -> AppResult<()> {
    let title = changes.title.as_deref().map(title).transpose()?;
    let side = changes.side.as_deref().map(side).transpose()?;
    sqlx::query(
        "update close_plan_steps set title = coalesce($2, title), side = coalesce($3, side),
                due_date = case when $4 then $5 else due_date end,
                done_at = case when $6 is null then done_at when $6 then coalesce(done_at, now()) end,
                done_by = case when $6 is null then done_by when $6 then coalesce(done_by, 'us') end
         where id = $1",
    )
    .bind(step_id)
    .bind(title)
    .bind(side)
    .bind(changes.due_date.is_some())
    .bind(changes.due_date.flatten())
    .bind(changes.done)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn delete_step(db: &PgPool, step_id: Uuid) -> sqlx::Result<()> {
    sqlx::query("delete from close_plan_steps where id = $1").bind(step_id).execute(db).await.map(|_| ())
}

/// What the buyer's page shows: the plan and who it's between, nothing the team keeps to itself.
#[derive(Debug, Serialize, FromRow)]
pub struct Public {
    pub goal: String,
    pub first_name: String,
    pub company: String,
    pub company_domain: Option<String>,
    /// The project's company name and domain, from its research.
    pub from_name: String,
    pub from_domain: Option<String>,
    pub sender_name: String,
    /// The mailbox that last wrote to them: its booking link, else its Splee booking page, else the project's. Null without any.
    pub booking_url: Option<String>,
    pub reply_to: Option<String>,
    #[sqlx(skip)]
    pub steps: Vec<Step>,
}

pub async fn public(db: &PgPool, public_url: &str, token: &str) -> sqlx::Result<Option<Public>> {
    let row = sqlx::query_as::<_, (Uuid,)>("select id from close_plans where token = $1 and goal is not null").bind(token).fetch_optional(db).await?;
    let Some((plan_id,)) = row else { return Ok(None) };
    let page = sqlx::query_as::<_, Public>(
        "select p.goal, l.first_name, l.company_name as company, l.company_domain::text as company_domain,
                coalesce(nullif(pr.company->>'name', ''), pr.name) as from_name, pr.domain::text as from_domain,
                coalesce(nullif(m.from_name, ''), s.sender_name, '') as sender_name,
                coalesce(nullif(m.booking_link, ''), $2 || '/book/' || b.slug, nullif(s.booking_link, '')) as booking_url,
                m.email as reply_to
         from close_plans p join deals d on d.id = p.deal_id join leads l on l.id = d.lead_id join projects pr on pr.id = p.project_id
         left join project_settings s on s.project_id = p.project_id
         left join lateral (select e.mailbox_id from email_messages e where e.lead_id = l.id and e.direction = 'outbound' and e.mailbox_id is not null
                            and e.status = 'sent' order by e.sent_at desc limit 1) e on true
         left join mailboxes m on m.id = e.mailbox_id
         left join booking_pages b on b.mailbox_id = m.id
         where p.id = $1",
    )
    .bind(plan_id)
    .bind(public_url.trim_end_matches('/'))
    .fetch_one(db)
    .await?;
    Ok(Some(Public { steps: steps(db, plan_id).await?, ..page }))
}

/// Counts a look at the buyer's page and returns whether it was the first.
pub async fn record_view(db: &PgPool, token: &str) -> sqlx::Result<bool> {
    let first: Option<bool> = sqlx::query_scalar(
        "update close_plans set views = views + 1, first_viewed_at = coalesce(first_viewed_at, now()), last_viewed_at = now()
         where token = $1 and goal is not null returning views = 1",
    )
    .bind(token)
    .fetch_optional(db)
    .await?;
    Ok(first == Some(true))
}

/// The buyer ticks or unticks one of their own steps. Returns the step when it changed.
pub async fn tick(db: &PgPool, token: &str, step_id: Uuid, done: bool) -> sqlx::Result<Option<Step>> {
    sqlx::query_as(
        "update close_plan_steps s set done_at = case when $3 then now() end, done_by = case when $3 then 'them' end
         from close_plans p where p.id = s.plan_id and p.token = $1 and p.goal is not null and s.id = $2 and s.side = 'them'
           and (s.done_at is not null) <> $3
         returning s.id, s.title, s.detail, s.side, s.due_date, s.done_at, s.done_by",
    )
    .bind(token)
    .bind(step_id)
    .bind(done)
    .fetch_optional(db)
    .await
}

/// What the buyer did on the page, for the project's chats.
pub enum Seen<'a> {
    Opened,
    Finished(&'a Step),
}

pub async fn announce(db: &PgPool, public_url: &str, token: &str, seen: Seen<'_>) -> anyhow::Result<()> {
    let (project_id, deal_id, lead_id, done, total): (Uuid, Uuid, Uuid, i64, i64) = sqlx::query_as(
        "select p.project_id, p.deal_id, d.lead_id, (select count(*) from close_plan_steps s where s.plan_id = p.id and s.done_at is not null),
                (select count(*) from close_plan_steps s where s.plan_id = p.id)
         from close_plans p join deals d on d.id = p.deal_id where p.token = $1",
    )
    .bind(token)
    .fetch_one(db)
    .await?;
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(lead_id).fetch_one(db).await?;
    let who = if lead.full_name.trim().is_empty() { lead.email.clone().unwrap_or_default() } else { lead.full_name.trim().to_string() };
    let who = if lead.company_name.trim().is_empty() { who } else { format!("{who} ({})", lead.company_name.trim()) };
    let (label, summary) = match seen {
        Seen::Opened => ("Plan opened", format!("{who} opened the close plan")),
        Seen::Finished(step) => ("Plan step done", format!("{who} finished \"{}\" on the close plan, {done} of {total} steps done", step.title)),
    };
    let link = format!("{}/app/pipeline?deal={deal_id}", public_url.trim_end_matches('/'));
    let alert = ChatAlert { label, summary: &summary, quote: None, link: &link, link_text: "Open the deal" };
    hooks::post_chat(db, project_id, &alert).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_buyer_sees_the_plan_once_written_and_ticks_only_their_own_steps(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into project_settings (project_id, booking_link, sender_name) values ($1, 'https://cal.example/dana', 'Dana')").bind(project).execute(&db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, first_name, company_name) values ($1, 'Lukas Weber', 'Lukas', 'Quanta Forms') returning id")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Proposal', 'open', 0) returning id").bind(org).fetch_one(&db).await.unwrap();
        let deal: Uuid = sqlx::query_scalar("insert into deals (org_id, project_id, lead_id, stage_id) values ($1, $2, $3, $4) returning id")
            .bind(org)
            .bind(project)
            .bind(lead)
            .bind(stage)
            .fetch_one(&db)
            .await
            .unwrap();

        let step = |title: &str, side: &str| NewStep { title: title.into(), detail: String::new(), side: side.into(), due_date: None };
        add_step(&db, project, deal, step("Send the order form", "us")).await.unwrap();
        add_step(&db, project, deal, step("Sign the order form", "them")).await.unwrap();
        assert!(add_step(&db, project, deal, step(" ", "them")).await.is_err());
        assert!(add_step(&db, project, deal, step("Pay", "both")).await.is_err());
        let plan = for_deal(&db, "https://app.example", deal).await.unwrap().unwrap();
        assert_eq!(plan.url, format!("https://app.example/plan/{}", plan.token));
        let (ours, theirs) = (plan.steps[0].id, plan.steps[1].id);

        assert!(public(&db, "https://app.example", &plan.token).await.unwrap().is_none(), "not shared before it has a goal");
        assert!(!record_view(&db, &plan.token).await.unwrap());
        sqlx::query("update close_plans set goal = 'Quanta Forms live by January'").execute(&db).await.unwrap();
        let page = public(&db, "https://app.example", &plan.token).await.unwrap().unwrap();
        assert_eq!((page.from_name.as_str(), page.first_name.as_str(), page.steps.len()), ("Fieldnote", "Lukas", 2));
        assert_eq!(page.booking_url.as_deref(), Some("https://cal.example/dana"));
        assert!(record_view(&db, &plan.token).await.unwrap(), "the first look");
        assert!(!record_view(&db, &plan.token).await.unwrap());

        assert!(tick(&db, &plan.token, ours, true).await.unwrap().is_none(), "our step isn't theirs to tick");
        assert!(tick(&db, "nope", theirs, true).await.unwrap().is_none());
        assert_eq!(tick(&db, &plan.token, theirs, true).await.unwrap().unwrap().done_by.as_deref(), Some("them"));
        update_step(&db, ours, StepChanges { title: None, side: None, due_date: None, done: Some(true) }).await.unwrap();
        let plan = for_deal(&db, "https://app.example", deal).await.unwrap().unwrap();
        assert_eq!(plan.steps.iter().map(|s| s.done_by.as_deref()).collect::<Vec<_>>(), vec![Some("us"), Some("them")]);
        assert_eq!(plan.views, 2);
    }
}
