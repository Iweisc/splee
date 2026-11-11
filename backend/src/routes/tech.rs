//! Tech-stack signals: the tools a project looks for, and its companies with the tools their sites and DNS show.

use axum::extract::{Path, Query, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::util_pagination::{PageQuery, Paginated};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::fingerprints::{self, CATALOGUE, Category};
use crate::models::Ts;
use crate::state::AppState;
use crate::tech::{self, Pick};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/tech", get(catalogue))
        .route("/projects/{id}/tech/picks", put(set_picks))
        .route("/projects/{id}/tech/companies", get(companies))
}

/// Enough for every tool a product competes with or works alongside.
const MAX_PICKS: usize = 50;

/// Every tool that can be detected, the categories in order, and the project's picks.
async fn catalogue(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    let categories: Vec<Value> = Category::ALL.iter().map(|c| json!({ "key": c.key(), "label": c.label() })).collect();
    Ok(Json(json!({ "catalogue": CATALOGUE, "categories": categories, "picks": tech::picks(&state.db, id).await? })))
}

#[derive(Deserialize)]
struct PicksBody {
    picks: Vec<Pick>,
}

/// Replaces the project's picks, in the order given. A tool picked twice keeps its first kind.
async fn set_picks(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PicksBody>) -> AppResult<Json<Vec<Pick>>> {
    auth.project(&state.db, id).await?;
    let mut picks: Vec<Pick> = vec![];
    for pick in body.picks {
        let tool = fingerprints::find(&pick.tech).ok_or_else(|| AppError::BadRequest(format!("Splee can't detect {} yet", pick.tech.trim())))?;
        if !matches!(pick.kind.as_str(), "uses" | "competitor") {
            return Err(AppError::BadRequest("Mark each tool as uses or competitor".into()));
        }
        if !picks.iter().any(|p| p.tech == tool.name) {
            picks.push(Pick { tech: tool.name.into(), kind: pick.kind });
        }
    }
    if picks.len() > MAX_PICKS {
        return Err(AppError::BadRequest(format!("Pick at most {MAX_PICKS} tools")));
    }
    let (names, kinds): (Vec<String>, Vec<String>) = picks.iter().map(|p| (p.tech.clone(), p.kind.clone())).unzip();
    let mut tx = state.db.begin().await?;
    sqlx::query("delete from project_tech_picks where project_id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query(
        "insert into project_tech_picks (project_id, tech, kind, position)
         select $1, tech, kind, n - 1 from unnest($2::text[], $3::text[]) with ordinality as p(tech, kind, n)",
    )
    .bind(id)
    .bind(&names)
    .bind(&kinds)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    crate::score::touch(&state.db, id).await?;
    Ok(Json(picks))
}

#[derive(Deserialize)]
struct CompaniesQuery {
    /// A tool's name, a category's key, or "picks" for any of the project's picks. Every company with a tool found without it.
    tech: Option<String>,
}

/// A project company whose scan found some tools. leads counts the project's leads there, so only companies without any
/// are offered to move to a campaign.
#[derive(Serialize, FromRow)]
pub struct TechCompany {
    id: Uuid,
    domain: String,
    name: String,
    segment_id: Option<Uuid>,
    leads: i64,
    techs: Vec<String>,
    scanned_at: Option<Ts>,
}

async fn companies(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<CompaniesQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<TechCompany>>> {
    auth.project(&state.db, id).await?;
    let names: Option<Vec<String>> = match q.tech.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        None => None,
        Some("picks") => Some(tech::picks(&state.db, id).await?.into_iter().map(|p| p.tech).collect()),
        Some(filter) => Some(fingerprints::named(filter).ok_or_else(|| AppError::BadRequest(format!("Splee can't detect {filter} yet")))?
            .into_iter().map(String::from).collect()),
    };
    Ok(Json(tech_companies(&state.db, id, names.as_deref(), &page).await?))
}

const COMPANIES_FILTER: &str = "from companies c join tech_scans t on t.domain = c.domain
     where c.project_id = $1 and c.status <> 'excluded' and cardinality(t.techs) > 0 and ($2::text[] is null or t.techs && $2)";

/// Companies with the most of the project's picks first.
async fn tech_companies(db: &PgPool, project_id: Uuid, names: Option<&[String]>, page: &PageQuery) -> sqlx::Result<Paginated<TechCompany>> {
    let total: i64 = sqlx::query_scalar(&format!("select count(*) {COMPANIES_FILTER}")).bind(project_id).bind(names).fetch_one(db).await?;
    let items = sqlx::query_as(&format!(
        "select c.id, c.domain::text, c.name, c.segment_id, t.techs, t.scanned_at,
                (select count(*) from leads l where l.project_id = c.project_id and (l.company_id = c.id or l.company_domain = c.domain)) as leads
         {COMPANIES_FILTER}
         order by (select count(*) from project_tech_picks p where p.project_id = $1 and p.tech = any(t.techs)) desc,
                  coalesce(nullif(c.name, ''), c.domain::text), c.id
         limit $3 offset $4"
    ))
    .bind(project_id)
    .bind(names)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(db)
    .await?;
    Ok(Paginated { items, total })
}

#[cfg(test)]
mod tests {
    use axum::Json;
    use axum::extract::{Path, Query, State};
    use serde_json::json;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;
    use crate::error::AppError;

    async fn org_with_project(db: &PgPool) -> (Auth, Uuid) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id) values ($1) returning id").bind(org).fetch_one(db).await.unwrap();
        (Auth { user_id: Uuid::new_v4(), org_id: org, role: "owner".into(), token_hash: String::new() }, project)
    }

    fn body(picks: serde_json::Value) -> Json<PicksBody> {
        Json(serde_json::from_value(json!({ "picks": picks })).unwrap())
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn picks_are_checked_against_the_catalogue_and_kept_to_the_organization(db: PgPool) {
        let state = crate::autopilot::tests::state(db.clone());
        let (auth, project) = org_with_project(&db).await;
        let (other_auth, other_project) = org_with_project(&db).await;

        let picks = [json!({"tech": "hubspot", "kind": "competitor"}), json!({"tech": "Intercom", "kind": "uses"}), json!({"tech": "HubSpot", "kind": "uses"})];
        let Json(saved) = set_picks(State(state.clone()), auth.clone(), Path(project), body(json!(picks))).await.unwrap();
        let kinds: Vec<(&str, &str)> = saved.iter().map(|p| (p.tech.as_str(), p.kind.as_str())).collect();
        assert_eq!(kinds, [("HubSpot", "competitor"), ("Intercom", "uses")]);
        assert_eq!(tech::picks(&db, project).await.unwrap(), saved);

        for bad in [json!([{"tech": "Nope CRM", "kind": "uses"}]), json!([{"tech": "Stripe", "kind": "maybe"}])] {
            let refused = set_picks(State(state.clone()), auth.clone(), Path(project), body(bad)).await;
            assert!(matches!(refused, Err(AppError::BadRequest(_))));
        }
        assert_eq!(tech::picks(&db, project).await.unwrap().len(), 2);

        // Another organization's project looks like one that doesn't exist, and keeps its picks.
        let refused = set_picks(State(state.clone()), other_auth.clone(), Path(project), body(json!([]))).await;
        assert!(matches!(refused, Err(AppError::NotFound)));
        assert!(matches!(catalogue(State(state.clone()), other_auth, Path(project)).await, Err(AppError::NotFound)));
        assert_eq!(tech::picks(&db, project).await.unwrap().len(), 2);
        assert!(tech::picks(&db, other_project).await.unwrap().is_empty());
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn companies_filter_by_tool_category_or_picks_with_picks_first(db: PgPool) {
        let state = crate::autopilot::tests::state(db.clone());
        let (auth, project) = org_with_project(&db).await;
        for (domain, name, techs) in [
            ("crm.com", "Crm Co", "{Pipedrive,Cloudflare}"),
            ("both.com", "Both", "{HubSpot,Intercom}"),
            ("plain.com", "Plain", "{Cloudflare}"),
            ("none.com", "None", "{}"),
        ] {
            sqlx::query("insert into companies (project_id, domain, name) values ($1, $2, $3)").bind(project).bind(domain).bind(name).execute(&db).await.unwrap();
            sqlx::query("insert into tech_scans (domain, techs, scanned_at) values ($1, $2::text[], now())").bind(domain).bind(techs).execute(&db).await.unwrap();
        }
        sqlx::query("insert into project_tech_picks (project_id, tech, kind) values ($1, 'HubSpot', 'competitor'), ($1, 'Intercom', 'uses')")
            .bind(project)
            .execute(&db)
            .await
            .unwrap();
        let list = |tech: Option<&str>| {
            let state = state.clone();
            let auth = auth.clone();
            let q = CompaniesQuery { tech: tech.map(String::from) };
            async move {
                let Json(page) = companies(State(state), auth, Path(project), Query(q), Query(PageQuery { page: None, per_page: None })).await.unwrap();
                page.items.into_iter().map(|c| c.domain).collect::<Vec<_>>()
            }
        };
        assert_eq!(list(None).await, ["both.com", "crm.com", "plain.com"]);
        assert_eq!(list(Some("crm")).await, ["both.com", "crm.com"]);
        assert_eq!(list(Some("cloudflare")).await, ["crm.com", "plain.com"]);
        assert_eq!(list(Some("picks")).await, ["both.com"]);
        let unknown = companies(State(state.clone()), auth.clone(), Path(project), Query(CompaniesQuery { tech: Some("Nope".into()) }), Query(PageQuery { page: None, per_page: None })).await;
        assert!(matches!(unknown, Err(AppError::BadRequest(_))));
    }
}
