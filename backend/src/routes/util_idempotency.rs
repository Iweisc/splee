//! Idempotency-Key on the public API's writes, so a client that retries a POST after a timeout doesn't create a second campaign
//! or send a reply twice. The first request claims (org, key) with an insert before it runs, so a concurrent duplicate can't run
//! too. Its response is kept for a day and replayed to a retry with the same method, path and body.

use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::attachments::UPLOAD_BODY_LIMIT;
use crate::error::{AppError, AppResult};

const HEADER: &str = "idempotency-key";
/// Larger responses aren't kept, and their key is released so a retry runs again.
const MAX_STORED_BODY: usize = 1024 * 1024;
const RUNNING: &str = "A request with this Idempotency-Key is still running. Retry once it finishes";

/// Runs a /v1 request under its Idempotency-Key. Reads, and writes without a key, go straight through.
pub(super) async fn run(db: &PgPool, org_id: Uuid, req: Request, next: Next) -> Response {
    let write = matches!(*req.method(), Method::POST | Method::PATCH | Method::DELETE);
    match req.headers().get(HEADER).cloned() {
        Some(key) if write => once(db, org_id, &key, req, next).await.unwrap_or_else(IntoResponse::into_response),
        _ => next.run(req).await,
    }
}

#[derive(FromRow, PartialEq)]
struct Fingerprint {
    method: String,
    path: String,
    body_hash: String,
}

#[derive(FromRow)]
struct Stored {
    #[sqlx(flatten)]
    request: Fingerprint,
    status: Option<i32>,
    content_type: Option<String>,
    response_body: Option<Vec<u8>>,
}

async fn once(db: &PgPool, org_id: Uuid, key: &HeaderValue, req: Request, next: Next) -> AppResult<Response> {
    let key = valid(key)?;
    let (parts, body) = req.into_parts();
    // The route's own body limit still applies when its handler reads this. This one only keeps a huge body out of memory here.
    let body = to_bytes(body, UPLOAD_BODY_LIMIT).await.map_err(|_| AppError::BadRequest("The request body is too large or was cut off".into()))?;
    let request = Fingerprint { method: parts.method.to_string(), path: parts.uri.path().to_string(), body_hash: hex::encode(Sha256::digest(&body)) };
    let Some(claim) = Claim::take(db, org_id, key, &request).await? else {
        return earlier(db, org_id, key, &request).await;
    };
    let res = next.run(Request::from_parts(parts, Body::from(body))).await;
    let status = res.status();
    // A 5xx, 409 or 429 says nothing about what a retry would get, so the retry runs instead of seeing it again.
    let kept = status.is_success() || (status.is_client_error() && status != StatusCode::CONFLICT && status != StatusCode::TOO_MANY_REQUESTS);
    if !kept {
        claim.release().await;
        return Ok(res);
    }
    let (parts, body) = res.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(e) => {
            claim.release().await;
            return Err(AppError::Internal(anyhow::anyhow!("could not read a response to keep: {e}")));
        }
    };
    if bytes.len() > MAX_STORED_BODY {
        claim.release().await;
    } else {
        let content_type = parts.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok());
        claim.keep(status, content_type, &bytes).await;
    }
    Ok(Response::from_parts(parts, Body::from(bytes)))
}

fn valid(key: &HeaderValue) -> AppResult<&str> {
    match key.to_str() {
        Ok(k) if (1..=255).contains(&k.len()) && k.bytes().all(|b| (b' '..=b'~').contains(&b)) => Ok(k),
        _ => Err(AppError::BadRequest("Idempotency-Key must be 1 to 255 printable ASCII characters".into())),
    }
}

/// Another request has this key: replay its response, or refuse.
async fn earlier(db: &PgPool, org_id: Uuid, key: &str, request: &Fingerprint) -> AppResult<Response> {
    let stored = sqlx::query_as::<_, Stored>(
        "select method, path, body_hash, status, content_type, response_body from idempotency_keys where org_id = $1 and key = $2",
    )
    .bind(org_id)
    .bind(key)
    .fetch_optional(db)
    .await?;
    // None when the first request failed and released the key between our insert and this read. A retry runs.
    let Some(stored) = stored else { return Err(AppError::Conflict(RUNNING.into())) };
    if stored.request != *request {
        return Err(AppError::Unprocessable("This Idempotency-Key was used for a different request. Send a new key with each new request".into()));
    }
    let (Some(status), Some(body)) = (stored.status, stored.response_body) else { return Err(AppError::Conflict(RUNNING.into())) };
    let mut res = Response::new(Body::from(body));
    *res.status_mut() = StatusCode::from_u16(status as u16).map_err(anyhow::Error::from)?;
    if let Some(content_type) = stored.content_type.and_then(|v| HeaderValue::from_str(&v).ok()) {
        res.headers_mut().insert(header::CONTENT_TYPE, content_type);
    }
    res.headers_mut().insert("idempotent-replayed", HeaderValue::from_static("true"));
    Ok(res)
}

/// A key this request holds until it keeps its response or releases it. When axum drops the request, as when the client hangs
/// up, the drop releases the key in the background so a retry runs. Every query names the claim's token, so a release that
/// lands late can't delete the claim of a retry that took the key in between.
struct Claim {
    db: PgPool,
    org_id: Uuid,
    key: String,
    token: Uuid,
    held: bool,
}

impl Claim {
    async fn take(db: &PgPool, org_id: Uuid, key: &str, request: &Fingerprint) -> sqlx::Result<Option<Claim>> {
        let token = Uuid::new_v4();
        // The insert runs in its own task and hands back the Claim itself, so a request dropped at any point can't leave a claim
        // nobody releases: a Claim that never reaches the request is dropped, and dropping it releases the key. An expired row is
        // taken over, since the cleanup worker only gets to them now and then.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (db_task, key_task, request) = (db.clone(), key.to_string(), (request.method.clone(), request.path.clone(), request.body_hash.clone()));
        tokio::spawn(async move {
            let claimed: sqlx::Result<Option<bool>> = sqlx::query_scalar(
                "insert into idempotency_keys (org_id, key, method, path, body_hash, token, expires_at)
                 values ($1, $2, $3, $4, $5, $6, now() + interval '24 hours')
                 on conflict (org_id, key) do update set method = excluded.method, path = excluded.path, body_hash = excluded.body_hash,
                     token = excluded.token, status = null, content_type = null, response_body = null, created_at = now(),
                     expires_at = excluded.expires_at
                 where idempotency_keys.expires_at <= now()
                 returning true",
            )
            .bind(org_id)
            .bind(&key_task)
            .bind(&request.0)
            .bind(&request.1)
            .bind(&request.2)
            .bind(token)
            .fetch_optional(&db_task)
            .await;
            let _ = tx.send(claimed.map(|c| c.map(|_| Claim { db: db_task, org_id, key: key_task, token, held: true })));
        });
        rx.await.map_err(|_| sqlx::Error::WorkerCrashed)?
    }

    async fn keep(mut self, status: StatusCode, content_type: Option<&str>, body: &[u8]) {
        let kept = sqlx::query(
            "update idempotency_keys set status = $4, content_type = $5, response_body = $6 where org_id = $1 and key = $2 and token = $3",
        )
        .bind(self.org_id)
        .bind(&self.key)
        .bind(self.token)
        .bind(i32::from(status.as_u16()))
        .bind(content_type)
        .bind(body)
        .execute(&self.db)
        .await;
        match kept {
            Ok(_) => self.held = false,
            Err(e) => {
                tracing::error!(error = ?e, "could not keep a response for its idempotency key");
                self.release().await;
            }
        }
    }

    async fn release(mut self) {
        release(&self.db, self.org_id, &self.key, self.token).await;
        self.held = false;
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.held {
            let (db, org_id, key, token) = (self.db.clone(), self.org_id, std::mem::take(&mut self.key), self.token);
            tokio::spawn(async move { release(&db, org_id, &key, token).await });
        }
    }
}

async fn release(db: &PgPool, org_id: Uuid, key: &str, token: Uuid) {
    let deleted = sqlx::query("delete from idempotency_keys where org_id = $1 and key = $2 and token = $3 and status is null")
        .bind(org_id)
        .bind(key)
        .bind(token)
        .execute(db)
        .await;
    if let Err(e) = deleted {
        tracing::error!(error = ?e, "could not release an idempotency key");
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::valid;

    #[test]
    fn keys_are_1_to_255_printable_ascii_characters() {
        for ok in ["a", "order-42 retry", &"k".repeat(255), "~!@#$%^&*()"] {
            assert_eq!(valid(&HeaderValue::from_str(ok).unwrap()).unwrap(), ok);
        }
        for bad in ["", &"k".repeat(256), "tab\there"] {
            assert!(valid(&HeaderValue::from_str(bad).unwrap()).is_err(), "{bad:?}");
        }
        assert!(valid(&HeaderValue::from_bytes("café".as_bytes()).unwrap()).is_err());
    }
}
