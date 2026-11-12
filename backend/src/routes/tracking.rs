//! Email tracking's public routes, which the frontend serves at /c/{token} and /o/{token}.gif on PUBLIC_URL and every
//! tracking host, the check a reverse proxy asks before it gets a certificate for a tracking host, and the Domains page's
//! tracking hosts. See crate::tracking.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, LOCATION, USER_AGENT};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::demo::client_ip;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::state::AppState;
use crate::tracking::{self, Hit};
use crate::visitors::{self, Kind};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/t/c/{token}", get(click))
        .route("/t/o/{file}", get(open))
        .route("/t/allowed", get(allowed))
        .route("/tracking-domains", get(list))
        .route("/tracking-domains/check", post(check))
}

/// Hits a minute from one address that get stored. Past it the redirect still works, so a person never sees an error.
const PER_IP: u32 = 120;

fn hit(state: &AppState, headers: &HeaderMap, peer: SocketAddr, method: &Method, kind: &'static str, email_id: Uuid, link_token: Option<String>) -> Hit {
    let ip: Option<IpAddr> = client_ip(headers, peer).parse().ok();
    Hit {
        kind,
        email_id,
        link_token,
        head: method == Method::HEAD,
        user_agent: headers.get(USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string(),
        ip,
        ip_hash: ip.map(|ip| visitors::ip_hash(&state.cfg.app_secret, ip)).unwrap_or_default(),
        at: Utc::now(),
    }
}

/// Stores the hit in the background, so the redirect or pixel never waits on the network lookup.
fn keep(state: &AppState, hit: Hit) {
    if !visitors::allow(&format!("track:{}", hit.ip_hash), PER_IP) {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let result = async {
            let Some(id) = tracking::begin(&state.db, &hit).await? else { return anyhow::Ok(()) };
            let network = match hit.ip {
                Some(ip) => visitors::network_kind(&state, ip).await.unwrap_or(Kind::Unknown),
                None => Kind::Unknown,
            };
            tracking::finish(&state.db, id, &hit, network).await?;
            // A click up to BURST after this one has been stored by now, if only as pending.
            let settled = hit.at + tracking::BURST + std::time::Duration::from_secs(1);
            if let Ok(wait) = (settled - Utc::now()).to_std() {
                tokio::time::sleep(wait).await;
            }
            tracking::burst(&state.db, id).await?;
            tracking::announce(&state, id).await
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error = ?e, "couldn't record an email open or click");
        }
    });
}

/// A 302 to the link's stored address. Nothing in the request can change where it goes.
async fn click(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    method: Method,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let link: Option<(Uuid, String)> = sqlx::query_as("select email_id, url from tracked_links where token = $1").bind(&token).fetch_optional(&state.db).await?;
    let (email_id, url) = link.ok_or(AppError::NotFound)?;
    let location = HeaderValue::from_str(&url).map_err(|_| AppError::NotFound)?;
    keep(&state, hit(&state, &headers, peer, &method, "click", email_id, Some(token)));
    Ok((StatusCode::FOUND, [(LOCATION, location), (CACHE_CONTROL, HeaderValue::from_static("no-store"))]).into_response())
}

async fn open(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    method: Method,
    Path(file): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let token = file.strip_suffix(".gif").ok_or(AppError::NotFound)?;
    let email_id: Uuid = sqlx::query_scalar("select id from email_messages where open_token = $1").bind(token).fetch_optional(&state.db).await?.ok_or(AppError::NotFound)?;
    keep(&state, hit(&state, &headers, peer, &method, "open", email_id, None));
    let headers = [(CONTENT_TYPE, "image/gif"), (CACHE_CONTROL, "no-store, no-cache, must-revalidate, private")];
    Ok((headers, tracking::PIXEL).into_response())
}

#[derive(Deserialize)]
struct AllowedQuery {
    domain: String,
}

/// For a reverse proxy that gets certificates on demand, like Caddy's on_demand_tls ask: 200 for a verified tracking host.
async fn allowed(State(state): State<AppState>, Query(q): Query<AllowedQuery>) -> AppResult<StatusCode> {
    let known: bool = sqlx::query_scalar("select exists (select 1 from tracking_domains where host = lower($1) and verified_at is not null)")
        .bind(q.domain.trim().trim_end_matches('.'))
        .fetch_one(&state.db)
        .await?;
    Ok(if known { StatusCode::OK } else { StatusCode::NOT_FOUND })
}

#[derive(Serialize, FromRow, Debug)]
pub struct TrackingDomain {
    domain: String,
    host: String,
    verified_at: Option<Ts>,
    checked_at: Option<Ts>,
    error: String,
    /// Bought through Porkbun, so Splee set the CNAME itself.
    porkbun: bool,
}

#[derive(Serialize, Debug)]
pub struct TrackingDomains {
    /// What each tracking host's CNAME points to. None while PUBLIC_URL is a local address DNS can't point at.
    target: Option<String>,
    domains: Vec<TrackingDomain>,
}

/// The org's mailbox and bought domains, each with its tracking host and how its last check went.
async fn domains(db: &PgPool, org_id: Uuid) -> sqlx::Result<Vec<TrackingDomain>> {
    let rows: Vec<TrackingDomain> = sqlx::query_as(
        "with d as (
           select lower(split_part(email::text, '@', 2)) as domain from mailboxes where org_id = $1
           union select lower(domain) from sending_domains where org_id = $1 and status = 'registered')
         select d.domain, 'track.' || d.domain as host, t.verified_at, t.checked_at, coalesce(t.error, '') as error,
           exists (select 1 from sending_domains s where s.org_id = $1 and lower(s.domain) = d.domain and s.status = 'registered') as porkbun
         from d left join tracking_domains t on t.org_id = $1 and t.domain = d.domain
         order by d.domain",
    )
    .bind(org_id)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().filter(|d| tracking::own_domain(&d.domain)).collect())
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<TrackingDomains>> {
    Ok(Json(TrackingDomains { target: tracking::cname_target(&state.cfg.public_url), domains: domains(&state.db, auth.org_id).await? }))
}

#[derive(Deserialize)]
struct CheckBody {
    domain: String,
}

async fn check(State(state): State<AppState>, auth: Auth, Json(body): Json<CheckBody>) -> AppResult<Json<TrackingDomains>> {
    let target = tracking::cname_target(&state.cfg.public_url)
        .ok_or_else(|| AppError::BadRequest("Set PUBLIC_URL to the address people reach Splee at, then check again".into()))?;
    let domain = body.domain.trim().to_lowercase();
    if !domains(&state.db, auth.org_id).await?.iter().any(|d| d.domain == domain) {
        return Err(AppError::NotFound);
    }
    let resolver = crate::mail::dns::resolver().map_err(AppError::Internal)?;
    tracking::check_and_save(&state.db, &resolver, auth.org_id, &domain, &target).await?;
    Ok(Json(TrackingDomains { target: Some(target), domains: domains(&state.db, auth.org_id).await? }))
}

#[cfg(test)]
mod tests {
    use axum::extract::{Path, Query, State};
    use axum::{Json, Router};
    use reqwest::StatusCode;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;
    use crate::auth::ApiKeyAuth;

    async fn serve(state: AppState) -> String {
        let app = Router::new().nest("/api", router()).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap() });
        base
    }

    /// An organization with one email sent an hour ago, holding one tracked link and a pixel. Returns (org, project, email).
    async fn sent_email(db: &PgPool, name: &str) -> (Uuid, Uuid, Uuid) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(db).await.unwrap();
        let email: Uuid = sqlx::query_scalar(
            "with l as (insert into leads (project_id, full_name, email) values ($1, 'Sam Lee', 'sam@lee.example') returning id)
             insert into email_messages (project_id, lead_id, direction, kind, status, sent_at, track_clicks, track_opens, open_token)
             select $1, id, 'outbound', 'initial', 'sent', now() - interval '1 hour', true, true, $2 || '-pixel' from l returning id",
        )
        .bind(project)
        .bind(name)
        .fetch_one(db)
        .await
        .unwrap();
        sqlx::query("insert into tracked_links (token, email_id, position, url) values ($1 || '-link', $2, 0, 'https://acme.example/pricing?ref=email')")
            .bind(name)
            .bind(email)
            .execute(db)
            .await
            .unwrap();
        (org, project, email)
    }

    /// Waits for the background task to store `n` hits and finish their network lookups.
    async fn hits(db: &PgPool, n: i64) -> Vec<(String, Option<String>)> {
        for _ in 0..100 {
            let rows: Vec<(String, Option<String>)> =
                sqlx::query_as("select kind, filter_reason from tracking_hits order by created_at").fetch_all(db).await.unwrap();
            if rows.len() as i64 >= n && rows.iter().all(|(_, reason)| reason.as_deref() != Some("pending")) {
                return rows;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("no {n} hits");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn clicks_redirect_only_to_the_stored_link_and_the_pixel_is_a_gif(db: PgPool) {
        let base = serve(crate::autopilot::tests::state(db.clone())).await;
        sent_email(&db, "acme").await;
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
        let browser = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15";

        // Whatever the request adds, it goes to the stored link.
        let res = client.get(format!("{base}/t/c/acme-link?url=https://evil.example/&next=https://evil.example/")).header("user-agent", browser).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::FOUND);
        assert_eq!(res.headers()["location"], "https://acme.example/pricing?ref=email");
        for path in ["t/c/nope", "t/c/acme-pixel", "t/o/nope.gif", "t/o/acme-pixel", "t/o/acme-link.gif"] {
            assert_eq!(client.get(format!("{base}/{path}")).send().await.unwrap().status(), StatusCode::NOT_FOUND, "{path}");
        }

        let res = client.get(format!("{base}/t/o/acme-pixel.gif")).header("user-agent", browser).send().await.unwrap();
        assert_eq!((res.status(), res.headers()["content-type"].to_str().unwrap()), (StatusCode::OK, "image/gif"));
        assert!(res.headers()["cache-control"].to_str().unwrap().contains("no-store"));
        assert_eq!(res.bytes().await.unwrap().as_ref(), crate::tracking::PIXEL);

        // A link checker's HEAD still gets the redirect, and is stored as filtered.
        let res = client.head(format!("{base}/t/c/acme-link")).header("user-agent", browser).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::FOUND);
        let mut stored = hits(&db, 3).await;
        stored.sort();
        assert_eq!(stored, [("click".into(), None), ("click".into(), Some("head_request".into())), ("open".into(), None)]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn only_verified_tracking_hosts_get_certificates(db: PgPool) {
        let state = crate::autopilot::tests::state(db.clone());
        let (org, _, _) = sent_email(&db, "acme").await;
        sqlx::query("insert into tracking_domains (org_id, domain, host, verified_at) values ($1, 'getacme.example', 'track.getacme.example', now()),
                     ($1, 'tryacme.example', 'track.tryacme.example', null)")
            .bind(org)
            .execute(&db)
            .await
            .unwrap();
        let ask = |domain: &str| allowed(State(state.clone()), Query(AllowedQuery { domain: domain.into() }));
        assert_eq!(ask("Track.GetAcme.example.").await.unwrap(), StatusCode::OK);
        assert_eq!(ask("track.tryacme.example").await.unwrap(), StatusCode::NOT_FOUND);
        assert_eq!(ask("getacme.example").await.unwrap(), StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_api_lists_counted_events_of_the_callers_organization_only(db: PgPool) {
        let state = crate::autopilot::tests::state(db.clone());
        let (ours, project, email) = sent_email(&db, "acme").await;
        let (theirs, their_project, their_email) = sent_email(&db, "globex").await;
        for (project, email, link, reason) in
            [(project, email, Some("acme-link"), None), (project, email, None, Some("scanner_agent")), (their_project, their_email, None, None)]
        {
            sqlx::query(
                "insert into tracking_hits (project_id, email_id, lead_id, kind, link_token, filter_reason)
                 select $1, $2, lead_id, case when $3::text is null then 'open' else 'click' end, $3, $4 from email_messages where id = $2",
            )
            .bind(project)
            .bind(email)
            .bind(link)
            .bind(reason)
            .execute(&db)
            .await
            .unwrap();
        }
        let list = |org: Uuid, project: Uuid| {
            super::super::api_v1::email_events(State(state.clone()), ApiKeyAuth { org_id: org }, Path(project), Query(serde_json::from_str("{}").unwrap()))
        };
        let Json(events) = list(ours, project).await.unwrap();
        let events = serde_json::to_value(events).unwrap();
        assert_eq!(events.as_array().unwrap().len(), 1);
        assert_eq!((events[0]["event"].as_str(), events[0]["url"].as_str()), (Some("email.clicked"), Some("https://acme.example/pricing?ref=email")));
        assert!(matches!(list(theirs, project).await, Err(AppError::NotFound)));
        assert_eq!(list(theirs, their_project).await.unwrap().0.len(), 1);
    }
}
