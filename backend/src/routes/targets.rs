//! Targets: what each teammate should win this month, what they won, and what the agent's forecast says closes the gap.
//! A deal is the teammate's when they own its lead. Only deals in the organization's currency count. "This month" is the
//! calendar month in UTC, like the pipeline's Won this month.

use axum::extract::{Path, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/targets", get(list)).route("/targets/{user_id}", put(set))
}

/// Open deals a teammate's row lists to close the gap: commit first, then best case, the biggest first.
const PATH_DEALS: i64 = 5;

/// A teammate's month, or the deals nobody owns when `user_id` is null. `commit_cents` and `best_case_cents` are open
/// deals the latest forecast of their project, for this month, put there. `deals` is up to five of those.
#[derive(Serialize, sqlx::FromRow)]
struct Row {
    user_id: Option<Uuid>,
    name: String,
    target_cents: Option<i64>,
    won_cents: i64,
    won_deals: i64,
    commit_cents: i64,
    best_case_cents: i64,
    deals: Value,
}

#[derive(Serialize)]
struct View {
    currency: String,
    /// Days into the month and the days it has, for the pace marker.
    day: i32,
    days: i32,
    rows: Vec<Row>,
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<View>> {
    let (currency, day, days): (String, i32, i32) = sqlx::query_as(
        "select currency, extract(day from now() at time zone 'utc')::int,
                extract(day from date_trunc('month', now() at time zone 'utc') + interval '1 month' - interval '1 day')::int
         from organizations where id = $1",
    )
    .bind(auth.org_id)
    .fetch_one(&state.db)
    .await?;
    let rows = sqlx::query_as::<_, Row>(
        "with placed as (
           select f.project_id, p.deal_id, p.category, p.why
           from (select distinct on (pf.project_id) pf.project_id, pf.forecast from pipeline_forecasts pf join projects pr on pr.id = pf.project_id
                 where pr.org_id = $1 and pf.forecast is not null order by pf.project_id, pf.created_at desc) f,
                jsonb_to_recordset(f.forecast->'deals') as p(deal_id uuid, category text, why text)
           where (f.forecast->>'month_end')::date >= (now() at time zone 'utc')::date),
         owned as (
           select l.owner_id, d.id, d.status, d.value_cents, d.closed_at, l.full_name, l.company_name, pl.category, pl.why
           from deals d join leads l on l.id = d.lead_id left join placed pl on pl.deal_id = d.id and pl.project_id = d.project_id
           where d.org_id = $1 and lower(d.currency) = lower($2)),
         people as (
           select m.user_id, coalesce(nullif(u.name, ''), u.email::text) as name, m.monthly_target_cents as target_cents
           from memberships m join users u on u.id = m.user_id where m.org_id = $1
           union all select null, 'Unassigned', null)
         select p.user_id, p.name, p.target_cents,
                coalesce(sum(d.value_cents) filter (where d.status = 'won' and d.closed_at >= date_trunc('month', now() at time zone 'utc') at time zone 'utc'), 0)::bigint as won_cents,
                count(d.id) filter (where d.status = 'won' and d.closed_at >= date_trunc('month', now() at time zone 'utc') at time zone 'utc') as won_deals,
                coalesce(sum(d.value_cents) filter (where d.status = 'open' and d.category = 'commit'), 0)::bigint as commit_cents,
                coalesce(sum(d.value_cents) filter (where d.status = 'open' and d.category = 'best_case'), 0)::bigint as best_case_cents,
                coalesce((select jsonb_agg(jsonb_build_object('deal_id', x.id, 'name', x.full_name, 'company', x.company_name,
                                                              'value_cents', x.value_cents, 'category', x.category, 'why', x.why))
                          from (select * from owned x where x.status = 'open' and x.category in ('commit', 'best_case')
                                  and x.owner_id is not distinct from p.user_id
                                order by x.category = 'commit' desc, x.value_cents desc limit $3) x), '[]') as deals
         from people p left join owned d on d.owner_id is not distinct from p.user_id
         group by p.user_id, p.name, p.target_cents
         having p.user_id is not null
             or count(d.id) filter (where (d.status = 'won' and d.closed_at >= date_trunc('month', now() at time zone 'utc') at time zone 'utc')
                                       or (d.status = 'open' and d.category in ('commit', 'best_case'))) > 0
         order by p.user_id is null, won_cents desc, p.name",
    )
    .bind(auth.org_id)
    .bind(&currency)
    .bind(PATH_DEALS)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(View { currency, day, days, rows }))
}

#[derive(Deserialize)]
struct Target {
    /// Null or 0 clears it.
    target_cents: Option<i64>,
}

/// Sets a teammate's monthly target. Owners and admins only.
async fn set(State(state): State<AppState>, auth: Auth, Path(user_id): Path<Uuid>, Json(body): Json<Target>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    let target = body.target_cents.filter(|c| *c > 0);
    if target.is_some_and(|c| c > 1_000_000_000_000) {
        return Err(AppError::BadRequest("That target is too big".into()));
    }
    let done = sqlx::query("update memberships set monthly_target_cents = $3 where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(user_id)
        .bind(target)
        .execute(&state.db)
        .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(serde_json::json!({ "target_cents": target })))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_teammate_sees_won_against_target_and_the_forecast_deals_that_close_the_gap(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name, currency) values ('Acme', 'usd') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(&db).await.unwrap();
        let mut users = vec![];
        for (name, role) in [("Dana", "owner"), ("Sam", "member")] {
            let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values (lower($1) || '@acme.test', $1, 'x') returning id")
                .bind(name)
                .fetch_one(&db)
                .await
                .unwrap();
            sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, $3)").bind(org).bind(user).bind(role).execute(&db).await.unwrap();
            users.push(user);
        }
        let (dana, sam) = (users[0], users[1]);
        let mut ids = vec![];
        // Dana won 6,000 this month and 9,000 last month, and has a commit, a best case and a later deal. Sam's won deal is in
        // euros, which doesn't count. Nobody owns one open deal.
        for (owner, status, cents, currency, closed) in [
            (Some(dana), "won", 600000, "usd", Some(0)),
            (Some(dana), "won", 900000, "usd", Some(40)),
            (Some(dana), "open", 300000, "usd", None),
            (Some(dana), "open", 800000, "usd", None),
            (Some(dana), "open", 50000, "usd", None),
            (Some(sam), "won", 700000, "eur", Some(0)),
            (None, "open", 100000, "usd", None),
        ] {
            let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, owner_id) values ($1, 'Lead', $2) returning id")
                .bind(project)
                .bind(owner)
                .fetch_one(&db)
                .await
                .unwrap();
            let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, gen_random_uuid()::text, $2, 0) returning id")
                .bind(org)
                .bind(status)
                .fetch_one(&db)
                .await
                .unwrap();
            let deal: Uuid = sqlx::query_scalar(
                "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, closed_at)
                 values ($1, $2, $3, $4, $5, $6, $7, now() - make_interval(days => $8)) returning id",
            )
            .bind(org)
            .bind(project)
            .bind(lead)
            .bind(stage)
            .bind(status)
            .bind(cents)
            .bind(currency)
            .bind(closed)
            .fetch_one(&db)
            .await
            .unwrap();
            ids.push(deal);
        }
        // Whatever day of the month the test runs on, the second won deal belongs to last month.
        sqlx::query("update deals set closed_at = date_trunc('month', now()) - interval '2 days' where id = $1").bind(ids[1]).execute(&db).await.unwrap();
        sqlx::query("update deals set closed_at = greatest(date_trunc('month', now()), now() - interval '1 minute') where id in ($1, $2)")
            .bind(ids[0])
            .bind(ids[5])
            .execute(&db)
            .await
            .unwrap();
        let month_end = sqlx::query_scalar::<_, chrono::NaiveDate>("select (date_trunc('month', now()) + interval '1 month - 1 day')::date").fetch_one(&db).await.unwrap();
        let forecast = json!({ "headline": "", "month_end": month_end, "focus": [], "slipping": [], "deals": [
            { "deal_id": ids[2], "category": "commit", "why": "Signing Friday" },
            { "deal_id": ids[3], "category": "best_case", "why": "Legal review" },
            { "deal_id": ids[4], "category": "later", "why": "" },
            { "deal_id": ids[6], "category": "commit", "why": "Verbal yes" }
        ]});
        sqlx::query("insert into pipeline_forecasts (project_id, forecast) values ($1, $2)").bind(project).bind(forecast).execute(&db).await.unwrap();

        let state = crate::autopilot::tests::state(db.clone());
        let auth = |user: Uuid, role: &str| Auth { user_id: user, org_id: org, role: role.into(), token_hash: String::new() };
        assert!(set(State(state.clone()), auth(sam, "member"), Path(sam), Json(Target { target_cents: Some(1) })).await.is_err(), "members can't set targets");
        let Json(saved) = set(State(state.clone()), auth(dana, "owner"), Path(dana), Json(Target { target_cents: Some(2_000_000) })).await.unwrap();
        assert_eq!(saved["target_cents"], 2_000_000);

        let Json(v) = list(State(state), auth(sam, "member")).await.unwrap();
        assert_eq!(v.currency, "usd");
        let rows: Vec<_> = v.rows.iter().map(|r| (r.name.as_str(), r.target_cents, r.won_cents, r.won_deals, r.commit_cents, r.best_case_cents)).collect();
        assert_eq!(rows, vec![("Dana", Some(2_000_000), 600000, 1, 300000, 800000), ("Sam", None, 0, 0, 0, 0), ("Unassigned", None, 0, 0, 100000, 0)]);
        let path: Vec<_> = v.rows[0].deals.as_array().unwrap().iter().map(|d| (d["category"].as_str().unwrap(), d["value_cents"].as_i64().unwrap())).collect();
        assert_eq!(path, vec![("commit", 300000), ("best_case", 800000)], "commit first, never a later deal");
    }
}
