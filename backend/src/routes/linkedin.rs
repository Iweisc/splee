//! LinkedIn sequences: a connect note and two messages per lead, sent by hand from the user's own profile. Each step comes
//! due a few days after the one before it was sent, and a reply on either channel stops both.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::leads::{LEAD_ROW_SELECT, LeadRow};
use super::{util_input, util_scope};
use crate::agent::{self, kind};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::models::{Lead, LinkedinMessage};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/linkedin", get(list))
        .route("/leads/{id}/linkedin", post(generate))
        .route("/leads/{id}/linkedin/replied", post(replied))
        .route("/linkedin/{id}", patch(update))
        .route("/unipile/{token}", post(unipile_event))
}

/// Days from sending a step to the next one coming due: a connect note leaves time to accept, a message time to answer.
fn wait_after(kind: &str) -> i32 {
    if kind == "connect" { 3 } else { 4 }
}

#[derive(Deserialize)]
struct ListQuery {
    segment_id: Option<Uuid>,
}

#[derive(Serialize)]
struct Thread {
    lead: LeadRow,
    messages: Vec<LinkedinMessage>,
}

/// Leads with a LinkedIn profile or a generated sequence, so the page can offer "generate" for the rest.
async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(q): Query<ListQuery>) -> AppResult<Json<Vec<Thread>>> {
    auth.project(&state.db, id).await?;
    let leads = sqlx::query_as::<_, LeadRow>(&format!(
        "{LEAD_ROW_SELECT}
         where l.project_id = $1 and ($2::uuid is null or l.segment_id = $2)
           and (l.linkedin_url <> '' or exists (select 1 from linkedin_messages m where m.lead_id = l.id))
         order by l.created_at desc limit 500"
    ))
    .bind(id)
    .bind(q.segment_id)
    .fetch_all(&state.db)
    .await?;
    let ids: Vec<Uuid> = leads.iter().map(|l| l.lead.id).collect();
    let messages = sqlx::query_as::<_, LinkedinMessage>("select * from linkedin_messages where lead_id = any($1) order by step, created_at")
        .bind(&ids)
        .fetch_all(&state.db)
        .await?;
    let mut by_lead: HashMap<Uuid, Vec<LinkedinMessage>> = HashMap::new();
    for message in messages {
        by_lead.entry(message.lead_id).or_default().push(message);
    }
    let threads = leads
        .into_iter()
        .map(|lead| {
            let messages = by_lead.remove(&lead.lead.id).unwrap_or_default();
            Thread { lead, messages }
        })
        .collect();
    Ok(Json(threads))
}

async fn generate(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    let run_id =
        agent::enqueue(&state.db, kind::LINKEDIN_SEQUENCE, Some(lead.project_id), Some(auth.org_id), json!({ "lead_id": lead.id }))
            .await?;
    Ok(Json(json!({ "run_id": run_id })))
}

#[derive(Deserialize)]
struct UpdateBody {
    status: String,
    body: Option<String>,
}

async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<UpdateBody>) -> AppResult<Json<LinkedinMessage>> {
    let message: LinkedinMessage = util_scope::load(&state.db, "linkedin_messages", id, auth.org_id).await?;
    util_input::one_of("status", &body.status, &["todo", "done", "skipped"])?;
    if message.sent_via == "unipile" {
        return Err(AppError::BadRequest("Splee already sent this on LinkedIn".into()));
    }
    Ok(Json(mark(&state.db, &message, &body.status, body.body).await?))
}

/// Sending a step brings the next one due after wait_after, skipping it brings the next one due now, and undoing it
/// takes the later ones off the list until it's sent again.
pub async fn mark(db: &PgPool, message: &LinkedinMessage, status: &str, body: Option<String>) -> sqlx::Result<LinkedinMessage> {
    let mut tx = db.begin().await?;
    let updated = sqlx::query_as::<_, LinkedinMessage>(
        "update linkedin_messages set status = $2, body = coalesce($3, body),
           done_at = case when $2 = 'done' then coalesce(done_at, now()) end,
           due_at = case when $2 = 'todo' then coalesce(due_at, now()) else due_at end
         where id = $1 returning *",
    )
    .bind(message.id)
    .bind(status)
    .bind(body)
    .fetch_one(&mut *tx)
    .await?;
    if status == "todo" {
        sqlx::query("update linkedin_messages set due_at = null where lead_id = $1 and step > $2 and status = 'todo'")
            .bind(message.lead_id)
            .bind(message.step)
            .execute(&mut *tx)
            .await?;
    } else {
        // Only the next step to do moves, so steps already sent or skipped keep their record.
        sqlx::query(
            "update linkedin_messages set due_at = case when $3 = 'done' then now() + make_interval(days => $4) else now() end
             where id = (select id from linkedin_messages where lead_id = $1 and step > $2 and status = 'todo' order by step limit 1)",
        )
        .bind(message.lead_id)
        .bind(message.step)
        .bind(status)
        .bind(wait_after(&message.kind))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(updated)
}

async fn replied(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    let updated = stop_for_reply(&state.db, &lead, "They replied on LinkedIn").await?;
    hooks::team_set_status(&state, &lead.status, &updated).await;
    Ok(Json(json!({ "ok": true })))
}

/// They answered somewhere other than email, like LinkedIn or a text: the lead moves to Replied and every sequence stops, like
/// an email reply. `why` is shown on the emails and texts it cancels.
pub async fn stop_for_reply(db: &PgPool, lead: &Lead, why: &str) -> sqlx::Result<Lead> {
    let mut tx = db.begin().await?;
    let updated = sqlx::query_as::<_, Lead>(
        "update leads set status = case when status in ('new', 'queued', 'contacted') then 'replied' else status end where id = $1 returning *",
    )
    .bind(lead.id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "update email_messages set status = 'cancelled', error = $2
         where lead_id = $1 and kind in ('initial', 'follow_up') and status in ('draft', 'approved', 'scheduled')",
    )
    .bind(lead.id)
    .bind(why)
    .execute(&mut *tx)
    .await?;
    sqlx::query(crate::mail::STOP_LINKEDIN).bind(lead.id).execute(&mut *tx).await?;
    sqlx::query(crate::twilio::STOP_TEXTS).bind(lead.id).bind(why).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(updated)
}

/// Unipile's posts for the project's LinkedIn account. The token in the URL picks the project, like a booking webhook.
async fn unipile_event(State(state): State<AppState>, Path(token): Path<String>, Json(body): Json<Value>) -> AppResult<Json<Value>> {
    let (project_id, account): (Uuid, String) =
        sqlx::query_as("select project_id, unipile_account_id from project_settings where unipile_webhook_token = $1 and unipile_token_enc <> ''")
            .bind(&token)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound)?;
    if let Some((lead, text)) = crate::unipile::on_event(&state.db, project_id, &account, &body).await? {
        hooks::announce_linkedin_reply(&state, &lead, &text);
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn sequence(db: &PgPool) -> (Lead, Vec<LinkedinMessage>) {
        let project: Uuid = sqlx::query_scalar("insert into projects (name) values ('Acme') returning id").fetch_one(db).await.unwrap();
        let lead = sqlx::query_as::<_, Lead>("insert into leads (project_id, full_name, status) values ($1, 'Sam Lee', 'contacted') returning *")
            .bind(project)
            .fetch_one(db)
            .await
            .unwrap();
        for (step, kind) in [(1, "connect"), (2, "message"), (3, "message")] {
            sqlx::query(
                "insert into linkedin_messages (lead_id, project_id, step, kind, body, due_at) values ($1, $2, $3, $4, 'Hi', case when $3 = 1 then now() end)",
            )
            .bind(lead.id)
            .bind(project)
            .bind(step)
            .bind(kind)
            .execute(db)
            .await
            .unwrap();
        }
        (lead.clone(), steps(db, lead.id).await)
    }

    async fn steps(db: &PgPool, lead: Uuid) -> Vec<LinkedinMessage> {
        sqlx::query_as("select * from linkedin_messages where lead_id = $1 order by step").bind(lead).fetch_all(db).await.unwrap()
    }

    /// Days from now until each step is due, to the nearest day, None when it isn't due yet.
    fn days(steps: &[LinkedinMessage]) -> Vec<Option<i64>> {
        steps.iter().map(|m| m.due_at.map(|d| ((d - chrono::Utc::now()).num_minutes() + 720).div_euclid(1440))).collect()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_step_comes_due_after_the_one_before(db: PgPool) {
        let (lead, s) = sequence(&db).await;
        assert_eq!(days(&s), [Some(0), None, None]);

        mark(&db, &s[0], "done", None).await.unwrap();
        let s = steps(&db, lead.id).await;
        assert!(s[0].done_at.is_some());
        assert_eq!(days(&s)[1..], [Some(3), None]);

        // Undo takes the next step off the list, and skipping brings it due right away.
        mark(&db, &s[0], "todo", None).await.unwrap();
        assert_eq!(days(&steps(&db, lead.id).await)[1..], [None, None]);
        mark(&db, &s[0], "skipped", None).await.unwrap();
        let s = steps(&db, lead.id).await;
        assert_eq!((s[0].done_at, days(&s)[1]), (None, Some(0)));

        mark(&db, &s[1], "done", None).await.unwrap();
        assert_eq!(days(&steps(&db, lead.id).await)[2], Some(4));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_linkedin_reply_stops_the_emails_and_the_rest_of_the_sequence(db: PgPool) {
        let (lead, s) = sequence(&db).await;
        mark(&db, &s[0], "done", None).await.unwrap();
        for (kind, status) in [("initial", "sent"), ("follow_up", "approved"), ("manual_reply", "draft")] {
            sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'outbound', $3, $4)")
                .bind(lead.project_id)
                .bind(lead.id)
                .bind(kind)
                .bind(status)
                .execute(&db)
                .await
                .unwrap();
        }

        assert_eq!(stop_for_reply(&db, &lead, "They replied on LinkedIn").await.unwrap().status, "replied");
        let statuses: Vec<String> = steps(&db, lead.id).await.into_iter().map(|m| m.status).collect();
        assert_eq!(statuses, ["done", "skipped", "skipped"]);
        let emails: Vec<(String, String)> = sqlx::query_as("select kind, status from email_messages where lead_id = $1 order by kind")
            .bind(lead.id)
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(emails, [("follow_up".into(), "cancelled".into()), ("initial".into(), "sent".into()), ("manual_reply".into(), "draft".into())]);
    }
}
