//! The Customers page: every won deal in a project with what it paid, when you last talked, and where the agent's checks
//! on it stand (agent::churn, agent::expansions, agent::referrals).

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::AppResult;
use crate::models::Ts;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/customers", get(list))
}

/// A won deal as the Customers page lists it. Someone at the customer is the deal's lead or a lead with its company's
/// domain. `revenue_cents` is what they paid in the last 12 months in the deal's currency, refunds taken off. `risk` and
/// `risk_why` are from the latest finished check, and none once it's dropped rather than the one before it. `growth` is the
/// opening of an expansion draft waiting to be sent, and `asked` says whether the referral ask went out.
#[derive(Serialize, sqlx::FromRow)]
struct Customer {
    deal_id: Uuid,
    lead_id: Uuid,
    lead_name: String,
    company_name: String,
    company_domain: Option<String>,
    owner_name: Option<String>,
    value_cents: i64,
    currency: String,
    won_at: Option<Ts>,
    revenue_cents: i64,
    last_paid_at: Option<Ts>,
    last_contact_at: Option<Ts>,
    risk: Option<String>,
    risk_why: Option<String>,
    growth: Option<String>,
    asked: bool,
    page_views: i32,
    referred: i64,
}

/// The project's customers, the riskiest first, then the biggest.
async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Customer>>> {
    auth.project(&state.db, id).await?;
    let customers = sqlx::query_as::<_, Customer>(
        "select d.id as deal_id, d.lead_id, l.full_name as lead_name, l.company_name, l.company_domain::text, nullif(u.name, '') as owner_name,
                d.value_cents, d.currency, d.closed_at as won_at,
                coalesce(paid.cents, 0)::bigint as revenue_cents, paid.last as last_paid_at,
                (select max(coalesce(e.sent_at, e.created_at)) from email_messages e
                 where e.lead_id = d.lead_id and (e.direction = 'inbound' or e.status = 'sent')) as last_contact_at,
                risk.risk, risk.why as risk_why,
                (select x.why from deal_expansions x join email_messages xe on xe.id = x.email_id
                 where x.deal_id = d.id and x.dismissed_at is null and xe.status = 'draft' order by x.created_at desc limit 1) as growth,
                coalesce(re.status in ('approved', 'scheduled', 'sending', 'sent'), false) as asked,
                coalesce(f.views, 0) as page_views,
                (select count(*) from leads rl where rl.referral_id = f.id) as referred
         from deals d join leads l on l.id = d.lead_id
         left join users u on u.id = l.owner_id
         left join lateral (
           select sum(py.amount_cents) as cents, max(py.paid_at) filter (where py.kind = 'payment') as last
           from payments py join leads pl on pl.id = py.lead_id
           where py.project_id = d.project_id and py.paid_at > now() - interval '12 months' and lower(py.currency) = lower(d.currency)
             and (pl.id = d.lead_id or pl.company_domain = l.company_domain)) paid on true
         left join lateral (
           select case when c.dismissed_at is null then c.risk end as risk, case when c.dismissed_at is null then c.why end as why
           from churn_risks c where c.deal_id = d.id and c.risk is not null
           order by c.created_at desc limit 1) risk on true
         left join deal_referrals f on f.deal_id = d.id
         left join email_messages re on re.id = f.email_id
         where d.project_id = $1 and d.status = 'won'
         order by case risk.risk when 'high' then 0 when 'medium' then 1 else 2 end, d.value_cents desc, d.closed_at desc
         limit 500",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(customers))
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn customers_list_what_they_paid_and_where_the_checks_stand_riskiest_first(db: PgPool) {
        let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('dana@example.test', 'x') returning id").fetch_one(&db).await.unwrap();
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(&db).await.unwrap();
        let mut deals = vec![];
        for (name, domain, status, cents) in [("Ines", "lumenfold.example", "won", 480000), ("Bo", "brightline.example", "won", 900000), ("Cy", "copper.example", "lost", 100)] {
            let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, company_name, company_domain) values ($1, $2, $3, $3) returning id")
                .bind(project)
                .bind(name)
                .bind(domain)
                .fetch_one(&db)
                .await
                .unwrap();
            let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, $2, $3, 0) returning id")
                .bind(org)
                .bind(name)
                .bind(status)
                .fetch_one(&db)
                .await
                .unwrap();
            let deal: Uuid = sqlx::query_scalar(
                "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, closed_at) values ($1, $2, $3, $4, $5, $6, 'usd', now()) returning id",
            )
            .bind(org)
            .bind(project)
            .bind(lead)
            .bind(stage)
            .bind(status)
            .bind(cents)
            .fetch_one(&db)
            .await
            .unwrap();
            deals.push((deal, lead));
        }
        let (lumenfold, ines) = deals[0];
        let (brightline, bo) = deals[1];
        // Lumenfold paid through a colleague, got a refund, and paid in another currency and long ago, which don't count.
        let colleague: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, company_domain) values ($1, 'Ana', 'lumenfold.example') returning id")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        for (id, kind, cents, currency, days) in [("a", "payment", 40000, "usd", 10), ("b", "refund", -10000, "usd", 5), ("c", "payment", 5000, "eur", 3), ("d", "payment", 9999, "usd", 400)] {
            sqlx::query(
                "insert into payments (org_id, project_id, lead_id, provider, external_id, kind, amount_cents, currency, paid_at)
                 values ($1, $2, $3, 'stripe', $4, $5, $6, $7, now() - make_interval(days => $8))",
            )
            .bind(org)
            .bind(project)
            .bind(colleague)
            .bind(id)
            .bind(kind)
            .bind(cents)
            .bind(currency)
            .bind(days)
            .execute(&db)
            .await
            .unwrap();
        }
        sqlx::query("insert into churn_risks (project_id, deal_id, risk, why, written_at) values ($1, $2, 'high', 'Stopped paying.', now())")
            .bind(project)
            .bind(lumenfold)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, subject, body, status) values ($1, $2, 'inbound', 'inbound', 'Hi', 'Hi', 'received')")
            .bind(project)
            .bind(ines)
            .execute(&db)
            .await
            .unwrap();
        // Bo's latest check was dropped, so the older high risk doesn't come back.
        sqlx::query(
            "insert into churn_risks (project_id, deal_id, risk, why, written_at, created_at, dismissed_at)
             values ($1, $2, 'high', 'Old.', now(), now() - interval '9 days', null), ($1, $2, 'medium', 'Handled.', now(), now() - interval '2 days', now())",
        )
        .bind(project)
        .bind(brightline)
        .execute(&db)
        .await
        .unwrap();
        let draft: Uuid = sqlx::query_scalar("insert into email_messages (project_id, lead_id, direction, kind, subject, body, status) values ($1, $2, 'outbound', 'expansion', 'Seats', 'Hi', 'draft') returning id")
            .bind(project)
            .bind(bo)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into deal_expansions (project_id, deal_id, email_id, why, written_at) values ($1, $2, $3, 'Hiring 4 AEs.', now())")
            .bind(project)
            .bind(brightline)
            .bind(draft)
            .execute(&db)
            .await
            .unwrap();
        let sent: Uuid = sqlx::query_scalar("insert into email_messages (project_id, lead_id, direction, kind, subject, body, status) values ($1, $2, 'outbound', 'referral_ask', 'Intro', 'Hi', 'sent') returning id")
            .bind(project)
            .bind(bo)
            .fetch_one(&db)
            .await
            .unwrap();
        let referral: Uuid = sqlx::query_scalar("insert into deal_referrals (project_id, deal_id, email_id, views, written_at) values ($1, $2, $3, 3, now()) returning id")
            .bind(project)
            .bind(brightline)
            .bind(sent)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into leads (project_id, full_name, source, referral_id) values ($1, 'Lea', 'referral', $2)").bind(project).bind(referral).execute(&db).await.unwrap();

        let state = crate::autopilot::tests::state(db.clone());
        let auth = Auth { user_id: user, org_id: org, role: "owner".into(), token_hash: String::new() };
        let Json(list) = list(State(state), auth, Path(project)).await.unwrap();
        let rows: Vec<_> = list.iter().map(|c| (c.lead_name.as_str(), c.revenue_cents, c.risk.as_deref(), c.growth.as_deref(), c.asked, c.page_views, c.referred)).collect();
        assert_eq!(rows, vec![("Ines", 30000, Some("high"), None, false, 0, 0), ("Bo", 0, None, Some("Hiring 4 AEs."), true, 3, 1)], "the lost deal is left out");
        assert!(list[0].last_contact_at.is_some() && list[0].last_paid_at.is_some() && list[1].last_paid_at.is_none());
    }
}
