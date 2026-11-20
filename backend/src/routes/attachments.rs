//! Uploading files to send with a reply and downloading the files on a thread's emails. See mail/attachments.rs.

use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::mail::attachments::{self, MAX_FILE_BYTES, MAX_FILES};
use crate::models::{EmailAttachment, Mailbox};
use crate::state::AppState;

/// The multipart form around a 25 MB file needs a little more.
pub(super) const UPLOAD_BODY_LIMIT: usize = MAX_FILE_BYTES as usize + 1024 * 1024;
/// Uploads waiting for a reply in one organization. They go after a day, so this only stops a script filling the database.
const MAX_UNUSED: i64 = 100;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/attachments", post(upload).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)))
        .route("/attachments/{id}", get(download))
}

async fn upload(State(state): State<AppState>, auth: Auth, form: Multipart) -> AppResult<Json<EmailAttachment>> {
    Ok(Json(upload_to(&state.db, auth.org_id, form).await?))
}

async fn download(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Response> {
    download_from(&state.db, auth.org_id, id).await
}

/// Keeps the form's `file` for the organization until a reply takes it.
pub(super) async fn upload_to(db: &sqlx::PgPool, org_id: Uuid, mut form: Multipart) -> AppResult<EmailAttachment> {
    let bad = |e: axum::extract::multipart::MultipartError| AppError::BadRequest(e.body_text());
    let mut file = None;
    while let Some(field) = form.next_field().await.map_err(bad)? {
        if field.name() == Some("file") {
            let name = attachments::clean_filename(field.file_name().unwrap_or_default(), "");
            file = Some((name, field.bytes().await.map_err(bad)?));
        }
    }
    let Some((filename, data)) = file else { return Err(AppError::BadRequest("Send the file as the form's file field".into())) };
    if filename.is_empty() || data.is_empty() {
        return Err(AppError::BadRequest("That file is empty".into()));
    }
    if data.len() as u64 > MAX_FILE_BYTES {
        return Err(AppError::BadRequest(format!("{filename} is over 25 MB. Share a link to it instead")));
    }
    let content_type = attachments::upload_type(&filename, &data).map_err(AppError::BadRequest)?;
    let unused: i64 = sqlx::query_scalar("select count(*) from email_attachments where org_id = $1 and message_id is null")
        .bind(org_id)
        .fetch_one(db)
        .await?;
    if unused >= MAX_UNUSED {
        return Err(AppError::TooManyRequests("Too many files are waiting to be sent. Send your replies or try again tomorrow".into()));
    }
    let mut tx = db.begin().await?;
    let attachment = sqlx::query_as::<_, EmailAttachment>(
        "insert into email_attachments (org_id, filename, content_type, size_bytes) values ($1, $2, $3, $4) returning *",
    )
    .bind(org_id)
    .bind(&filename)
    .bind(content_type)
    .bind(data.len() as i64)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("insert into email_attachment_data (attachment_id, data) values ($1, $2)").bind(attachment.id).bind(data.as_ref()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(attachment)
}

/// The file as a download, whoever in the organization asks. Only images and PDFs keep their type, and nosniff stops a
/// browser guessing another.
pub(super) async fn download_from(db: &sqlx::PgPool, org_id: Uuid, id: Uuid) -> AppResult<Response> {
    let (filename, content_type, data): (String, String, Vec<u8>) = sqlx::query_as(
        "select a.filename, a.content_type, d.data from email_attachments a join email_attachment_data d on d.attachment_id = a.id
         where a.id = $1 and a.org_id = $2",
    )
    .bind(id)
    .bind(org_id)
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound)?;
    let headers = [
        (header::CONTENT_TYPE, attachments::served_type(&content_type).to_string()),
        (header::CONTENT_DISPOSITION, attachments::disposition(&filename)),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
    ];
    Ok((headers, data).into_response())
}

/// Puts the organization's uploads `ids` on `message_id`, after any files it has, within what `mailbox` sends in one email.
pub(super) async fn take(tx: &mut sqlx::PgConnection, org_id: Uuid, message_id: Uuid, ids: &[Uuid], mailbox: Option<&Mailbox>) -> AppResult<()> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    let sizes: Vec<i64> = sqlx::query_scalar("select size_bytes from email_attachments where id = any($1) and org_id = $2 and message_id is null for update")
        .bind(&ids)
        .bind(org_id)
        .fetch_all(&mut *tx)
        .await?;
    if sizes.len() != ids.len() {
        return Err(AppError::BadRequest("One of the files is gone or already sent. Attach it again".into()));
    }
    let (had, had_bytes): (i64, i64) =
        sqlx::query_as("select count(*), coalesce(sum(size_bytes), 0)::bigint from email_attachments where message_id = $1")
            .bind(message_id)
            .fetch_one(&mut *tx)
            .await?;
    if had as usize + ids.len() > MAX_FILES {
        return Err(AppError::BadRequest(format!("Attach at most {MAX_FILES} files to a reply")));
    }
    let total = (had_bytes + sizes.iter().sum::<i64>()) as u64;
    let (budget, sender) = attachments::files_budget(mailbox);
    if total > budget {
        return Err(AppError::BadRequest(format!(
            "These files come to {}, and {sender} at most {} of files in one email. Remove some or share a link instead",
            attachments::size_label(total),
            attachments::size_label(budget),
        )));
    }
    sqlx::query("update email_attachments set message_id = $1 where id = any($2)").bind(message_id).bind(&ids).execute(&mut *tx).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::http::header;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::download_from;
    use crate::error::AppError;

    async fn org(db: &PgPool, name: &str) -> Uuid {
        sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap()
    }

    async fn file(db: &PgPool, org: Uuid, filename: &str, content_type: &str, data: &[u8]) -> Uuid {
        let id: Uuid = sqlx::query_scalar(
            "insert into email_attachments (org_id, filename, content_type, size_bytes) values ($1, $2, $3, $4) returning id",
        )
        .bind(org)
        .bind(filename)
        .bind(content_type)
        .bind(data.len() as i64)
        .fetch_one(db)
        .await
        .unwrap();
        sqlx::query("insert into email_attachment_data (attachment_id, data) values ($1, $2)").bind(id).bind(data).execute(db).await.unwrap();
        id
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_file_downloads_only_for_its_organization_and_never_as_a_page(db: PgPool) {
        let (acme, other) = (org(&db, "Acme").await, org(&db, "Other").await);
        let pdf = file(&db, acme, "Q3 brief.pdf", "application/pdf", b"%PDF-1.4").await;
        let page = file(&db, acme, "invite.html", "text/html", b"<script>alert(1)</script>").await;

        assert!(matches!(download_from(&db, other, pdf).await, Err(AppError::NotFound)));
        assert!(matches!(download_from(&db, acme, Uuid::new_v4()).await, Err(AppError::NotFound)));

        let res = download_from(&db, acme, pdf).await.unwrap();
        let headers = res.headers();
        assert_eq!(headers[header::CONTENT_TYPE], "application/pdf");
        assert_eq!(headers[header::CONTENT_DISPOSITION], "attachment; filename=\"Q3 brief.pdf\"; filename*=UTF-8''Q3%20brief.pdf");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"%PDF-1.4");

        let res = download_from(&db, acme, page).await.unwrap();
        assert_eq!(res.headers()[header::CONTENT_TYPE], "application/octet-stream");

        // A lead's file Splee didn't keep is listed but has nothing to download.
        let big: Uuid = sqlx::query_scalar(
            "insert into email_attachments (org_id, filename, content_type, size_bytes, stored) values ($1, 'video.mov', 'video/quicktime', 30000000, false)
             returning id",
        )
        .bind(acme)
        .fetch_one(&db)
        .await
        .unwrap();
        assert!(matches!(download_from(&db, acme, big).await, Err(AppError::NotFound)));
    }
}
