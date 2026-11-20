use std::collections::HashSet;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::imports::{self, Csv};
use super::util_input;
use super::util_pagination::{PageQuery, Paginated};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::SuppressionList;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/suppression-lists", get(list).post(upload))
        .route("/suppression-lists/columns", post(columns))
        .route("/suppression-lists/{id}", patch(rescope).delete(remove))
        .route("/suppression-lists/{id}/entries", get(entries))
        .layer(DefaultBodyLimit::max(20 * 1024 * 1024))
}

/// SQL that's true when one of org `org`'s lists covering project `project` has `email`, the email's domain or
/// `company_domain`. Each argument is an SQL expression of type text. It's one lookup on suppression_list_entries_org_value
/// however many lists the org has.
pub fn listed(org: &str, project: &str, email: &str, company_domain: &str) -> String {
    format!(
        "exists (select 1 from suppression_list_entries le join suppression_lists sl on sl.id = le.list_id
                 where le.org_id = {org} and le.value in (lower({email}), lower(split_part({email}, '@', 2)), lower({company_domain}))
                   and (sl.project_ids is null or {project} = any(sl.project_ids)))"
    )
}

/// Every value on the org's do-not-contact list and on its lists covering a project, lowercased, for imports that check
/// many leads at once. $1 is the org, $2 the project.
pub const SUPPRESSED_VALUES: &str = "select lower(value::text) from suppressions where org_id = $1
     union select le.value from suppression_list_entries le join suppression_lists sl on sl.id = le.list_id
     where le.org_id = $1 and (sl.project_ids is null or $2 = any(sl.project_ids))";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    People,
    Companies,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::People => "people",
            Kind::Companies => "companies",
        }
    }

    /// The value an entry of this kind keeps, or None when `raw` isn't one.
    fn parse(self, raw: &str) -> Option<String> {
        match self {
            Kind::People => util_input::email(raw),
            Kind::Companies => util_input::domain(raw),
        }
    }

    fn of(raw: &str) -> Option<Kind> {
        [Kind::People, Kind::Companies].into_iter().find(|k| k.parse(raw).is_some())
    }
}

const VIEW: &str = "select l.id, l.name::text as name, l.kind,
            case when l.project_ids is not null then array(select p.id from projects p where p.id = any(l.project_ids) order by p.created_at) end
              as project_ids,
            l.created_by, l.created_at, (select count(*) from suppression_list_entries e where e.list_id = l.id) as entries
     from suppression_lists l where l.org_id = $1 and ($2::uuid is null or l.id = $2) order by l.created_at desc";

/// The org's lists, newest first.
pub async fn all(db: &PgPool, org_id: Uuid) -> sqlx::Result<Vec<SuppressionList>> {
    sqlx::query_as::<_, SuppressionList>(VIEW).bind(org_id).bind(None::<Uuid>).fetch_all(db).await
}

async fn one(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<SuppressionList> {
    sqlx::query_as::<_, SuppressionList>(VIEW).bind(org_id).bind(id).fetch_optional(db).await?.ok_or(AppError::NotFound)
}

/// A scope a client sent: None for every project, else a non-empty set of the org's own projects.
async fn check_scope(db: &PgPool, org_id: Uuid, ids: Option<Vec<Uuid>>) -> AppResult<Option<Vec<Uuid>>> {
    let Some(mut ids) = ids else { return Ok(None) };
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Err(AppError::Unprocessable("Pick at least one project, or send null for all of them".into()));
    }
    let ours: i64 = sqlx::query_scalar("select count(*) from projects where org_id = $1 and id = any($2)").bind(org_id).bind(&ids).fetch_one(db).await?;
    if ours != ids.len() as i64 {
        return Err(AppError::Unprocessable("A project in project_ids isn't in your organization".into()));
    }
    Ok(Some(ids))
}

fn check_name(name: &str) -> AppResult<&str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("Name the list".into()));
    }
    if name.chars().count() > 100 {
        return Err(AppError::BadRequest("Keep the name under 100 characters".into()));
    }
    Ok(name)
}

/// Takes turns per org, so two uploads with the same name can't both pick the same free one.
async fn lock_names(conn: &mut PgConnection, org_id: Uuid) -> sqlx::Result<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('suppression-lists:' || $1::text, 0))").bind(org_id).execute(conn).await?;
    Ok(())
}

/// Makes a list named `name`, or "name (2)" and on when the org has one by that name, so an upload never merges into
/// another list.
async fn create(conn: &mut PgConnection, org_id: Uuid, name: &str, kind: Kind, scope: Option<Vec<Uuid>>, by: Option<Uuid>) -> sqlx::Result<Uuid> {
    lock_names(conn, org_id).await?;
    let taken: HashSet<String> =
        sqlx::query_scalar("select lower(name::text) from suppression_lists where org_id = $1").bind(org_id).fetch_all(&mut *conn).await?.into_iter().collect();
    let free = std::iter::once(name.to_string())
        .chain((2..).map(|n| format!("{name} ({n})")))
        .find(|n| !taken.contains(&n.to_lowercase()))
        .unwrap_or_default();
    sqlx::query_scalar("insert into suppression_lists (org_id, name, kind, project_ids, created_by) values ($1, $2, $3, $4, $5) returning id")
        .bind(org_id)
        .bind(free)
        .bind(kind.as_str())
        .bind(scope)
        .bind(by)
        .fetch_one(conn)
        .await
}

/// Adds the valid values to a list and returns how many were new and how many were skipped as invalid or already on it.
/// Blank cells don't count.
async fn add_entries<'a>(conn: &mut PgConnection, list_id: Uuid, org_id: Uuid, kind: Kind, raws: impl IntoIterator<Item = &'a str>) -> sqlx::Result<(u64, u64)> {
    let mut given = 0;
    let mut values = Vec::new();
    for raw in raws.into_iter().map(str::trim).filter(|r| !r.is_empty()) {
        given += 1;
        values.extend(kind.parse(raw));
    }
    let added = sqlx::query(
        "insert into suppression_list_entries (list_id, org_id, value) select $1, $2, v from unnest($3::text[]) v on conflict do nothing",
    )
    .bind(list_id)
    .bind(org_id)
    .bind(&values)
    .execute(conn)
    .await?
    .rows_affected();
    Ok((added, given - added))
}

#[derive(Serialize)]
pub struct Added {
    pub list: SuppressionList,
    pub added: u64,
    pub skipped: u64,
}

/// For the public API: adds to the org's list called `name`, making it when there's none, so a CRM can sync in batches.
/// `scope` left out keeps an existing list's projects and covers all of them for a new one.
pub async fn add_by_name<'a>(
    db: &PgPool,
    org_id: Uuid,
    name: &str,
    kind: Kind,
    scope: Option<Option<Vec<Uuid>>>,
    raws: impl IntoIterator<Item = &'a str>,
) -> AppResult<Added> {
    let name = check_name(name)?;
    let scope = match scope {
        Some(ids) => Some(check_scope(db, org_id, ids).await?),
        None => None,
    };
    let mut tx = db.begin().await?;
    lock_names(&mut tx, org_id).await?;
    let existing: Option<(Uuid, String)> = sqlx::query_as("select id, kind from suppression_lists where org_id = $1 and name = $2::citext")
        .bind(org_id)
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?;
    let id = match existing {
        Some((_, other)) if other != kind.as_str() => {
            return Err(AppError::Unprocessable(format!("\"{name}\" is a {other} list")));
        }
        Some((id, _)) => {
            if let Some(scope) = &scope {
                sqlx::query("update suppression_lists set project_ids = $2 where id = $1").bind(id).bind(scope).execute(&mut *tx).await?;
            }
            id
        }
        None => create(&mut tx, org_id, name, kind, scope.flatten(), None).await?,
    };
    let (added, skipped) = add_entries(&mut tx, id, org_id, kind, raws).await?;
    tx.commit().await?;
    Ok(Added { list: one(db, org_id, id).await?, added, skipped })
}

/// Points a list at other projects. `ids` None covers all of them.
pub async fn set_scope(db: &PgPool, org_id: Uuid, id: Uuid, ids: Option<Vec<Uuid>>) -> AppResult<SuppressionList> {
    let scope = check_scope(db, org_id, ids).await?;
    let updated = sqlx::query("update suppression_lists set project_ids = $3 where id = $1 and org_id = $2")
        .bind(id)
        .bind(org_id)
        .bind(scope)
        .execute(db)
        .await?
        .rows_affected();
    if updated == 0 {
        return Err(AppError::NotFound);
    }
    one(db, org_id, id).await
}

/// Deletes a list and every entry on it. Emails to the people it held go out again from the next send.
pub async fn delete(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<()> {
    let deleted = sqlx::query("delete from suppression_lists where id = $1 and org_id = $2").bind(id).bind(org_id).execute(db).await?.rows_affected();
    if deleted == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// A list's entries in order, `limit` from `offset`.
pub async fn entries_page(db: &PgPool, org_id: Uuid, id: Uuid, limit: i64, offset: i64) -> AppResult<Paginated<String>> {
    one(db, org_id, id).await?;
    let total: i64 = sqlx::query_scalar("select count(*) from suppression_list_entries where list_id = $1").bind(id).fetch_one(db).await?;
    let items = sqlx::query_scalar("select value from suppression_list_entries where list_id = $1 order by value limit $2 offset $3")
        .bind(id)
        .bind(limit)
        .bind(offset)
        .fetch_all(db)
        .await?;
    Ok(Paginated { items, total })
}

#[derive(Serialize, Debug, PartialEq)]
struct Column {
    header: String,
    /// What most of its filled cells are, or None when they're neither emails nor domains.
    kind: Option<&'static str>,
    /// How many of its cells are of that kind.
    values: usize,
}

/// A header that is itself an email or a domain is a file without a header row, so it counts as a value.
fn cells(csv: &Csv, i: usize) -> impl Iterator<Item = &str> {
    let header = csv.headers.get(i).map(String::as_str).filter(|h| Kind::of(h).is_some());
    header.into_iter().chain(csv.rows.iter().filter_map(move |(_, row)| row.get(i).map(String::as_str)))
}

/// Each column with what it holds: people when most of its filled cells are emails, companies when most are domains.
fn read_columns(csv: &Csv) -> Vec<Column> {
    (0..csv.headers.len())
        .map(|i| {
            let filled: Vec<&str> = cells(csv, i).map(str::trim).filter(|c| !c.is_empty()).collect();
            let found = [Kind::People, Kind::Companies]
                .into_iter()
                .map(|k| (k, filled.iter().filter(|c| k.parse(c).is_some()).count()))
                .find(|(_, n)| *n * 2 > filled.len());
            Column { header: csv.headers[i].clone(), kind: found.map(|(k, _)| k.as_str()), values: found.map_or(0, |(_, n)| n) }
        })
        .collect()
}

/// The column with the most emails or domains, the first on a tie.
fn best_column(columns: &[Column]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, c) in columns.iter().enumerate() {
        if c.kind.is_some() && best.is_none_or(|b| c.values > columns[b].values) {
            best = Some(i);
        }
    }
    best
}

#[derive(Deserialize)]
struct CsvBody {
    csv: String,
}

async fn columns(_auth: Auth, Json(body): Json<CsvBody>) -> AppResult<Json<Value>> {
    let columns = read_columns(&imports::parse_csv(body.csv.as_bytes())?);
    Ok(Json(json!({ "column": best_column(&columns), "columns": columns })))
}

#[derive(Deserialize)]
struct UploadBody {
    name: String,
    csv: String,
    /// Which column to read, by position. Left out, the one best_column picks.
    column: Option<usize>,
    /// Null covers every project.
    project_ids: Option<Vec<Uuid>>,
}

/// A new list from a CSV's column of emails or domains. A name the org already uses gets " (2)" and on.
async fn upload(State(state): State<AppState>, auth: Auth, Json(body): Json<UploadBody>) -> AppResult<Json<Added>> {
    let name = check_name(&body.name)?;
    let scope = check_scope(&state.db, auth.org_id, body.project_ids).await?;
    let csv = imports::parse_csv(body.csv.as_bytes())?;
    let columns = read_columns(&csv);
    let column = body.column.or_else(|| best_column(&columns));
    let Some((i, kind)) = column.and_then(|i| Some((i, columns.get(i)?.kind?))) else {
        return Err(AppError::BadRequest("The file has no column of emails or domains".into()));
    };
    let kind = if kind == "people" { Kind::People } else { Kind::Companies };
    let mut tx = state.db.begin().await?;
    let id = create(&mut tx, auth.org_id, name, kind, scope, Some(auth.user_id)).await?;
    let (added, skipped) = add_entries(&mut tx, id, auth.org_id, kind, cells(&csv, i)).await?;
    tx.commit().await?;
    Ok(Json(Added { list: one(&state.db, auth.org_id, id).await?, added, skipped }))
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<SuppressionList>>> {
    Ok(Json(all(&state.db, auth.org_id).await?))
}

#[derive(Deserialize)]
pub struct ScopeBody {
    /// Required: null covers every project. Left out is a 422 rather than a silent "all".
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    pub project_ids: Option<Option<Vec<Uuid>>>,
}

impl ScopeBody {
    pub fn into_scope(self) -> AppResult<Option<Vec<Uuid>>> {
        self.project_ids.ok_or_else(|| AppError::Unprocessable("Send project_ids: a list of project ids, or null for all projects".into()))
    }
}

async fn rescope(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<ScopeBody>) -> AppResult<Json<SuppressionList>> {
    Ok(Json(set_scope(&state.db, auth.org_id, id, body.into_scope()?).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    delete(&state.db, auth.org_id, id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn entries(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(page): Query<PageQuery>) -> AppResult<Json<Paginated<String>>> {
    Ok(Json(entries_page(&state.db, auth.org_id, id, page.limit(), page.offset()).await?))
}

#[cfg(test)]
mod tests {
    use axum::extract::{Json, State};
    use axum::Router;
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::{Kind, UploadBody};
    use crate::auth::Auth;
    use crate::crypto::sha256_hex;
    use crate::models::Lead;

    const KEY_A: &str = "splee_lists_key_a";
    const KEY_B: &str = "splee_lists_key_b";

    #[test]
    fn the_column_with_the_most_emails_or_domains_is_picked() {
        let csv = super::imports::parse_csv(b"Name,Email,Website\nAna,ana@north.example,north.example\nBo,,https://www.globex.example/\nCy,cy@globex.example,\n").unwrap();
        let columns = super::read_columns(&csv);
        let kinds: Vec<_> = columns.iter().map(|c| (c.kind, c.values)).collect();
        assert_eq!(kinds, [(None, 0), (Some("people"), 2), (Some("companies"), 2)]);
        assert_eq!(super::best_column(&columns), Some(1), "a tie goes to the first");
        // No header row: the first value counts too.
        let bare = super::imports::parse_csv(b"acme.example\nglobex.example\n").unwrap();
        assert_eq!(super::read_columns(&bare)[0].values, 2);
    }

    struct Org {
        id: Uuid,
        projects: [Uuid; 2],
    }

    /// Acme with KEY_A and Other with KEY_B, two projects each.
    async fn seed(db: &PgPool) -> (Org, Org) {
        let mut orgs = vec![];
        for (name, key) in [("Acme", KEY_A), ("Other", KEY_B)] {
            let id: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
            let mut projects = [Uuid::nil(); 2];
            for (i, p) in projects.iter_mut().enumerate() {
                *p = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id")
                    .bind(id)
                    .bind(format!("{name} {i}"))
                    .fetch_one(db)
                    .await
                    .unwrap();
            }
            sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'CRM', 'splee_lists_', $2)")
                .bind(id)
                .bind(sha256_hex(key))
                .execute(db)
                .await
                .unwrap();
            orgs.push(Org { id, projects });
        }
        let b = orgs.pop().unwrap();
        (orgs.pop().unwrap(), b)
    }

    async fn lead(db: &PgPool, project: Uuid, email: &str, company_domain: Option<&str>) -> Lead {
        sqlx::query_as("insert into leads (project_id, email, company_domain, status) values ($1, $2, $3, 'queued') returning *")
            .bind(project)
            .bind(email)
            .bind(company_domain)
            .fetch_one(db)
            .await
            .unwrap()
    }

    async fn blocked(db: &PgPool, org: Uuid, lead: &Lead) -> bool {
        crate::mail::sender::suppressed(db, org, lead, lead.email.as_deref().unwrap()).await.unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn lists_block_sends_only_in_the_projects_they_cover(db: PgPool) {
        let (a, b) = seed(&db).await;
        let [first, second] = a.projects;
        super::add_by_name(&db, a.id, "CRM customers", Kind::People, Some(Some(vec![first])), ["ANA@north.example"]).await.unwrap();
        super::add_by_name(&db, a.id, "Partner agencies", Kind::Companies, Some(Some(vec![second])), ["https://www.agency.example/"]).await.unwrap();
        // Another org's list names the same people and covers all its projects. It never applies to Acme.
        super::add_by_name(&db, b.id, "Theirs", Kind::People, None, ["dee@north.example"]).await.unwrap();

        let ana_first = lead(&db, first, "ana@north.example", None).await;
        assert!(blocked(&db, a.id, &ana_first).await);
        assert!(!blocked(&db, a.id, &lead(&db, second, "ana@north.example", None).await).await);
        // By email domain or by company domain, only where the companies list reaches.
        assert!(blocked(&db, a.id, &lead(&db, second, "bo@agency.example", None).await).await);
        assert!(blocked(&db, a.id, &lead(&db, second, "cy@gmail.example", Some("agency.example")).await).await);
        assert!(!blocked(&db, a.id, &lead(&db, first, "cy@gmail.example", Some("agency.example")).await).await);
        assert!(!blocked(&db, a.id, &lead(&db, first, "dee@north.example", None).await).await);

        // A queued first email to Ana is cancelled right before sending.
        sqlx::query("insert into project_settings (project_id, paused, send_days, send_start_hour, send_end_hour) values ($1, false, '{1,2,3,4,5,6,7}', 0, 24)")
            .bind(first)
            .execute(&db)
            .await
            .unwrap();
        let queue = || {
            sqlx::query_scalar::<_, Uuid>(
                "insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'outbound', 'initial', 'approved') returning id",
            )
            .bind(first)
            .bind(ana_first.id)
            .fetch_one(&db)
        };
        let status = |id: Uuid| sqlx::query_as::<_, (String, Option<String>)>("select status, error from email_messages where id = $1").bind(id).fetch_one(&db);
        let state = crate::autopilot::tests::state(db.clone());
        let email = queue().await.unwrap();
        crate::mail::sender::tick(&state).await.unwrap();
        assert_eq!(status(email).await.unwrap(), ("cancelled".into(), Some("Recipient is on the do-not-contact list".into())));

        // Deleting the list lets the next one through the check. With no mailbox here it then waits or fails on DNS instead.
        let list: Uuid = sqlx::query_scalar("select id from suppression_lists where name = 'CRM customers'").fetch_one(&db).await.unwrap();
        assert!(matches!(super::delete(&db, b.id, list).await, Err(crate::error::AppError::NotFound)), "another org can't delete it");
        super::delete(&db, a.id, list).await.unwrap();
        assert!(!blocked(&db, a.id, &ana_first).await);
        let email = queue().await.unwrap();
        crate::mail::sender::tick(&state).await.unwrap();
        assert_ne!(status(email).await.unwrap().0, "cancelled");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn an_upload_with_a_taken_name_gets_a_suffix(db: PgPool) {
        let (a, b) = seed(&db).await;
        let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('jo@acme.example', 'x') returning id").fetch_one(&db).await.unwrap();
        let state = crate::autopilot::tests::state(db.clone());
        let auth = |org_id| Auth { user_id: user, org_id, role: "owner".into(), token_hash: String::new() };
        let upload = |org_id: Uuid, name: &str, csv: &str| {
            let body = UploadBody { name: name.into(), csv: csv.into(), column: None, project_ids: None };
            super::upload(State(state.clone()), auth(org_id), Json(body))
        };

        let Json(made) = upload(a.id, "CRM customers", "Name,Email\nAna,ana@north.example\nAna,ANA@north.example\nX,not-an-email\n").await.unwrap();
        assert_eq!((made.list.name.as_str(), made.list.kind.as_str(), made.added, made.skipped), ("CRM customers", "people", 1, 2));
        assert_eq!(made.list.created_by, Some(user));
        let Json(again) = upload(a.id, "crm customers", "acme.example\nglobex.example\n").await.unwrap();
        assert_eq!((again.list.name.as_str(), again.list.kind.as_str(), again.added), ("crm customers (2)", "companies", 2));
        let Json(third) = upload(a.id, " CRM customers ", "ana@north.example").await.unwrap();
        assert_eq!(third.list.name, "CRM customers (3)");
        // Names are per org, and an org sees only its own lists.
        let Json(theirs) = upload(b.id, "CRM customers", "bo@other.example").await.unwrap();
        assert_eq!(theirs.list.name, "CRM customers");
        let names: Vec<String> = super::all(&db, b.id).await.unwrap().into_iter().map(|l| l.name).collect();
        assert_eq!(names, ["CRM customers"]);
        assert_eq!(super::all(&db, a.id).await.unwrap().len(), 3);
        assert!(upload(a.id, "Nothing", "Name\nAna\n").await.is_err(), "a file with no emails or domains");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_api_manages_lists_and_refuses_bad_scopes(db: PgPool) {
        let (a, b) = seed(&db).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", super::super::api_v1::router()).with_state(crate::autopilot::tests::state(db.clone()));
        tokio::spawn(async move { axum::serve(listener, app).await });
        let client = reqwest::Client::new();
        let call = |method: reqwest::Method, key: &str, path: &str, body: Option<Value>| {
            let mut req = client.request(method, format!("{base}{path}")).bearer_auth(key);
            if let Some(body) = body {
                req = req.json(&body);
            }
            async move {
                let res = req.send().await.unwrap();
                (res.status().as_u16(), res.json::<Value>().await.unwrap_or(Value::Null))
            }
        };
        let post = |path: &str, body: Value| call(reqwest::Method::POST, KEY_A, path, Some(body));
        let people = "/v1/suppression-lists/people";

        assert_eq!(post(people, json!({"list": "CRM", "people": [{"email": "a@x.example"}], "project_ids": []})).await.0, 422);
        let foreign = json!({"list": "CRM", "people": [{"email": "a@x.example"}], "project_ids": [b.projects[0]]});
        assert_eq!(post(people, foreign).await.0, 422);
        assert_eq!(sqlx::query_scalar::<_, i64>("select count(*) from suppression_lists").fetch_one(&db).await.unwrap(), 0);

        let body = json!({"list": "CRM", "people": [{"email": "A@x.example"}, {"email": "b@x.example"}, {"email": "nope"}], "project_ids": [a.projects[0]]});
        let (status, made) = post(people, body).await;
        assert_eq!((status, made["added"].clone(), made["skipped"].clone()), (200, json!(2), json!(1)), "{made}");
        assert_eq!(made["list"]["project_ids"], json!([a.projects[0]]));
        let id = made["list"]["id"].as_str().unwrap().to_string();
        // The same name adds to that list and keeps its projects, so a CRM can sync in batches.
        let (_, more) = post(people, json!({"list": "crm", "people": [{"email": "b@x.example"}, {"email": "c@x.example"}]})).await;
        assert_eq!((more["list"]["id"].as_str(), more["added"].clone(), more["skipped"].clone()), (Some(id.as_str()), json!(1), json!(1)));
        assert_eq!((more["list"]["entries"].clone(), more["list"]["project_ids"].clone()), (json!(3), json!([a.projects[0]])));
        assert_eq!(post("/v1/suppression-lists/companies", json!({"list": "CRM", "domains": ["x.example"]})).await.0, 422, "a people list");
        let (status, firms) = post("/v1/suppression-lists/companies", json!({"list": "Agencies", "domains": ["x.example"], "project_ids": null})).await;
        assert_eq!((status, firms["list"]["project_ids"].clone(), firms["list"]["kind"].clone()), (200, Value::Null, json!("companies")));

        let (_, listed) = call(reqwest::Method::GET, KEY_A, "/v1/suppression-lists", None).await;
        assert_eq!(listed.as_array().map(Vec::len), Some(2));
        assert_eq!(call(reqwest::Method::GET, KEY_B, "/v1/suppression-lists", None).await, (200, json!([])));
        let entries = format!("/v1/suppression-lists/{id}/entries");
        assert_eq!(call(reqwest::Method::GET, KEY_A, &format!("{entries}?limit=1&offset=1"), None).await, (200, json!({"items": ["b@x.example"], "total": 3})));
        assert_eq!(call(reqwest::Method::GET, KEY_B, &entries, None).await.0, 404);

        let one = format!("/v1/suppression-lists/{id}");
        assert_eq!(call(reqwest::Method::PATCH, KEY_A, &one, Some(json!({}))).await.0, 422, "project_ids is required");
        assert_eq!(call(reqwest::Method::PATCH, KEY_A, &one, Some(json!({"project_ids": []}))).await.0, 422);
        assert_eq!(call(reqwest::Method::PATCH, KEY_B, &one, Some(json!({"project_ids": null}))).await.0, 404);
        let (status, all) = call(reqwest::Method::PATCH, KEY_A, &one, Some(json!({"project_ids": null}))).await;
        assert_eq!((status, all["project_ids"].clone()), (200, Value::Null));
        assert_eq!(call(reqwest::Method::DELETE, KEY_B, &one, None).await.0, 404);
        assert_eq!(call(reqwest::Method::DELETE, KEY_A, &one, None).await, (200, json!({"ok": true})));
        assert_eq!(sqlx::query_scalar::<_, i64>("select count(*) from suppression_list_entries").fetch_one(&db).await.unwrap(), 1);
    }
}
