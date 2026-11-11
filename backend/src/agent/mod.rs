//! Agent runs: every Claude-powered task is a row in agent_runs, picked up by the worker.
//! Routes call `enqueue` (and `leads_without_initial` to refuse empty write runs). The worker, SDK wrapper and task implementations live in this module.

mod bluesky;
mod directory;
mod discourse;
mod explore;
mod hashnode;
mod hiring;
mod lemmy;
mod linkedin;
mod mastodon;
mod nostr;
mod prospect;
mod reddit;
mod replies;
pub mod sdk;
pub mod social;
mod stackexchange;
mod threads;
mod tumblr;
mod write;
pub mod x;
mod youtube;

pub use write::{leads_without_initial, signature};

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::events;
use crate::models::AgentRun;
use crate::state::AppState;

/// Valid values for agent_runs.kind. Keep in sync with the check constraint in the migration.
pub mod kind {
    pub const EXPLORE: &str = "explore";
    pub const FIND_COMPANIES: &str = "find_companies";
    pub const FIND_PEOPLE: &str = "find_people";
    pub const WRITE_EMAILS: &str = "write_emails";
    pub const CLASSIFY_REPLY: &str = "classify_reply";
    pub const DRAFT_REPLY: &str = "draft_reply";
    pub const LINKEDIN_SEQUENCE: &str = "linkedin_sequence";
    pub const EXPLORER: &str = "explorer";
    pub const SEGMENT_FROM_PROMPT: &str = "segment_from_prompt";
    pub const SOCIAL: &str = "social";
    pub const WRITE_TEXTS: &str = "write_texts";
}

pub async fn enqueue<'e, E: sqlx::PgExecutor<'e>>(db: E, kind: &str, project_id: Option<Uuid>, org_id: Option<Uuid>, input: Value) -> sqlx::Result<Uuid> {
    sqlx::query_scalar("insert into agent_runs (kind, project_id, org_id, input) values ($1, $2, $3, $4) returning id")
        .bind(kind)
        .bind(project_id)
        .bind(org_id)
        .bind(input)
        .fetch_one(db)
        .await
}

/// What a finished task reports: the `done` event line and the JSON stored in agent_runs.output.
pub struct Outcome {
    pub message: String,
    pub output: Value,
}

impl Outcome {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), output: json!({}) }
    }
}

/// One executing run. Steps report progress through it and Claude calls add their cost to the run.
pub struct Ctx {
    pub state: AppState,
    pub run: AgentRun,
    step: AtomicI32,
}

impl Ctx {
    pub fn db(&self) -> &PgPool {
        &self.state.db
    }

    pub fn step(&self) -> i32 {
        self.step.load(Ordering::Relaxed)
    }

    pub fn input<T: DeserializeOwned>(&self) -> anyhow::Result<T> {
        serde_json::from_value(self.run.input.clone()).map_err(|e| anyhow::anyhow!("invalid run input: {e}"))
    }

    pub fn project_id(&self) -> anyhow::Result<Uuid> {
        self.run.project_id.ok_or_else(|| anyhow::anyhow!("this run has no project"))
    }

    pub async fn log(&self, message: impl AsRef<str>) -> anyhow::Result<()> {
        events::emit(&self.state, self.run.id, self.step(), "log", message.as_ref(), json!({})).await?;
        Ok(())
    }

    pub async fn begin_step(&self, step: i32, title: &str) -> anyhow::Result<()> {
        self.step.store(step, Ordering::Relaxed);
        sqlx::query("update agent_runs set step = $2 where id = $1").bind(self.run.id).bind(step).execute(self.db()).await?;
        events::emit(&self.state, self.run.id, step, "phase", title, json!({"step": step, "title": title, "status": "started"})).await?;
        Ok(())
    }

    pub async fn finish_step(&self, step: i32, title: &str) -> anyhow::Result<()> {
        events::emit(&self.state, self.run.id, step, "phase", title, json!({"step": step, "title": title, "status": "finished"})).await?;
        Ok(())
    }

    pub async fn ask<T: DeserializeOwned>(&self, call: &sdk::Call) -> anyhow::Result<T> {
        sdk::ask(&self.state, Some(self), call).await
    }

    async fn add_cost(&self, usd: f64) {
        if usd <= 0.0 {
            return;
        }
        let res = sqlx::query("update agent_runs set cost_usd = cost_usd + $2 where id = $1")
            .bind(self.run.id)
            .bind(usd)
            .execute(self.db())
            .await;
        if let Err(e) = res {
            tracing::warn!(error = ?e, run_id = %self.run.id, "could not record run cost");
        }
    }
}

pub fn spawn_workers(state: AppState) {
    let s = state.clone();
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(3600), || expire_visitor_data(&s.db), "visitor data cleanup").await });

    tokio::spawn(async move {
        // A previous process died mid-run. Its Claude subprocesses are gone, so start those runs over.
        if let Err(e) = sqlx::query("update agent_runs set status = 'queued', started_at = null where status = 'running'")
            .execute(&state.db)
            .await
        {
            tracing::error!(error = ?e, "could not requeue interrupted runs");
        }

        let slots = Arc::new(Semaphore::new(state.cfg.agent_concurrency.max(1)));
        loop {
            let permit = slots.clone().acquire_owned().await.expect("semaphore is never closed");
            match claim(&state.db).await {
                Ok(Some(run)) => {
                    let state = state.clone();
                    tokio::spawn(async move {
                        execute(state, run).await;
                        drop(permit);
                    });
                }
                Ok(None) => {
                    drop(permit);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(e) => {
                    drop(permit);
                    tracing::error!(error = ?e, "could not claim agent run");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });
}

/// Deletes signed-out demos and explorer runs a week after they start, and support chats a week after their last message.
/// All of them keep the visitor's IP. Only the tab that started a demo holds its claim token, in sessionStorage, so a week
/// leaves plenty of time to sign up. The chat widget starts a new conversation when its stored one is gone.
/// Expired sign-in sessions go too: sign-in already ignores them, and nothing else removes them. So do website visitors'
/// page views after 180 days (visitors.rs).
pub(crate) async fn expire_visitor_data(db: &PgPool) -> anyhow::Result<()> {
    crate::visitors::expire(db).await?;
    sqlx::query("delete from sessions where expires_at < now()").execute(db).await?;
    sqlx::query("delete from projects where org_id is null and created_at < now() - interval '7 days'").execute(db).await?;
    sqlx::query("delete from agent_runs where kind = 'explorer' and project_id is null and created_at < now() - interval '7 days'").execute(db).await?;
    sqlx::query(
        "delete from support_conversations c where c.created_at < now() - interval '7 days'
           and not exists (select 1 from support_messages m where m.conversation_id = c.id and m.created_at >= now() - interval '7 days')",
    )
    .execute(db)
    .await?;
    Ok(())
}

async fn claim(db: &PgPool) -> sqlx::Result<Option<AgentRun>> {
    sqlx::query_as::<_, AgentRun>(
        "update agent_runs set status = 'running', started_at = now()
         where id = (select id from agent_runs where status = 'queued' order by created_at for update skip locked limit 1)
         returning *",
    )
    .fetch_optional(db)
    .await
}

async fn execute(state: AppState, run: AgentRun) {
    let run_id = run.id;
    let ctx = Ctx { state: state.clone(), run, step: AtomicI32::new(0) };
    tracing::info!(%run_id, kind = %ctx.run.kind, "agent run started");

    // Dropping the task future on cancel also kills the Claude subprocess it is waiting on.
    let result = tokio::select! {
        r = dispatch(&ctx) => Some(r),
        _ = wait_for_cancel(&state.db, run_id) => None,
    };

    let step = ctx.step();
    // Before the final event, so the UI's refresh on that event already sees the settled project.
    if !matches!(result, Some(Ok(_))) {
        settle_research(&state.db, &ctx.run).await;
    }
    let finished = match result {
        None => events::emit(&state, run_id, step, "error", "Run cancelled", json!({"cancelled": true})).await.map(|_| ()),
        Some(Ok(outcome)) => {
            let updated = sqlx::query(
                "update agent_runs set status = 'succeeded', output = $2, error = '', finished_at = now() where id = $1 and status = 'running'",
            )
            .bind(run_id)
            .bind(&outcome.output)
            .execute(&state.db)
            .await;
            match updated {
                Ok(_) => events::emit(&state, run_id, step, "done", &outcome.message, outcome.output).await.map(|_| ()),
                Err(e) => Err(e),
            }
        }
        Some(Err(e)) => {
            let message = format!("{e:#}");
            tracing::warn!(%run_id, error = %message, "agent run failed");
            let updated = sqlx::query(
                "update agent_runs set status = 'failed', error = $2, finished_at = now() where id = $1 and status = 'running'",
            )
            .bind(run_id)
            .bind(&message)
            .execute(&state.db)
            .await;
            match updated {
                Ok(_) => events::emit(&state, run_id, step, "error", &message, json!({})).await.map(|_| ()),
                Err(e) => Err(e),
            }
        }
    };
    if let Err(e) = finished {
        tracing::error!(%run_id, error = ?e, "could not finish agent run");
    }
}

/// Only a research run that succeeds moves its project out of `researching`. One that was cancelled or failed
/// would leave it there for good, which hides the profile and disables "Re-run research". So it goes back to
/// `ready` when step 1 saved a profile, else `failed`, unless another research run for the project is already lined up.
pub async fn settle_research(db: &PgPool, run: &AgentRun) {
    let Some(project_id) = run.project_id.filter(|_| run.kind == kind::EXPLORE) else {
        return;
    };
    let settled = sqlx::query(
        "update projects set status = case when company = '{}'::jsonb then 'failed' else 'ready' end
         where id = $1 and status = 'researching'
           and not exists (select 1 from agent_runs where project_id = $1 and kind = $2 and status in ('queued', 'running') and id <> $3)",
    )
    .bind(project_id)
    .bind(kind::EXPLORE)
    .bind(run.id)
    .execute(db)
    .await;
    if let Err(e) = settled {
        tracing::error!(run_id = %run.id, error = ?e, "could not settle the project after research stopped");
    }
}

async fn wait_for_cancel(db: &PgPool, run_id: Uuid) {
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let status: Option<String> = sqlx::query_scalar("select status from agent_runs where id = $1")
            .bind(run_id)
            .fetch_optional(db)
            .await
            .unwrap_or(Some("running".into()));
        if status.as_deref() != Some("running") {
            return;
        }
    }
}

async fn dispatch(ctx: &Ctx) -> anyhow::Result<Outcome> {
    match ctx.run.kind.as_str() {
        kind::EXPLORE => explore::run(ctx).await,
        kind::FIND_COMPANIES => prospect::find_companies_run(ctx).await,
        kind::FIND_PEOPLE => prospect::find_people_run(ctx).await,
        kind::WRITE_EMAILS => write::write_emails_run(ctx).await,
        kind::LINKEDIN_SEQUENCE => write::linkedin_run(ctx).await,
        kind::CLASSIFY_REPLY => replies::classify_run(ctx).await,
        kind::DRAFT_REPLY => replies::draft_run(ctx).await,
        kind::EXPLORER => prospect::explorer_run(ctx).await,
        kind::SEGMENT_FROM_PROMPT => explore::segment_from_prompt_run(ctx).await,
        kind::SOCIAL => social::run(ctx).await,
        kind::WRITE_TEXTS => write::texts_run(ctx).await,
        other => anyhow::bail!("unsupported run kind {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn expire_visitor_data_only_deletes_week_old_visitor_work(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project = |org: Option<Uuid>, age: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into projects (org_id, created_at) values ($1, now() - $2::interval) returning id")
                .bind(org)
                .bind(age)
                .fetch_one(&db)
        };
        let old_demo = project(None, "8 days").await.unwrap();
        let new_demo = project(None, "6 days").await.unwrap();
        let old_claimed = project(Some(org), "8 days").await.unwrap();
        let explorer = |age: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into agent_runs (kind, input, created_at) values ('explorer', '{}', now() - $1::interval) returning id")
                .bind(age)
                .fetch_one(&db)
        };
        let old_explorer = explorer("8 days").await.unwrap();
        let new_explorer = explorer("6 days").await.unwrap();
        let chat = |last_message: &'static str| {
            let db = db.clone();
            async move {
                let id: Uuid = sqlx::query_scalar(
                    "insert into support_conversations (visitor_token, ip, created_at) values ('v', '203.0.113.9', now() - interval '8 days') returning id",
                )
                .fetch_one(&db)
                .await
                .unwrap();
                sqlx::query("insert into support_messages (conversation_id, role, content, created_at) values ($1, 'user', 'hi', now() - $2::interval)")
                    .bind(id)
                    .bind(last_message)
                    .execute(&db)
                    .await
                    .unwrap();
                id
            }
        };
        let idle_chat = chat("8 days").await;
        let active_chat = chat("1 day").await;
        let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('ana@example.com', 'x') returning id").fetch_one(&db).await.unwrap();
        for (token, expires) in [("expired", "-1 minute"), ("live", "1 day")] {
            sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + $4::interval)")
                .bind(token)
                .bind(user)
                .bind(org)
                .bind(expires)
                .execute(&db)
                .await
                .unwrap();
        }

        expire_visitor_data(&db).await.unwrap();

        let projects: Vec<Uuid> = sqlx::query_scalar("select id from projects").fetch_all(&db).await.unwrap();
        assert!(!projects.contains(&old_demo));
        assert!(projects.contains(&new_demo) && projects.contains(&old_claimed));
        let runs: Vec<Uuid> = sqlx::query_scalar("select id from agent_runs").fetch_all(&db).await.unwrap();
        assert!(!runs.contains(&old_explorer));
        assert!(runs.contains(&new_explorer));
        let chats: Vec<Uuid> = sqlx::query_scalar("select id from support_conversations").fetch_all(&db).await.unwrap();
        assert!(!chats.contains(&idle_chat) && chats.contains(&active_chat));
        let sessions: Vec<String> = sqlx::query_scalar("select token_hash from sessions").fetch_all(&db).await.unwrap();
        assert_eq!(sessions, vec!["live"]);
    }
}
