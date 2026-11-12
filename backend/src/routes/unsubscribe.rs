use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::error::{AppError, AppResult};
use crate::mail;
use crate::models::Lead;
use crate::state::AppState;

// GET only looks the address up, because mail security scanners open links in a real browser and would unsubscribe people
// who never clicked. POST unsubscribes: the page's button, and the RFC 8058 one-click POST mail clients send to List-Unsubscribe.
pub fn router() -> Router<AppState> {
    Router::new().route("/unsubscribe/{token}", get(show).post(unsubscribe))
}

async fn find(db: &PgPool, token: &str) -> AppResult<Lead> {
    sqlx::query_as::<_, Lead>("select * from leads where unsubscribe_token = $1")
        .bind(token)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// language is the one their last cold email was written in, so the page reads like the email that linked to it.
async fn show(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Value>> {
    let lead = find(&state.db, &token).await?;
    let language: Option<String> = sqlx::query_scalar(
        "select language from email_messages where lead_id = $1 and kind in ('initial', 'follow_up') and status = 'sent'
         order by sent_at desc limit 1",
    )
    .bind(lead.id)
    .fetch_optional(&state.db)
    .await?;
    let language = crate::language::or_english(language.as_deref().unwrap_or_default()).code;
    Ok(Json(json!({"email": lead.email, "unsubscribed": lead.status == "unsubscribed", "language": language})))
}

async fn unsubscribe(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Value>> {
    let lead = find(&state.db, &token).await?;
    let org_id: Option<uuid::Uuid> = sqlx::query_scalar("select org_id from projects where id = $1").bind(lead.project_id).fetch_one(&state.db).await?;
    mail::suppress_lead(&state, &lead, org_id, "unsubscribed", "unsubscribe").await?;
    Ok(Json(json!({"ok": true, "email": lead.email})))
}

#[cfg(test)]
mod tests {
    use axum::extract::{Path, State};
    use sqlx::PgPool;
    use uuid::Uuid;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_page_is_in_the_language_of_their_last_cold_email(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(&db).await.unwrap();
        let lead = |token: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, email, unsubscribe_token) values ($1, $2 || '@x.example', $2) returning id")
                .bind(project)
                .bind(token)
        };
        let anna = lead("anna").fetch_one(&db).await.unwrap();
        lead("dana").fetch_one(&db).await.unwrap();
        // A German first email, then a French follow-up, then a reply typed in the inbox, which has no footer and doesn't count.
        for (kind, language, days_ago) in [("initial", "de", 3), ("follow_up", "fr", 2), ("manual_reply", "es", 1)] {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, direction, kind, status, sent_at, language)
                 values ($1, $2, 'outbound', $3, 'sent', now() - make_interval(days => $5), $4)",
            )
            .bind(project)
            .bind(anna)
            .bind(kind)
            .bind(language)
            .bind(days_ago)
            .execute(&db)
            .await
            .unwrap();
        }
        let state = crate::autopilot::tests::state(db.clone());
        let page = |token: &'static str| super::show(State(state.clone()), Path(token.to_string()));
        assert_eq!(page("anna").await.unwrap().0["language"], "fr");
        // Never emailed, or emailed before emails had a language: English.
        assert_eq!(page("dana").await.unwrap().0["language"], "en");
    }
}
