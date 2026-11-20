//! Files on emails: the ones a lead attaches to a reply, kept when the poller reads it, and the ones someone uploads to send
//! with theirs. The bytes live in email_attachment_data, apart from the list, so loading a thread never reads them.

use std::collections::HashMap;

use mail_parser::{Message, MimeHeaders};
use uuid::Uuid;

use crate::models::{EmailAttachment, Mailbox};

const MB: u64 = 1024 * 1024;
/// The largest file kept from a lead or uploaded to send. A lead's bigger file is listed but not kept.
pub const MAX_FILE_BYTES: u64 = 25 * MB;
pub const MAX_FILES: usize = 10;

/// What a reply can carry: the extension, the type it's sent as, and how its first bytes start. Office files are zips. The
/// text types only have to be text.
const ALLOWED: &[(&str, &str, &[u8])] = &[
    ("pdf", "application/pdf", b"%PDF"),
    ("png", "image/png", b"\x89PNG"),
    ("jpg", "image/jpeg", b"\xFF\xD8\xFF"),
    ("jpeg", "image/jpeg", b"\xFF\xD8\xFF"),
    ("gif", "image/gif", b"GIF8"),
    ("webp", "image/webp", b"RIFF"),
    ("docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", b"PK\x03\x04"),
    ("pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation", b"PK\x03\x04"),
    ("xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", b"PK\x03\x04"),
    ("txt", "text/plain", b""),
    ("csv", "text/csv", b""),
];

pub const ALLOWED_NAMES: &str = "PDF, PNG, JPEG, GIF, WebP, DOCX, PPTX, XLSX, TXT or CSV";

/// Types a browser may show as what they are. Anything else, a lead's HTML or SVG included, downloads as
/// application/octet-stream so it can't run as a page on Splee's origin.
const SAFE_TO_SHOW: &[&str] = &["application/pdf", "image/png", "image/jpeg", "image/gif", "image/webp"];

pub fn served_type(content_type: &str) -> &str {
    if SAFE_TO_SHOW.contains(&content_type) { content_type } else { "application/octet-stream" }
}

/// The type an upload is sent as, from its extension, when its bytes look like that type too.
pub fn upload_type(filename: &str, data: &[u8]) -> Result<&'static str, String> {
    let ext = filename.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    let Some((_, content_type, magic)) = ALLOWED.iter().find(|(e, _, _)| *e == ext) else {
        return Err(format!("Splee can send {ALLOWED_NAMES} files"));
    };
    let looks_right = if magic.is_empty() { !data.contains(&0) } else { data.starts_with(magic) && (ext != "webp" || data.get(8..12) == Some(b"WEBP")) };
    if !looks_right {
        return Err(format!("{filename} isn't a real {} file", ext.to_uppercase()));
    }
    Ok(content_type)
}

/// A name that's safe to show and to put in a header: no folders, no control characters, at most 200 characters.
pub fn clean_filename(name: &str, fallback: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
    let clean = clean.trim().trim_start_matches('.').trim();
    if clean.is_empty() { fallback.to_string() } else { clean.to_string() }
}

/// Content-Disposition for a download. The quoted name is the ASCII fallback for old clients, filename* the real one.
pub fn disposition(filename: &str) -> String {
    let ascii: String = filename.chars().map(|c| if (c.is_ascii_graphic() && c != '"' && c != '\\') || c == ' ' { c } else { '_' }).collect();
    let encoded: String = filename
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// Most a mailbox sends in one email, which counts the files after base64, and who sends that, for the error.
fn message_limit(mb: Option<&Mailbox>) -> (u64, &'static str) {
    let host = mb.map(|m| m.smtp_host.trim().to_lowercase()).unwrap_or_default();
    if host.ends_with("gmail.com") || host.ends_with("googlemail.com") {
        (25 * MB, "Gmail sends")
    } else if host.ends_with("office365.com") {
        (35 * MB, "Microsoft 365 sends")
    } else if host.ends_with("outlook.com") {
        (20 * MB, "Outlook.com sends")
    } else {
        (25 * MB, "Most mail servers send")
    }
}

/// How many bytes of files fit in one email from `mb`: base64 makes each 57 bytes 78 with the line break, and the text and
/// headers get 256 KB.
pub fn files_budget(mb: Option<&Mailbox>) -> (u64, &'static str) {
    let (limit, name) = message_limit(mb);
    (limit * 57 / 78 - MB / 4, name)
}

pub fn size_label(bytes: u64) -> String {
    let mb = bytes as f64 / MB as f64;
    if mb >= 1.0 { format!("{mb:.1} MB") } else { format!("{} KB", bytes.div_ceil(1024)) }
}

/// A file a lead attached, as it came in.
pub struct Incoming<'a> {
    pub filename: String,
    pub content_type: String,
    pub inline: bool,
    pub data: &'a [u8],
}

/// The files attached to a reply, inline ones like a signature's logo included, since the text body Splee keeps doesn't show them.
pub fn incoming<'a>(msg: &'a Message<'a>) -> Vec<Incoming<'a>> {
    msg.attachments()
        .map(|part| {
            let content_type = part
                .content_type()
                .map(|c| match c.subtype() {
                    Some(sub) => format!("{}/{}", c.ctype(), sub),
                    None => c.ctype().to_string(),
                })
                .map(|t| t.to_lowercase())
                .unwrap_or_else(|| "application/octet-stream".into());
            let fallback = if part.is_message() { "message.eml" } else { "attachment" };
            let disposition = part.content_disposition();
            let inline = disposition.is_some_and(|d| d.is_inline()) || (disposition.is_none() && part.content_id().is_some());
            Incoming { filename: clean_filename(part.attachment_name().unwrap_or_default(), fallback), content_type, inline, data: part.contents() }
        })
        .collect()
}

/// Keeps a reply's files with it. One over MAX_FILE_BYTES is listed by name and size only.
pub async fn store_incoming(db: &sqlx::PgPool, org_id: Uuid, message_id: Uuid, files: &[Incoming<'_>]) -> sqlx::Result<()> {
    for file in files {
        let stored = file.data.len() as u64 <= MAX_FILE_BYTES;
        let mut tx = db.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into email_attachments (org_id, message_id, filename, content_type, size_bytes, inline, stored)
             values ($1, $2, $3, $4, $5, $6, $7) returning id",
        )
        .bind(org_id)
        .bind(message_id)
        .bind(&file.filename)
        .bind(&file.content_type)
        .bind(file.data.len() as i64)
        .bind(file.inline)
        .bind(stored)
        .fetch_one(&mut *tx)
        .await?;
        if stored {
            sqlx::query("insert into email_attachment_data (attachment_id, data) values ($1, $2)").bind(id).bind(file.data).execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

/// The files of each message, in the order they were added.
pub async fn for_messages(db: &sqlx::PgPool, message_ids: &[Uuid]) -> sqlx::Result<HashMap<Uuid, Vec<EmailAttachment>>> {
    let rows = sqlx::query_as::<_, EmailAttachment>("select * from email_attachments where message_id = any($1) order by created_at, filename")
        .bind(message_ids)
        .fetch_all(db)
        .await?;
    let mut out: HashMap<Uuid, Vec<EmailAttachment>> = HashMap::new();
    for a in rows {
        if let Some(m) = a.message_id {
            out.entry(m).or_default().push(a);
        }
    }
    Ok(out)
}

/// The files an email goes out with, bytes included.
pub async fn for_sending(db: &sqlx::PgPool, message_id: Uuid) -> sqlx::Result<Vec<(String, String, Vec<u8>)>> {
    sqlx::query_as(
        "select a.filename, a.content_type, d.data from email_attachments a join email_attachment_data d on d.attachment_id = a.id
         where a.message_id = $1 order by a.created_at, a.filename",
    )
    .bind(message_id)
    .fetch_all(db)
    .await
}

/// For the hourly cleanup: uploads no reply took within a day.
pub async fn delete_unused(db: &sqlx::PgPool) -> sqlx::Result<()> {
    sqlx::query("delete from email_attachments where message_id is null and created_at < now() - interval '1 day'").execute(db).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::{clean_filename, delete_unused, disposition, files_budget, served_type, upload_type};

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn only_uploads_no_reply_took_for_a_day_are_deleted(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id) values ($1) returning id").bind(project).fetch_one(&db).await.unwrap();
        let sent: Uuid = sqlx::query_scalar(
            "insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'outbound', 'manual_reply', 'sent') returning id",
        )
        .bind(project)
        .bind(lead)
        .fetch_one(&db)
        .await
        .unwrap();
        for (name, message, age) in [("old.pdf", None, "25 hours"), ("new.pdf", None, "1 hour"), ("sent.pdf", Some(sent), "30 days")] {
            sqlx::query(
                "insert into email_attachments (org_id, message_id, filename, content_type, size_bytes, created_at)
                 values ($1, $2, $3, 'application/pdf', 1, now() - $4::interval)",
            )
            .bind(org)
            .bind(message)
            .bind(name)
            .bind(age)
            .execute(&db)
            .await
            .unwrap();
        }
        delete_unused(&db).await.unwrap();
        let left: Vec<String> = sqlx::query_scalar("select filename from email_attachments order by filename").fetch_all(&db).await.unwrap();
        assert_eq!(left, ["new.pdf", "sent.pdf"]);
    }

    #[test]
    fn uploads_must_be_an_allowed_type_and_look_like_it() {
        assert_eq!(upload_type("Deck.PDF", b"%PDF-1.7 ..."), Ok("application/pdf"));
        assert_eq!(upload_type("notes.csv", b"name,email\n"), Ok("text/csv"));
        assert_eq!(upload_type("logo.webp", b"RIFF\0\0\0\0WEBPVP8 "), Ok("image/webp"));
        assert!(upload_type("tool.exe", b"MZ").unwrap_err().contains("PDF, PNG"));
        assert!(upload_type("fake.pdf", b"<html>").unwrap_err().contains("isn't a real PDF"));
        assert!(upload_type("binary.txt", b"a\0b").is_err());
        assert!(upload_type("noext", b"%PDF").is_err());
    }

    #[test]
    fn only_images_and_pdfs_keep_their_type_when_downloaded() {
        assert_eq!(served_type("application/pdf"), "application/pdf");
        assert_eq!(served_type("image/png"), "image/png");
        for risky in ["text/html", "image/svg+xml", "application/javascript", "text/plain"] {
            assert_eq!(served_type(risky), "application/octet-stream");
        }
    }

    #[test]
    fn filenames_lose_folders_and_control_characters() {
        assert_eq!(clean_filename("C:\\Users\\sam\\Q3 plan.pdf", "attachment"), "Q3 plan.pdf");
        assert_eq!(clean_filename("../../etc/passwd", "attachment"), "passwd");
        assert_eq!(clean_filename("bad\r\nname.txt", "attachment"), "badname.txt");
        assert_eq!(clean_filename("", "message.eml"), "message.eml");
        assert_eq!(disposition("Prix \"été\".pdf"), "attachment; filename=\"Prix __t__.pdf\"; filename*=UTF-8''Prix%20%22%C3%A9t%C3%A9%22.pdf");
    }

    #[test]
    fn a_25_mb_server_fits_about_18_mb_of_files_once_they_are_encoded() {
        let (budget, name) = files_budget(None);
        assert_eq!(name, "Most mail servers send");
        assert!(budget > 18 * 1024 * 1024 && budget < 19 * 1024 * 1024, "{budget}");
    }
}
