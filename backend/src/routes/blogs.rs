//! Connecting the company's own WordPress, Ghost and Webflow blogs, which crate::blog publishes approved articles on. Secrets
//! are write-only. Testing a blog reads it with what the user typed, and adding one reads it again before saving it.

use axum::extract::{Path, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{util_input, util_scope};
use crate::auth::Auth;
use crate::blog::{self, Fault, Ghost, Webflow, WordPress};
use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::models::Blog;
use crate::state::AppState;

/// Plenty for a company's own sites.
const MAX_BLOGS: i64 = 10;
/// Bounds the reads one test makes on a Webflow account with many sites.
const MAX_SITES: usize = 10;
const MAX_COLLECTIONS: usize = 20;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/blogs", get(list).post(add))
        .route("/projects/{id}/blogs/test", post(test))
        .route("/blogs/{id}", patch(update).delete(remove))
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Blog>>> {
    auth.project(&state.db, id).await?;
    let blogs = sqlx::query_as("select * from blogs where project_id = $1 order by created_at").bind(id).fetch_all(&state.db).await?;
    Ok(Json(blogs))
}

#[derive(Deserialize)]
struct ConnectBody {
    kind: String,
    /// WordPress's site, or Ghost's admin address.
    #[serde(default)]
    url: String,
    /// WordPress only.
    #[serde(default)]
    username: String,
    /// WordPress's application password, Ghost's Admin API key or Webflow's API token.
    secret: String,
    /// Webflow only: the site and collection articles go in, and the collection's fields for each part. Tags and excerpt are
    /// optional, and the title is the collection's name field unless another is picked.
    #[serde(default)]
    site_id: String,
    #[serde(default)]
    collection_id: String,
    #[serde(default)]
    title_field: String,
    #[serde(default)]
    body_field: String,
    #[serde(default)]
    tags_field: String,
    #[serde(default)]
    excerpt_field: String,
    #[serde(default)]
    publish: bool,
    weekly_cap: Option<i32>,
}

/// What reading the blog found. For Webflow, the sites the token can see, each with its collections and their fields.
#[derive(Serialize)]
struct Tested {
    name: String,
    user: String,
    url: String,
    sites: Vec<Value>,
}

fn refused(e: Fault) -> AppError {
    AppError::BadRequest(e.message())
}

fn secret(b: &ConnectBody) -> AppResult<&str> {
    let secret = b.secret.trim();
    if secret.is_empty() {
        let what = match b.kind.as_str() {
            "wordpress" => "the application password",
            "ghost" => "the Admin API key",
            _ => "the API token",
        };
        return Err(AppError::BadRequest(format!("Paste {what}")));
    }
    Ok(secret)
}

/// Reads a WordPress or Ghost blog with what the user typed.
async fn read_site(b: &ConnectBody) -> AppResult<Tested> {
    let secret = secret(b)?;
    let url = blog::site_url(&b.url).ok_or_else(|| {
        AppError::BadRequest(match b.kind.as_str() {
            "ghost" => "Write the address you open Ghost's admin on, like example.ghost.io".into(),
            _ => "Write the blog's address, like example.com/blog".into(),
        })
    })?;
    let site = match b.kind.as_str() {
        "wordpress" => {
            let username = b.username.trim();
            if username.is_empty() {
                return Err(AppError::BadRequest("Write the WordPress username the application password belongs to".into()));
            }
            WordPress::new(&url, username, secret).await.map_err(refused)?.check().await.map_err(refused)?
        }
        _ => Ghost::new(&url, secret).await.map_err(refused)?.check().await.map_err(refused)?,
    };
    let name = if site.name.is_empty() { url.trim_start_matches("https://").to_string() } else { site.name };
    Ok(Tested { name, user: site.user, url, sites: Vec::new() })
}

fn field(f: &Value) -> Value {
    json!({ "slug": f["slug"], "name": f["displayName"], "type": f["type"], "required": f["isRequired"] == true })
}

async fn test(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<ConnectBody>) -> AppResult<Json<Tested>> {
    auth.project(&state.db, id).await?;
    util_input::one_of("kind", &b.kind, &blog::KINDS)?;
    if b.kind != "webflow" {
        return Ok(Json(read_site(&b).await?));
    }
    let wf = Webflow::new(&state.cfg.webflow_api_url, secret(&b)?).map_err(refused)?;
    let mut sites = Vec::new();
    for site in wf.sites().await.map_err(refused)?.into_iter().take(MAX_SITES) {
        let mut collections = Vec::new();
        for c in wf.collections(&site.id).await.map_err(refused)?.into_iter().take(MAX_COLLECTIONS) {
            let full = wf.collection(c["id"].as_str().unwrap_or_default()).await.map_err(refused)?;
            let fields: Vec<Value> = full["fields"].as_array().map(|f| f.iter().map(field).collect()).unwrap_or_default();
            collections.push(json!({ "id": c["id"], "name": c["displayName"], "slug": c["slug"], "fields": fields }));
        }
        sites.push(json!({ "id": site.id, "name": site.name, "url": site.url, "collections": collections }));
    }
    if sites.is_empty() {
        return Err(AppError::BadRequest("The token can't see any site. Make it in the settings of the site the blog is on".into()));
    }
    Ok(Json(Tested { name: String::new(), user: String::new(), url: String::new(), sites }))
}

/// A blog row before it's saved.
struct Found {
    url: String,
    name: String,
    account: String,
    collection_name: String,
    collection_slug: String,
    title_field: String,
}

/// Reads the Webflow site and collection the user picked, and checks the fields they picked for each part, and that Splee
/// can fill every field the collection requires.
async fn read_webflow(state: &AppState, b: &ConnectBody) -> AppResult<Found> {
    let wf = Webflow::new(&state.cfg.webflow_api_url, secret(b)?).map_err(refused)?;
    let site = wf.sites().await.map_err(refused)?.into_iter().find(|s| s.id == b.site_id);
    let site = site.ok_or_else(|| AppError::BadRequest("Pick one of the sites the token can see".into()))?;
    let listed = wf.collections(&site.id).await.map_err(refused)?;
    if !listed.iter().any(|c| c["id"] == b.collection_id.as_str()) {
        return Err(AppError::BadRequest(format!("Pick one of the collections on {}", site.name)));
    }
    let collection = wf.collection(&b.collection_id).await.map_err(refused)?;
    let name = said(&collection["displayName"]);
    let fields = collection["fields"].as_array().cloned().unwrap_or_default();
    let kind_of = |slug: &str| fields.iter().find(|f| f["slug"] == slug).map(|f| said(&f["type"]));
    let title_field = if b.title_field.is_empty() { "name" } else { b.title_field.as_str() };
    for (slug, wanted, what, required) in [
        (title_field, "PlainText", "title", true),
        (b.body_field.as_str(), "RichText", "article", true),
        (b.tags_field.as_str(), "PlainText", "tags", false),
        (b.excerpt_field.as_str(), "PlainText", "excerpt", false),
    ] {
        if slug.is_empty() && !required {
            continue;
        }
        if kind_of(slug).as_deref() != Some(wanted) {
            let kind = if wanted == "RichText" { "rich text" } else { "plain text" };
            return Err(AppError::BadRequest(format!("Pick a {kind} field of {name} for the {what}")));
        }
    }
    let filled = ["slug", title_field, &b.body_field, &b.tags_field, &b.excerpt_field];
    if let Some(f) = fields.iter().find(|f| f["isRequired"] == true && !filled.contains(&f["slug"].as_str().unwrap_or_default())) {
        let field = said(&f["displayName"]);
        return Err(AppError::BadRequest(format!("{name} needs {field} filled in, which Splee can't write. Make it optional in Webflow, or pick another collection")));
    }
    Ok(Found {
        url: site.url,
        name: site.name,
        account: format!("webflow/{}", b.collection_id),
        collection_name: name,
        collection_slug: said(&collection["slug"]),
        title_field: title_field.into(),
    })
}

fn said(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn check_cap(cap: i32) -> AppResult<()> {
    if !(1..=7).contains(&cap) {
        return Err(AppError::BadRequest("A blog takes 1 to 7 articles a week".into()));
    }
    Ok(())
}

/// Saves the blog once reading it works. Adding the same blog again replaces its secret and settings and unpauses it, and
/// keeps its articles.
async fn add(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<ConnectBody>) -> AppResult<Json<Blog>> {
    auth.project(&state.db, id).await?;
    util_input::one_of("kind", &b.kind, &blog::KINDS)?;
    let weekly_cap = b.weekly_cap.unwrap_or(2);
    check_cap(weekly_cap)?;
    let webflow = b.kind == "webflow";
    let found = if webflow {
        read_webflow(&state, &b).await?
    } else {
        let site = read_site(&b).await?;
        let account = blog::account(&site.url);
        Found { url: site.url, name: site.name, account, collection_name: String::new(), collection_slug: String::new(), title_field: String::new() }
    };
    let others: i64 = sqlx::query_scalar("select count(*) from blogs where project_id = $1 and account <> $2").bind(id).bind(&found.account).fetch_one(&state.db).await?;
    if others >= MAX_BLOGS {
        return Err(AppError::BadRequest(format!("A project can connect up to {MAX_BLOGS} blogs")));
    }
    let only = |s: &str| if webflow { s.to_string() } else { String::new() };
    let added = sqlx::query_as::<_, Blog>(
        "insert into blogs (project_id, kind, url, name, username, secret_enc, account, site_id, collection_id, collection_name, collection_slug,
           title_field, body_field, tags_field, excerpt_field, publish, weekly_cap)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
         on conflict (project_id, account) do update set kind = excluded.kind, url = excluded.url, name = excluded.name, username = excluded.username,
           secret_enc = excluded.secret_enc, site_id = excluded.site_id, collection_id = excluded.collection_id,
           collection_name = excluded.collection_name, collection_slug = excluded.collection_slug, title_field = excluded.title_field,
           body_field = excluded.body_field, tags_field = excluded.tags_field, excerpt_field = excluded.excerpt_field, publish = excluded.publish,
           weekly_cap = excluded.weekly_cap, paused = false, error = null, failed_at = null
         returning *",
    )
    .bind(id)
    .bind(&b.kind)
    .bind(&found.url)
    .bind(&found.name)
    .bind(if b.kind == "wordpress" { b.username.trim() } else { "" })
    .bind(crypto::encrypt(&state.cfg.app_secret, secret(&b)?))
    .bind(&found.account)
    .bind(only(&b.site_id))
    .bind(only(&b.collection_id))
    .bind(&found.collection_name)
    .bind(&found.collection_slug)
    .bind(&found.title_field)
    .bind(only(&b.body_field))
    .bind(only(&b.tags_field))
    .bind(only(&b.excerpt_field))
    .bind(b.publish)
    .bind(weekly_cap)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(added))
}

#[derive(Deserialize)]
struct UpdateBody {
    publish: Option<bool>,
    weekly_cap: Option<i32>,
}

async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(b): Json<UpdateBody>) -> AppResult<Json<Blog>> {
    let blog: Blog = util_scope::load(&state.db, "blogs", id, auth.org_id).await?;
    if let Some(cap) = b.weekly_cap {
        check_cap(cap)?;
    }
    let updated = sqlx::query_as("update blogs set publish = coalesce($2, publish), weekly_cap = coalesce($3, weekly_cap) where id = $1 returning *")
        .bind(blog.id)
        .bind(b.publish)
        .bind(b.weekly_cap)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(updated))
}

/// The blog's approved articles go back to drafts, since adding the blog again makes a new row they don't point at.
async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let blog: Blog = util_scope::load(&state.db, "blogs", id, auth.org_id).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("update social_posts set status = 'draft', updated_at = now() where blog_id = $1 and status = 'approved'").bind(blog.id).execute(&mut *tx).await?;
    sqlx::query("delete from blogs where id = $1").bind(blog.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use sqlx::PgPool;

    use super::*;
    use crate::blog::fake;
    use crate::crypto::sha256_hex;

    /// Ana's project in Acme, and another org's project with a blog of its own.
    async fn server(db: &PgPool) -> (String, String, Uuid, Uuid, Uuid) {
        let (fake, _) = fake::start().await;
        let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('ana@acme.test', 'Ana', 'x') returning id").fetch_one(db).await.unwrap();
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(db).await.unwrap();
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(user).execute(db).await.unwrap();
        sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
            .bind(sha256_hex("session-acme"))
            .bind(user)
            .bind(org)
            .execute(db)
            .await
            .unwrap();
        let ours: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(db).await.unwrap();
        let other: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Other') returning id").fetch_one(db).await.unwrap();
        let theirs: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Other') returning id").bind(other).fetch_one(db).await.unwrap();
        let their_blog: Uuid = sqlx::query_scalar("insert into blogs (project_id, kind, url, secret_enc, account) values ($1, 'ghost', 'https://o.test', 'x', 'o.test') returning id")
            .bind(theirs)
            .fetch_one(db)
            .await
            .unwrap();
        let state = fake::state(db, &fake).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", crate::routes::router()).with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await });
        (base, fake, ours, theirs, their_blog)
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn blogs_are_read_before_they_are_saved_and_other_orgs_blogs_are_out_of_reach(db: PgPool) {
        let (base, fake, ours, theirs, their_blog) = server(&db).await;
        let client = reqwest::Client::new();
        let call = |method: reqwest::Method, path: String, body: Value| {
            client.request(method, format!("{base}{path}")).header("cookie", "splee_session=session-acme").json(&body).send()
        };
        let wordpress = json!({ "kind": "wordpress", "url": format!("{fake}/wp/wp-admin/"), "username": fake::WP_USER, "secret": fake::WP_PASSWORD });

        // Testing reads the site and who the password belongs to, and saves nothing.
        let res = call(reqwest::Method::POST, format!("/projects/{ours}/blogs/test"), wordpress.clone()).await.unwrap();
        assert_eq!(res.status(), 200);
        let tested: Value = res.json().await.unwrap();
        assert_eq!((tested["name"].as_str(), tested["user"].as_str(), tested["url"].as_str()), (Some("Acme & Co's blog"), Some("Ana Silva"), Some(&*format!("{fake}/wp"))));
        let wrong = json!({ "kind": "wordpress", "url": format!("{fake}/wp"), "username": fake::WP_USER, "secret": "nope" });
        let res = call(reqwest::Method::POST, format!("/projects/{ours}/blogs/test"), wrong.clone()).await.unwrap();
        assert!(res.text().await.unwrap().contains("didn't accept the username and application password"));
        assert_eq!(sqlx::query_scalar::<_, i64>("select count(*) from blogs where project_id = $1").bind(ours).fetch_one(&db).await.unwrap(), 0);
        let res = call(reqwest::Method::POST, format!("/projects/{ours}/blogs"), wrong).await.unwrap();
        assert_eq!(res.status(), 400);

        let res = call(reqwest::Method::POST, format!("/projects/{ours}/blogs"), wordpress.clone()).await.unwrap();
        let added: Value = res.json().await.unwrap();
        assert_eq!((added["name"].as_str(), added["publish"].as_bool(), added["weekly_cap"].as_i64()), (Some("Acme & Co's blog"), Some(false), Some(2)));
        assert!(added.get("secret_enc").is_none(), "the password stays on the server");
        let saved: String = sqlx::query_scalar("select secret_enc from blogs where project_id = $1").bind(ours).fetch_one(&db).await.unwrap();
        assert_eq!(crypto::decrypt("test-secret", &saved).unwrap(), fake::WP_PASSWORD);

        // Connecting it again unpauses it.
        sqlx::query("update blogs set paused = true, error = 'refused' where project_id = $1").bind(ours).execute(&db).await.unwrap();
        let again: Value = call(reqwest::Method::POST, format!("/projects/{ours}/blogs"), wordpress).await.unwrap().json().await.unwrap();
        assert_eq!((again["id"].clone(), again["paused"].clone(), again["error"].clone()), (added["id"].clone(), json!(false), Value::Null));

        let ghost = json!({ "kind": "ghost", "url": format!("{fake}/gh/ghost/#/site"), "secret": fake::GHOST_KEY, "publish": true, "weekly_cap": 3 });
        let added: Value = call(reqwest::Method::POST, format!("/projects/{ours}/blogs"), ghost).await.unwrap().json().await.unwrap();
        assert_eq!((added["name"].as_str(), added["url"].as_str(), added["weekly_cap"].as_i64()), (Some("Acme Notes"), Some(&*format!("{fake}/gh")), Some(3)));

        // Webflow lists its sites with their collections and fields, and checks the ones picked.
        let tested: Value = call(reqwest::Method::POST, format!("/projects/{ours}/blogs/test"), json!({ "kind": "webflow", "secret": fake::WEBFLOW_TOKEN }))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let blog_posts = &tested["sites"][0]["collections"][0];
        assert_eq!((tested["sites"][0]["url"].as_str(), blog_posts["name"].as_str()), (Some("https://www.acme.test"), Some("Blog Posts")));
        assert_eq!(blog_posts["fields"][2], json!({ "slug": "post-body", "name": "Post Body", "type": "RichText", "required": false }));
        let webflow = |collection: &str, body: &str| {
            json!({ "kind": "webflow", "secret": fake::WEBFLOW_TOKEN, "site_id": "site1", "collection_id": collection, "body_field": body, "tags_field": "tags",
                    "excerpt_field": "post-summary", "publish": true })
        };
        let res = call(reqwest::Method::POST, format!("/projects/{ours}/blogs"), webflow("col2", "")).await.unwrap();
        assert!(res.text().await.unwrap().contains("Pick a rich text field of Team for the article"));
        let res = call(reqwest::Method::POST, format!("/projects/{ours}/blogs"), webflow("col2", "name")).await.unwrap();
        assert_eq!(res.status(), 400);
        let res = call(reqwest::Method::POST, format!("/projects/{ours}/blogs"), webflow("col1", "post-summary")).await.unwrap();
        assert!(res.text().await.unwrap().contains("rich text field of Blog Posts"));
        let added: Value = call(reqwest::Method::POST, format!("/projects/{ours}/blogs"), webflow("col1", "post-body")).await.unwrap().json().await.unwrap();
        assert_eq!((added["url"].as_str(), added["collection_slug"].as_str(), added["title_field"].as_str()), (Some("https://www.acme.test"), Some("blog"), Some("name")));
        let blog = added["id"].as_str().unwrap();
        let res = call(reqwest::Method::PATCH, format!("/blogs/{blog}"), json!({ "weekly_cap": 9 })).await.unwrap();
        assert_eq!(res.status(), 400);
        let res: Value = call(reqwest::Method::PATCH, format!("/blogs/{blog}"), json!({ "publish": false, "weekly_cap": 4 })).await.unwrap().json().await.unwrap();
        assert_eq!((res["publish"].clone(), res["weekly_cap"].clone()), (json!(false), json!(4)));
        let listed: Vec<Value> = call(reqwest::Method::GET, format!("/projects/{ours}/blogs"), Value::Null).await.unwrap().json().await.unwrap();
        assert_eq!(listed.len(), 3);

        // An article needs a blog that isn't paused before it can be approved, and stays a draft until it is.
        let article = |status: &str| json!({ "platform": "blog", "blog_id": blog, "title": "How we warm up", "body": "## Day one", "excerpt": "Four weeks.", "status": status });
        sqlx::query("update blogs set paused = true where id = $1::uuid").bind(blog).execute(&db).await.unwrap();
        let res = call(reqwest::Method::POST, format!("/projects/{ours}/social"), article("approved")).await.unwrap();
        assert!(res.text().await.unwrap().contains("Webflow turned Splee down. Connect it again"));
        let draft: Value = call(reqwest::Method::POST, format!("/projects/{ours}/social"), article("draft")).await.unwrap().json().await.unwrap();
        assert_eq!((draft["status"].as_str(), draft["excerpt"].as_str()), (Some("draft"), Some("Four weeks.")));
        let approve = format!("/social/{}", draft["id"].as_str().unwrap());
        assert_eq!(call(reqwest::Method::PATCH, approve.clone(), json!({ "status": "approved" })).await.unwrap().status(), 400);
        sqlx::query("update blogs set paused = false where id = $1::uuid").bind(blog).execute(&db).await.unwrap();
        let long = call(reqwest::Method::PATCH, approve.clone(), json!({ "status": "approved", "excerpt": "a".repeat(301) })).await.unwrap();
        assert!(long.text().await.unwrap().contains("Excerpts are at most 300 characters"));
        let approved: Value = call(reqwest::Method::PATCH, approve, json!({ "status": "approved", "tags": ["#Cold Email", "cold email"] })).await.unwrap().json().await.unwrap();
        assert_eq!((approved["status"].as_str(), approved["tags"].clone()), (Some("approved"), json!(["Cold Email"])));

        // Another org's project and blog are out of reach, and removing ours sends its approved articles back to drafts.
        for (method, path) in [
            (reqwest::Method::GET, format!("/projects/{theirs}/blogs")),
            (reqwest::Method::POST, format!("/projects/{theirs}/blogs/test")),
            (reqwest::Method::POST, format!("/projects/{theirs}/blogs")),
            (reqwest::Method::PATCH, format!("/blogs/{their_blog}")),
            (reqwest::Method::DELETE, format!("/blogs/{their_blog}")),
        ] {
            let body = json!({ "kind": "ghost", "url": format!("{fake}/gh"), "secret": fake::GHOST_KEY, "publish": true });
            assert_eq!(call(method, path.clone(), body).await.unwrap().status(), 404, "{path}");
        }
        assert_eq!(call(reqwest::Method::DELETE, format!("/blogs/{blog}"), Value::Null).await.unwrap().status(), 200);
        let back: String = sqlx::query_scalar("select status from social_posts where id = $1::uuid").bind(draft["id"].as_str().unwrap()).fetch_one(&db).await.unwrap();
        assert_eq!(back, "draft");
    }
}
