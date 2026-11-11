//! The built-in deals pipeline, for teams without a CRM: per-organization stages, and at most one open deal per lead. A hot
//! reply opens a deal and a booked meeting moves it on, so nobody has to drag those cards. routes/deals.rs serves the board,
//! and the public API and MCP call the same functions.

use std::collections::HashSet;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::models::{Lead, Ts};
use crate::routes::util_input;
use crate::state::AppState;

pub const MAX_STAGES: usize = 10;
const MAX_NAME: usize = 40;
/// Won and lost columns on the board, and the win rate, look back this far.
const CLOSED_DAYS: i32 = 90;
/// More than a board can usefully show. The list stays complete through the API's pages.
const BOARD_LIMIT: i64 = 2000;

/// What every organization starts with: name, kind, probability and the automatic move that lands there.
const DEFAULT_STAGES: [(&str, &str, i32, Option<&str>); 6] = [
    ("Interested", "open", 10, Some("hot")),
    ("Meeting booked", "open", 30, Some("booked")),
    ("Proposal", "open", 50, None),
    ("Negotiation", "open", 70, None),
    ("Won", "won", 100, None),
    ("Lost", "lost", 0, None),
];

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Stage {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub position: i32,
    pub probability: i32,
    pub auto: Option<String>,
    /// Every deal in the stage, however long ago it closed.
    pub deals: i64,
}

const STAGE_SELECT: &str = "select s.id, s.name, s.kind, s.position, s.probability, s.auto,
       (select count(*) from deals d where d.stage_id = s.id) as deals
     from deal_stages s where s.org_id = $1";

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Deal {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_name: String,
    pub lead_id: Uuid,
    pub lead_name: String,
    pub lead_email: Option<String>,
    pub lead_title: String,
    pub company_name: String,
    pub company_domain: Option<String>,
    pub stage_id: Uuid,
    pub stage: String,
    pub status: String,
    /// In the currency's main unit, like 1200.5 for $1,200.50.
    #[serde(rename = "value", serialize_with = "major_units")]
    pub value_cents: i64,
    pub currency: String,
    pub close_date: Option<NaiveDate>,
    pub owner_id: Option<Uuid>,
    pub owner_name: Option<String>,
    pub notes: String,
    pub lost_reason: String,
    pub source: String,
    pub stage_changed_at: Ts,
    pub closed_at: Option<Ts>,
    pub created_at: Ts,
}

const DEAL_SELECT: &str = "select d.id, d.project_id, p.name as project_name, d.lead_id, l.full_name as lead_name, l.email::text as lead_email,
       l.title as lead_title, l.company_name, l.company_domain::text as company_domain, d.stage_id, s.name as stage, d.status, d.value_cents,
       d.currency, d.close_date, d.owner_id, coalesce(nullif(u.name, ''), u.email::text) as owner_name, d.notes, d.lost_reason, d.source,
       d.stage_changed_at, d.closed_at, d.created_at
     from deals d join deal_stages s on s.id = d.stage_id join leads l on l.id = d.lead_id join projects p on p.id = d.project_id
     left join users u on u.id = d.owner_id";

fn major_units<S: Serializer>(cents: &i64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(*cents as f64 / 100.0)
}

/// Creates the default stages the first time the organization needs a pipeline. The flag makes it once: a second request
/// racing this one waits on the organization's row lock and then finds the flag set.
pub async fn ensure_stages(db: &PgPool, org_id: Uuid) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    let fresh = sqlx::query("update organizations set pipeline_seeded = true where id = $1 and not pipeline_seeded").bind(org_id).execute(&mut *tx).await?;
    if fresh.rows_affected() == 1 {
        for (position, (name, kind, probability, auto)) in DEFAULT_STAGES.into_iter().enumerate() {
            sqlx::query("insert into deal_stages (org_id, name, kind, position, probability, auto) values ($1, $2, $3, $4, $5, $6)")
                .bind(org_id)
                .bind(name)
                .bind(kind)
                .bind(position as i32)
                .bind(probability)
                .bind(auto)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await
}

pub async fn stages(db: &PgPool, org_id: Uuid) -> sqlx::Result<Vec<Stage>> {
    ensure_stages(db, org_id).await?;
    sqlx::query_as(&format!("{STAGE_SELECT} order by s.position")).bind(org_id).fetch_all(db).await
}

async fn org_currency(db: &PgPool, org_id: Uuid) -> sqlx::Result<String> {
    sqlx::query_scalar("select currency from organizations where id = $1").bind(org_id).fetch_one(db).await
}

pub async fn get(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<Deal> {
    sqlx::query_as(&format!("{DEAL_SELECT} where d.id = $1 and d.org_id = $2"))
        .bind(id)
        .bind(org_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// The deal as this transaction sees it, so an event says what this change did and not what a later one did.
async fn get_in(conn: &mut PgConnection, org_id: Uuid, id: Uuid) -> sqlx::Result<Deal> {
    sqlx::query_as(&format!("{DEAL_SELECT} where d.id = $1 and d.org_id = $2")).bind(id).bind(org_id).fetch_one(conn).await
}

/// What kind the stage is now, holding it against stage edits until the transaction ends. Every path that puts a deal in a
/// stage takes this before the deal's row, like save_stages, so the two can't deadlock.
async fn lock_stage(conn: &mut PgConnection, org_id: Uuid, id: Uuid) -> AppResult<String> {
    sqlx::query_scalar("select kind from deal_stages where id = $1 and org_id = $2 for share")
        .bind(id)
        .bind(org_id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| AppError::Conflict("That stage was just removed. Reload and try again".into()))
}

/// The lead's open deal, or else the one that closed last, for the lead window and the inbox.
pub async fn for_lead(db: &PgPool, lead_id: Uuid) -> sqlx::Result<Option<Deal>> {
    sqlx::query_as(&format!("{DEAL_SELECT} where d.lead_id = $1 order by d.status = 'open' desc, d.closed_at desc nulls last limit 1"))
        .bind(lead_id)
        .fetch_optional(db)
        .await
}

#[derive(Debug, Default, Deserialize)]
pub struct DealFilter {
    pub project_id: Option<Uuid>,
    pub status: Option<String>,
    pub stage_id: Option<Uuid>,
    pub owner_id: Option<Uuid>,
    pub lead_id: Option<Uuid>,
}

/// A page of the organization's deals, the latest first, and how many match.
pub async fn list(db: &PgPool, org_id: Uuid, f: &DealFilter, limit: i64, offset: i64) -> AppResult<(Vec<Deal>, i64)> {
    if let Some(status) = &f.status {
        util_input::one_of("status", status, &["open", "won", "lost"])?;
    }
    let filter = "where d.org_id = $1 and ($2::uuid is null or d.project_id = $2) and ($3::text is null or d.status = $3)
       and ($4::uuid is null or d.stage_id = $4) and ($5::uuid is null or d.owner_id = $5) and ($6::uuid is null or d.lead_id = $6)";
    let total: i64 = sqlx::query_scalar(&format!("select count(*) from deals d {filter}"))
        .bind(org_id)
        .bind(f.project_id)
        .bind(&f.status)
        .bind(f.stage_id)
        .bind(f.owner_id)
        .bind(f.lead_id)
        .fetch_one(db)
        .await?;
    let deals = sqlx::query_as(&format!("{DEAL_SELECT} {filter} order by d.created_at desc, d.id limit $7 offset $8"))
        .bind(org_id)
        .bind(f.project_id)
        .bind(&f.status)
        .bind(f.stage_id)
        .bind(f.owner_id)
        .bind(f.lead_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(db)
        .await?;
    Ok((deals, total))
}

/// The forecast's sums for deals in one currency, in its main unit.
#[derive(Debug, Serialize, FromRow)]
pub struct Amounts {
    pub currency: String,
    #[serde(serialize_with = "major_units")]
    pub open: i64,
    /// Each open deal's value times its stage's probability.
    #[serde(serialize_with = "major_units")]
    pub weighted: i64,
    #[serde(serialize_with = "major_units")]
    pub won_this_month: i64,
}

#[derive(Debug, Serialize)]
pub struct Forecast {
    /// One row per currency the deals are priced in, the organization's own first.
    pub amounts: Vec<Amounts>,
    /// Deals won and lost in the last 90 days, and the share of those that were won. None when none closed.
    pub won: i64,
    pub lost: i64,
    pub win_rate: Option<f64>,
}

/// "This month" is the calendar month in UTC.
pub async fn forecast(db: &PgPool, org_id: Uuid, project_id: Option<Uuid>) -> sqlx::Result<Forecast> {
    let amounts = sqlx::query_as(
        "select d.currency,
           coalesce(sum(d.value_cents) filter (where d.status = 'open'), 0)::bigint as open,
           coalesce(round(sum(d.value_cents * s.probability) filter (where d.status = 'open') / 100.0), 0)::bigint as weighted,
           coalesce(sum(d.value_cents) filter (where d.status = 'won' and d.closed_at >= date_trunc('month', now())), 0)::bigint as won_this_month
         from deals d join deal_stages s on s.id = d.stage_id
         where d.org_id = $1 and ($2::uuid is null or d.project_id = $2)
         group by d.currency
         order by d.currency = (select currency from organizations where id = $1) desc, d.currency",
    )
    .bind(org_id)
    .bind(project_id)
    .fetch_all(db)
    .await?;
    let (won, lost): (i64, i64) = sqlx::query_as(
        "select count(*) filter (where status = 'won'), count(*) filter (where status = 'lost') from deals
         where org_id = $1 and ($2::uuid is null or project_id = $2) and closed_at > now() - make_interval(days => $3)",
    )
    .bind(org_id)
    .bind(project_id)
    .bind(CLOSED_DAYS)
    .fetch_one(db)
    .await?;
    let win_rate = (won + lost > 0).then(|| won as f64 / (won + lost) as f64);
    Ok(Forecast { amounts, won, lost, win_rate })
}

#[derive(Debug, Serialize)]
pub struct Board {
    /// What new deals are priced in.
    pub currency: String,
    pub stages: Vec<Stage>,
    /// Every open deal, and those that closed in the last 90 days.
    pub deals: Vec<Deal>,
    pub forecast: Forecast,
}

pub async fn board(db: &PgPool, org_id: Uuid, project_id: Option<Uuid>) -> AppResult<Board> {
    let stages = stages(db, org_id).await?;
    let deals = sqlx::query_as(&format!(
        "{DEAL_SELECT} where d.org_id = $1 and ($2::uuid is null or d.project_id = $2)
           and (d.status = 'open' or d.closed_at > now() - make_interval(days => $3))
         order by d.stage_changed_at desc limit $4"
    ))
    .bind(org_id)
    .bind(project_id)
    .bind(CLOSED_DAYS)
    .bind(BOARD_LIMIT)
    .fetch_all(db)
    .await?;
    Ok(Board { currency: org_currency(db, org_id).await?, stages, deals, forecast: forecast(db, org_id, project_id).await? })
}

/// A value in the currency's main unit as cents. None stays None, so a move or an edit can leave the value alone.
fn cents(value: Option<f64>) -> AppResult<Option<i64>> {
    match value {
        Some(v) if !v.is_finite() || !(0.0..1e12).contains(&v) => Err(AppError::BadRequest("The value must be a positive amount under a trillion".into())),
        Some(v) => Ok(Some((v * 100.0).round() as i64)),
        None => Ok(None),
    }
}

pub fn currency(raw: &str) -> AppResult<String> {
    let code = raw.trim().to_ascii_uppercase();
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(AppError::BadRequest("Give the currency as a three-letter code, like USD or EUR".into()));
    }
    Ok(code)
}

fn text(field: &str, raw: Option<&str>, max: usize) -> AppResult<Option<String>> {
    let value = raw.map(str::trim);
    if value.is_some_and(|v| v.chars().count() > max) {
        return Err(AppError::BadRequest(format!("Keep the {field} under {max} characters")));
    }
    Ok(value.map(str::to_string))
}

async fn check_owner(db: &PgPool, org_id: Uuid, owner_id: Option<Uuid>) -> AppResult<()> {
    let Some(owner_id) = owner_id else { return Ok(()) };
    let member: bool = sqlx::query_scalar("select exists (select 1 from memberships where org_id = $1 and user_id = $2)")
        .bind(org_id)
        .bind(owner_id)
        .fetch_one(db)
        .await?;
    if !member {
        return Err(AppError::BadRequest("The owner has to be on your team".into()));
    }
    Ok(())
}

/// The one-open-deal index turned an insert or a reopen away.
fn one_open(e: sqlx::Error) -> AppError {
    match e.as_database_error().and_then(|d| d.constraint()) {
        Some("deals_one_open_per_lead") => AppError::Conflict("This lead already has an open deal".into()),
        _ => e.into(),
    }
}

/// A stage of the organization picked by id or by name, any case, which reads better for an MCP client.
async fn find_stage(db: &PgPool, org_id: Uuid, id: Option<Uuid>, name: Option<&str>) -> AppResult<Option<Stage>> {
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    if id.is_none() && name.is_none() {
        return Ok(None);
    }
    ensure_stages(db, org_id).await?;
    let stage = sqlx::query_as::<_, Stage>(&format!("{STAGE_SELECT} and (s.id = $2 or ($2 is null and lower(s.name) = lower($3))) limit 1"))
        .bind(org_id)
        .bind(id)
        .bind(name)
        .fetch_optional(db)
        .await?;
    stage.map(Some).ok_or_else(|| AppError::BadRequest("There's no stage with that id or name".into()))
}

/// Where an automatic deal goes: the open stage marked `auto`, or the first open stage when that one was removed.
async fn auto_stage(db: &PgPool, org_id: Uuid, auto: &str) -> sqlx::Result<Option<(Uuid, i32)>> {
    sqlx::query_as("select id, position from deal_stages where org_id = $1 and kind = 'open' order by auto is not distinct from $2 desc, position limit 1")
        .bind(org_id)
        .bind(auto)
        .fetch_optional(db)
        .await
}

#[derive(Debug, Default, Deserialize)]
pub struct NewDeal {
    pub lead_id: Uuid,
    pub stage_id: Option<Uuid>,
    /// A stage name, for clients that don't keep ids.
    pub stage: Option<String>,
    pub value: Option<f64>,
    pub currency: Option<String>,
    pub close_date: Option<NaiveDate>,
    pub owner_id: Option<Uuid>,
    pub notes: Option<String>,
}

/// A deal someone started by hand or through the API, in the stage they picked or where hot replies start.
pub async fn create(state: &AppState, org_id: Uuid, by: Option<Uuid>, source: &str, input: NewDeal) -> AppResult<Deal> {
    let db = &state.db;
    let project_id: Uuid = sqlx::query_scalar("select l.project_id from leads l join projects p on p.id = l.project_id where l.id = $1 and p.org_id = $2")
        .bind(input.lead_id)
        .bind(org_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)?;
    ensure_stages(db, org_id).await?;
    let stage = match find_stage(db, org_id, input.stage_id, input.stage.as_deref()).await? {
        Some(stage) => stage.id,
        None => auto_stage(db, org_id, "hot").await?.ok_or(AppError::NotFound)?.0,
    };
    let currency = match &input.currency {
        Some(code) => currency(code)?,
        None => org_currency(db, org_id).await?,
    };
    check_owner(db, org_id, input.owner_id).await?;
    let notes = util_input::notes(input.notes.as_deref())?.unwrap_or_default();
    let mut tx = db.begin().await?;
    let kind = lock_stage(&mut tx, org_id, stage).await?;
    let id: Uuid = sqlx::query_scalar(
        "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, close_date, owner_id, notes, source, created_by, closed_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, case when $5 = 'open' then null else now() end)
         returning id",
    )
    .bind(org_id)
    .bind(project_id)
    .bind(input.lead_id)
    .bind(stage)
    .bind(&kind)
    .bind(cents(input.value)?.unwrap_or(0))
    .bind(currency)
    .bind(input.close_date)
    .bind(input.owner_id)
    .bind(notes)
    .bind(source)
    .bind(by)
    .fetch_one(&mut *tx)
    .await
    .map_err(one_open)?;
    let deal = get_in(&mut tx, org_id, id).await?;
    tx.commit().await?;
    announce(state, &deal, "deal.created", None).await;
    Ok(deal)
}

/// Sets a field when it's present. A present null clears it.
fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Default, Deserialize)]
pub struct DealChanges {
    pub value: Option<f64>,
    pub currency: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub close_date: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "nullable")]
    pub owner_id: Option<Option<Uuid>>,
    pub notes: Option<String>,
    pub lost_reason: Option<String>,
}

pub async fn update(db: &PgPool, org_id: Uuid, id: Uuid, c: DealChanges) -> AppResult<Deal> {
    let currency = c.currency.as_deref().map(currency).transpose()?;
    if let Some(owner) = c.owner_id {
        check_owner(db, org_id, owner).await?;
    }
    let notes = util_input::notes(c.notes.as_deref())?;
    let reason = text("reason", c.lost_reason.as_deref(), 500)?;
    let found = sqlx::query(
        "update deals set value_cents = coalesce($3, value_cents), currency = coalesce($4, currency),
           close_date = case when $5::bool then $6::date else close_date end, owner_id = case when $7::bool then $8::uuid else owner_id end,
           notes = coalesce($9, notes), lost_reason = coalesce($10, lost_reason)
         where id = $1 and org_id = $2",
    )
    .bind(id)
    .bind(org_id)
    .bind(cents(c.value)?)
    .bind(currency)
    .bind(c.close_date.is_some())
    .bind(c.close_date.flatten())
    .bind(c.owner_id.is_some())
    .bind(c.owner_id.flatten())
    .bind(notes)
    .bind(reason)
    .execute(db)
    .await?;
    if found.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    get(db, org_id, id).await
}

pub async fn delete(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<()> {
    let found = sqlx::query("delete from deals where id = $1 and org_id = $2").bind(id).bind(org_id).execute(db).await?;
    if found.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
pub struct Move {
    pub stage_id: Option<Uuid>,
    /// A stage name, for clients that don't keep ids.
    pub stage: Option<String>,
    /// The final value, when it's won.
    pub value: Option<f64>,
    pub lost_reason: Option<String>,
}

/// Moves a deal to any stage, back or forward, which is what a person dragging a card means. Won or lost closes it, an
/// open stage reopens it.
pub async fn move_to(state: &AppState, org_id: Uuid, id: Uuid, m: Move) -> AppResult<Deal> {
    let db = &state.db;
    let to = find_stage(db, org_id, m.stage_id, m.stage.as_deref()).await?.ok_or_else(|| AppError::BadRequest("Pick a stage with stage_id or stage".into()))?;
    let value = cents(m.value)?;
    let reason = text("reason", m.lost_reason.as_deref(), 500)?;
    let mut tx = db.begin().await?;
    let kind = lock_stage(&mut tx, org_id, to.id).await?;
    // The row lock makes two people dropping the same card at once take turns, so the second sees where the first left it.
    let (from_stage, from_status, from_name): (Uuid, String, String) = sqlx::query_as(
        "select d.stage_id, d.status, s.name from deals d join deal_stages s on s.id = d.stage_id where d.id = $1 and d.org_id = $2 for update of d",
    )
    .bind(id)
    .bind(org_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    sqlx::query(
        "update deals set stage_id = $2, status = $3,
           stage_changed_at = case when stage_id = $2 then stage_changed_at else now() end,
           closed_at = case when $3 = 'open' then null when status = $3 then closed_at else now() end,
           value_cents = coalesce($4, value_cents),
           lost_reason = case when $3 = 'lost' then coalesce($5, lost_reason) else '' end
         where id = $1",
    )
    .bind(id)
    .bind(to.id)
    .bind(&kind)
    .bind(value)
    .bind(reason)
    .execute(&mut *tx)
    .await
    .map_err(one_open)?;
    let deal = get_in(&mut tx, org_id, id).await?;
    tx.commit().await?;
    if from_stage != to.id {
        announce(state, &deal, "deal.stage_changed", Some(&from_name)).await;
    }
    if from_status != kind {
        match kind.as_str() {
            "won" => announce(state, &deal, "deal.won", Some(&from_name)).await,
            "lost" => announce(state, &deal, "deal.lost", Some(&from_name)).await,
            _ => {}
        }
    }
    Ok(deal)
}

async fn lead_org(db: &PgPool, lead: &Lead) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar("select org_id from projects where id = $1").bind(lead.project_id).fetch_one(db).await
}

/// Opens a deal for a lead with none yet, in `stage`. A lead whose deal closed doesn't get another, and the partial
/// unique index turns a second event racing this one into a no-op.
async fn open_automatically(db: &PgPool, org_id: Uuid, lead: &Lead, stage: Uuid, source: &str) -> AppResult<Option<Deal>> {
    let mut tx = db.begin().await?;
    if lock_stage(&mut tx, org_id, stage).await? != "open" {
        return Ok(None);
    }
    let id: Option<Uuid> = sqlx::query_scalar(
        "insert into deals (org_id, project_id, lead_id, stage_id, currency, source)
         select $1, $2, $3, $4, o.currency, $5 from organizations o
         where o.id = $1 and not exists (select 1 from deals where lead_id = $3)
         on conflict (lead_id) where status = 'open' do nothing
         returning id",
    )
    .bind(org_id)
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(stage)
    .bind(source)
    .fetch_optional(&mut *tx)
    .await?;
    let deal = match id {
        Some(id) => Some(get_in(&mut tx, org_id, id).await?),
        None => None,
    };
    tx.commit().await?;
    Ok(deal)
}

/// A reply was classified hot. Failures are logged, they never fail the run.
pub async fn hot_reply(state: &AppState, lead: &Lead) {
    if let Err(e) = open_for_hot_reply(state, lead).await {
        tracing::warn!(error = ?e, lead = %lead.id, "couldn't open a deal for the hot reply");
    }
}

async fn open_for_hot_reply(state: &AppState, lead: &Lead) -> AppResult<()> {
    let Some(org_id) = lead_org(&state.db, lead).await? else { return Ok(()) };
    ensure_stages(&state.db, org_id).await?;
    let Some((stage, _)) = auto_stage(&state.db, org_id, "hot").await? else { return Ok(()) };
    if let Some(deal) = open_automatically(&state.db, org_id, lead, stage, "hot_reply").await? {
        announce(state, &deal, "deal.created", None).await;
    }
    Ok(())
}

/// The lead booked a meeting. Failures are logged, they never fail the booking.
pub async fn booked(state: &AppState, lead: &Lead) {
    if let Err(e) = advance_for_booking(state, lead).await {
        tracing::warn!(error = ?e, lead = %lead.id, "couldn't move the deal for the booking");
    }
}

/// Moves the lead's open deal to the booked stage when it's in an earlier one, and never back from a later one or out of
/// won or lost. A lead without a deal gets one there.
async fn advance_for_booking(state: &AppState, lead: &Lead) -> AppResult<()> {
    let db = &state.db;
    let Some(org_id) = lead_org(db, lead).await? else { return Ok(()) };
    ensure_stages(db, org_id).await?;
    let Some((target, _)) = auto_stage(db, org_id, "booked").await? else { return Ok(()) };
    // Twice, for a hot reply that opens the deal between the look and the insert: the insert loses, and the second pass
    // finds that deal and moves it.
    for _ in 0..2 {
        let mut tx = db.begin().await?;
        if lock_stage(&mut tx, org_id, target).await? != "open" {
            return Ok(());
        }
        // Read under the lock, since a reorder that committed before it changes what forward means.
        let position: i32 = sqlx::query_scalar("select position from deal_stages where id = $1").bind(target).fetch_one(&mut *tx).await?;
        let open: Option<(Uuid, i32, String)> = sqlx::query_as(
            "select d.id, s.position, s.name from deals d join deal_stages s on s.id = d.stage_id
             where d.lead_id = $1 and d.status = 'open' for update of d",
        )
        .bind(lead.id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((id, at, from)) = open {
            if at < position {
                sqlx::query("update deals set stage_id = $2, stage_changed_at = now() where id = $1").bind(id).bind(target).execute(&mut *tx).await?;
                let deal = get_in(&mut tx, org_id, id).await?;
                tx.commit().await?;
                announce(state, &deal, "deal.stage_changed", Some(&from)).await;
            }
            return Ok(());
        }
        drop(tx);
        if let Some(deal) = open_automatically(db, org_id, lead, target, "booking").await? {
            announce(state, &deal, "deal.created", None).await;
            return Ok(());
        }
    }
    Ok(())
}

/// Sends a deal event to the project's webhooks, with the lead like the lead events and the deal under "deal".
async fn announce(state: &AppState, deal: &Deal, event: &'static str, previous_stage: Option<&str>) {
    let lead: Option<Lead> = sqlx::query_as("select * from leads where id = $1").bind(deal.lead_id).fetch_optional(&state.db).await.unwrap_or_default();
    let Some(lead) = lead else { return };
    hooks::send_lead_event(state, deal.project_id, event, &lead, Some(("deal", payload(&state.cfg.public_url, deal, previous_stage)))).await;
}

/// Keep in sync with the deal in PAYLOAD on the integrations page.
fn payload(public_url: &str, deal: &Deal, previous_stage: Option<&str>) -> Value {
    json!({
        "id": deal.id, "stage": deal.stage, "previous_stage": previous_stage, "status": deal.status,
        "value": deal.value_cents as f64 / 100.0, "currency": deal.currency, "close_date": deal.close_date,
        "owner": deal.owner_name, "lost_reason": deal.lost_reason, "url": format!("{public_url}/app/pipeline?deal={}", deal.id),
    })
}

/// "Deal in Splee: Proposal, 12,000 USD", added to the CRM note about the lead, or None when the lead has no deal.
pub async fn crm_line(db: &PgPool, lead_id: Uuid) -> Option<String> {
    let deal = for_lead(db, lead_id).await.ok()??;
    Some(format!("Deal in Splee: {}, {} {}", deal.stage, amount(deal.value_cents), deal.currency))
}

/// 1200000 cents as "12,000", and 1200050 as "12,000.50".
fn amount(cents: i64) -> String {
    let whole = (cents / 100).to_string();
    let mut grouped = String::new();
    for (i, digit) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if cents % 100 == 0 { grouped } else { format!("{grouped}.{:02}", cents % 100) }
}

#[derive(Debug, Deserialize)]
pub struct StageInput {
    /// None for a new stage.
    pub id: Option<Uuid>,
    pub name: String,
    pub kind: String,
    /// Ignored for won (100) and lost (0) stages.
    #[serde(default)]
    pub probability: i32,
}

/// Where the deals of a removed stage go.
#[derive(Debug, Deserialize)]
pub struct StageMove {
    pub from: Uuid,
    pub to: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct PipelineBody {
    pub currency: Option<String>,
    /// In board order. A saved stage missing from the list is removed.
    pub stages: Vec<StageInput>,
    #[serde(default)]
    pub moves: Vec<StageMove>,
}

/// Saves the stage list in one go. A stage with deals can't change between open, won and lost, which would close or
/// reopen them unseen, and can only be removed once its deals have somewhere to go: a kept stage of the same kind.
pub async fn save_stages(state: &AppState, org_id: Uuid, body: PipelineBody) -> AppResult<()> {
    let db = &state.db;
    if body.stages.is_empty() || body.stages.len() > MAX_STAGES {
        return Err(AppError::BadRequest(format!("Keep between 1 and {MAX_STAGES} stages")));
    }
    let (mut names, mut ids) = (HashSet::new(), HashSet::new());
    for s in &body.stages {
        if s.id.is_some_and(|id| !ids.insert(id)) {
            return Err(AppError::BadRequest("A stage is in the list twice".into()));
        }
        util_input::one_of("kind", &s.kind, &["open", "won", "lost"])?;
        let name = s.name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME {
            return Err(AppError::BadRequest(format!("Give each stage a name under {MAX_NAME} characters")));
        }
        if !names.insert(name.to_lowercase()) {
            return Err(AppError::BadRequest(format!("Two stages are called {name}")));
        }
        if !(0..=100).contains(&s.probability) {
            return Err(AppError::BadRequest("Probabilities go from 0 to 100".into()));
        }
    }
    for kind in ["open", "won", "lost"] {
        if !body.stages.iter().any(|s| s.kind == kind) {
            return Err(AppError::BadRequest(format!("Keep at least one {kind} stage")));
        }
    }
    let currency = body.currency.as_deref().map(currency).transpose()?;

    ensure_stages(db, org_id).await?;
    let mut tx = db.begin().await?;
    // Locked, so deals can't land in a stage while it's being removed or changing kind. The counts come after, in their own
    // statement, so they include a deal that landed while this waited for the lock.
    sqlx::query("select id from deal_stages where org_id = $1 for update").bind(org_id).execute(&mut *tx).await?;
    let saved: Vec<(Uuid, String, String, i64)> = sqlx::query_as(
        "select s.id, s.name, s.kind, (select count(*) from deals d where d.stage_id = s.id) from deal_stages s where s.org_id = $1",
    )
    .bind(org_id)
    .fetch_all(&mut *tx)
    .await?;
    let kept = |id: Uuid| body.stages.iter().find(|s| s.id == Some(id));
    let mut moved: Vec<(Uuid, String)> = Vec::new();
    for s in &body.stages {
        let Some(id) = s.id else { continue };
        let Some((_, name, kind, deals)) = saved.iter().find(|(saved, ..)| *saved == id) else {
            return Err(AppError::BadRequest("A stage in the list isn't one of yours".into()));
        };
        if *kind != s.kind && *deals > 0 {
            return Err(AppError::Conflict(format!("Move the deals out of {name} before changing whether it's open, won or lost")));
        }
    }
    for (id, name, kind, deals) in saved.iter().filter(|(id, ..)| kept(*id).is_none()) {
        if *deals > 0 {
            let to = body.moves.iter().find(|m| m.from == *id).map(|m| m.to).filter(|to| kept(*to).is_some_and(|s| s.kind == *kind));
            let Some(to) = to else {
                return Err(AppError::Conflict(format!("Pick another {kind} stage for the deals in {name} before removing it")));
            };
            let ids: Vec<Uuid> = sqlx::query_scalar("update deals set stage_id = $2, stage_changed_at = now() where stage_id = $1 returning id")
                .bind(id)
                .bind(to)
                .fetch_all(&mut *tx)
                .await?;
            moved.extend(ids.into_iter().map(|deal| (deal, name.clone())));
        }
        sqlx::query("delete from deal_stages where id = $1").bind(id).execute(&mut *tx).await?;
    }
    for (position, s) in body.stages.iter().enumerate() {
        let probability = match s.kind.as_str() {
            "won" => 100,
            "lost" => 0,
            _ => s.probability,
        };
        let name = s.name.trim();
        match s.id {
            Some(id) => {
                sqlx::query(
                    "update deal_stages set name = $2, kind = $3, position = $4, probability = $5, auto = case when $3 = 'open' then auto end where id = $1",
                )
                .bind(id)
                .bind(name)
                .bind(&s.kind)
                .bind(position as i32)
                .bind(probability)
                .execute(&mut *tx)
                .await?;
            }
            None => {
                sqlx::query("insert into deal_stages (org_id, name, kind, position, probability) values ($1, $2, $3, $4, $5)")
                    .bind(org_id)
                    .bind(name)
                    .bind(&s.kind)
                    .bind(position as i32)
                    .bind(probability)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }
    if let Some(currency) = currency {
        sqlx::query("update organizations set currency = $2 where id = $1").bind(org_id).bind(currency).execute(&mut *tx).await?;
    }
    let mut events = Vec::with_capacity(moved.len());
    for (id, from) in moved {
        events.push((get_in(&mut tx, org_id, id).await?, from));
    }
    tx.commit().await?;
    for (deal, from) in events {
        announce(state, &deal, "deal.stage_changed", Some(&from)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::Value;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;
    use crate::config::Config;

    fn state(db: PgPool) -> AppState {
        let cfg = Config {
            database_url: String::new(),
            app_secret: String::new(),
            public_url: "https://app.test".into(),
            unsubscribe_url: "https://app.test".into(),
            bind_addr: String::new(),
            agent_model: String::new(),
            agent_concurrency: 0,
            sending_enabled: false,
            bird_bin: None,
            open_signup: true,
            public_agent: false,
            porkbun_api_url: String::new(),
            twilio_api_url: String::new(),
            bots_api_url: String::new(),
            outreach_api_url: String::new(),
            postmaster_api_url: String::new(),
            operator_emails: Vec::new(),
            google: None,
            microsoft: None,
            slack: None,
            tumblr: None,
            ipinfo_token: None,
        };
        AppState { db, cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(16).0 }
    }

    async fn org(db: &PgPool, name: &str) -> (Uuid, Uuid) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(db).await.unwrap();
        (org, project)
    }

    async fn lead(db: &PgPool, project: Uuid, name: &str) -> Lead {
        sqlx::query_as("insert into leads (project_id, full_name, email, company_name, status) values ($1, $2, $3, 'Northwind', 'hot') returning *")
            .bind(project)
            .bind(name)
            .bind(format!("{}@northwind.example", name.to_lowercase()))
            .fetch_one(db)
            .await
            .unwrap()
    }

    async fn stage_id(db: &PgPool, org: Uuid, name: &str) -> Uuid {
        sqlx::query_scalar("select id from deal_stages where org_id = $1 and name = $2").bind(org).bind(name).fetch_one(db).await.unwrap()
    }

    /// The lead's deals as (stage, status), oldest first.
    async fn deals_of(db: &PgPool, lead: &Lead) -> Vec<(String, String)> {
        sqlx::query_as("select s.name, d.status from deals d join deal_stages s on s.id = d.stage_id where d.lead_id = $1 order by d.created_at")
            .bind(lead.id)
            .fetch_all(db)
            .await
            .unwrap()
    }

    fn to(stage: &str) -> Move {
        Move { stage: Some(stage.into()), ..Default::default() }
    }

    fn new_deal(lead: &Lead) -> NewDeal {
        NewDeal { lead_id: lead.id, ..Default::default() }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn default_stages_are_created_once_per_org(db: PgPool) {
        let (acme, _) = org(&db, "Acme").await;
        let (other, _) = org(&db, "Other").await;
        futures::future::join_all((0..5).map(|_| ensure_stages(&db, acme))).await.into_iter().for_each(|r| r.unwrap());
        let names: Vec<(String, String, i32, Option<String>)> =
            stages(&db, acme).await.unwrap().into_iter().map(|s| (s.name, s.kind, s.probability, s.auto)).collect();
        let open = |name: &str, p: i32, auto: Option<&str>| (name.to_string(), "open".to_string(), p, auto.map(str::to_string));
        assert_eq!(
            names,
            [
                open("Interested", 10, Some("hot")),
                open("Meeting booked", 30, Some("booked")),
                open("Proposal", 50, None),
                open("Negotiation", 70, None),
                ("Won".into(), "won".into(), 100, None),
                ("Lost".into(), "lost".into(), 0, None),
            ]
        );
        assert!(sqlx::query_scalar::<_, i64>("select count(*) from deal_stages where org_id = $1").bind(other).fetch_one(&db).await.unwrap() == 0);

        // A stage someone removed stays removed.
        let kept: Vec<StageInput> = stages(&db, acme)
            .await
            .unwrap()
            .into_iter()
            .filter(|s| s.name != "Negotiation")
            .map(|s| StageInput { id: Some(s.id), name: s.name, kind: s.kind, probability: s.probability })
            .collect();
        save_stages(&state(db.clone()), acme, PipelineBody { currency: None, stages: kept, moves: vec![] }).await.unwrap();
        assert_eq!(stages(&db, acme).await.unwrap().len(), 5);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_hot_reply_opens_one_deal(db: PgPool) {
        let state = state(db.clone());
        let (acme, project) = org(&db, "Acme").await;
        let sarah = lead(&db, project, "Sarah").await;
        // Two classified replies landing at once, then a third later.
        futures::future::join_all((0..4).map(|_| hot_reply(&state, &sarah))).await;
        hot_reply(&state, &sarah).await;
        assert_eq!(deals_of(&db, &sarah).await, [("Interested".to_string(), "open".to_string())]);
        let (source, currency): (String, String) =
            sqlx::query_as("select source, currency from deals where lead_id = $1").bind(sarah.id).fetch_one(&db).await.unwrap();
        assert_eq!((source.as_str(), currency.as_str()), ("hot_reply", "USD"));

        // A lost deal stays lost when they write again.
        let id: Uuid = sqlx::query_scalar("select id from deals where lead_id = $1").bind(sarah.id).fetch_one(&db).await.unwrap();
        move_to(&state, acme, id, to("Lost")).await.unwrap();
        hot_reply(&state, &sarah).await;
        assert_eq!(deals_of(&db, &sarah).await, [("Lost".to_string(), "lost".to_string())]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_booking_moves_the_deal_forward_but_never_back(db: PgPool) {
        let state = state(db.clone());
        let (acme, project) = org(&db, "Acme").await;
        let booked_now = |lead: &Lead| Lead { status: "meeting_booked".into(), ..lead.clone() };

        // Through the same call the booking webhook and the inbox make.
        let sarah = lead(&db, project, "Sarah").await;
        hot_reply(&state, &sarah).await;
        crate::hooks::status_changed(&state, "hot", &booked_now(&sarah)).await;
        assert_eq!(deals_of(&db, &sarah).await, [("Meeting booked".to_string(), "open".to_string())]);

        let id: Uuid = sqlx::query_scalar("select id from deals where lead_id = $1").bind(sarah.id).fetch_one(&db).await.unwrap();
        move_to(&state, acme, id, to("Proposal")).await.unwrap();
        booked(&state, &sarah).await;
        assert_eq!(deals_of(&db, &sarah).await, [("Proposal".to_string(), "open".to_string())]);
        move_to(&state, acme, id, Move { value: Some(9000.0), ..to("Won") }).await.unwrap();
        booked(&state, &sarah).await;
        assert_eq!(deals_of(&db, &sarah).await, [("Won".to_string(), "won".to_string())]);

        // Someone who books without replying first gets a deal at the booked stage.
        let jo = lead(&db, project, "Jo").await;
        booked(&state, &jo).await;
        booked(&state, &jo).await;
        assert_eq!(deals_of(&db, &jo).await, [("Meeting booked".to_string(), "open".to_string())]);

        // A hot reply and a booking racing for a new lead leave one open deal, at the booked stage.
        for i in 0..5 {
            let lead = lead(&db, project, &format!("Racer{i}")).await;
            tokio::join!(hot_reply(&state, &lead), booked(&state, &lead));
            assert_eq!(deals_of(&db, &lead).await, [("Meeting booked".to_string(), "open".to_string())], "race {i}");
        }
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn deals_move_by_hand_and_close_as_won_or_lost(db: PgPool) {
        let state = state(db.clone());
        let (acme, project) = org(&db, "Acme").await;
        let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('ana@acme.test', 'Ana', 'x') returning id")
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(acme).bind(user).execute(&db).await.unwrap();
        let sarah = lead(&db, project, "Sarah").await;

        let deal = create(&state, acme, Some(user), "manual", NewDeal { value: Some(12_000.0), owner_id: Some(user), ..new_deal(&sarah) }).await.unwrap();
        assert_eq!((deal.stage.as_str(), deal.value_cents, deal.owner_name.as_deref()), ("Interested", 1_200_000, Some("Ana")));
        let again = create(&state, acme, None, "manual", new_deal(&sarah)).await.unwrap_err();
        assert!(matches!(&again, AppError::Conflict(m) if m.contains("already has an open deal")), "{again:?}");

        let moved = move_to(&state, acme, deal.id, to("negotiation")).await.unwrap();
        assert_eq!((moved.stage.as_str(), moved.status.as_str()), ("Negotiation", "open"));
        assert!(moved.stage_changed_at > deal.stage_changed_at);
        let back = move_to(&state, acme, deal.id, Move { stage_id: Some(stage_id(&db, acme, "Proposal").await), ..Default::default() }).await.unwrap();
        assert_eq!(back.stage, "Proposal");

        let won = move_to(&state, acme, deal.id, Move { value: Some(15_500.5), ..to("Won") }).await.unwrap();
        assert_eq!((won.status.as_str(), won.value_cents), ("won", 1_550_050));
        assert!(won.closed_at.is_some());
        let lost = move_to(&state, acme, deal.id, Move { lost_reason: Some("Went with a competitor".into()), ..to("Lost") }).await.unwrap();
        assert_eq!((lost.status.as_str(), lost.lost_reason.as_str(), lost.value_cents), ("lost", "Went with a competitor", 1_550_050));

        // Closed, so the lead can have a new deal, and then the old one can't reopen beside it.
        let second = create(&state, acme, None, "manual", NewDeal { stage: Some("Proposal".into()), ..new_deal(&sarah) }).await.unwrap();
        let reopen = move_to(&state, acme, deal.id, to("Interested")).await.unwrap_err();
        assert!(matches!(reopen, AppError::Conflict(_)), "{reopen:?}");
        delete(&db, acme, second.id).await.unwrap();
        let reopened = move_to(&state, acme, deal.id, to("Interested")).await.unwrap();
        assert_eq!((reopened.status.as_str(), reopened.closed_at, reopened.lost_reason.as_str()), ("open", None, ""));

        let changes = DealChanges { close_date: Some(NaiveDate::from_ymd_opt(2026, 12, 1)), notes: Some(" Call Tuesday ".into()), ..Default::default() };
        let edited = update(&db, acme, deal.id, changes).await.unwrap();
        assert_eq!((edited.close_date, edited.notes.as_str(), edited.owner_id), (NaiveDate::from_ymd_opt(2026, 12, 1), "Call Tuesday", Some(user)));
        let cleared = update(&db, acme, deal.id, serde_json::from_value(json!({"owner_id": null, "currency": "eur"})).unwrap()).await.unwrap();
        assert_eq!((cleared.owner_id, cleared.currency.as_str(), cleared.close_date), (None, "EUR", NaiveDate::from_ymd_opt(2026, 12, 1)));
        assert!(move_to(&state, acme, deal.id, to("Nowhere")).await.is_err());
        assert!(create(&state, acme, None, "manual", NewDeal { value: Some(-5.0), ..new_deal(&lead(&db, project, "Neg").await) }).await.is_err());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn stage_edits_keep_deals_somewhere(db: PgPool) {
        let state = state(db.clone());
        let (acme, project) = org(&db, "Acme").await;
        let sarah = lead(&db, project, "Sarah").await;
        hot_reply(&state, &sarah).await;
        let input = |s: &Stage| StageInput { id: Some(s.id), name: s.name.clone(), kind: s.kind.clone(), probability: s.probability };
        let body = |stages: Vec<StageInput>, moves: Vec<StageMove>| PipelineBody { currency: None, stages, moves };
        let saved = stages(&db, acme).await.unwrap();
        let interested = saved[0].id;

        // Renamed and moved behind Proposal, the hot reply stage keeps its job.
        let mut list: Vec<StageInput> = saved.iter().map(input).collect();
        list[0].name = "Replied".into();
        list.swap(0, 2);
        list.push(StageInput { id: None, name: "Pilot".into(), kind: "open".into(), probability: 80 });
        save_stages(&state, acme, PipelineBody { currency: Some("gbp".into()), ..body(list, vec![]) }).await.unwrap();
        let now = stages(&db, acme).await.unwrap();
        let names: Vec<&str> = now.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Proposal", "Meeting booked", "Replied", "Negotiation", "Won", "Lost", "Pilot"]);
        assert_eq!((now[2].auto.as_deref(), now[2].deals), (Some("hot"), 1));
        assert_eq!(org_currency(&db, acme).await.unwrap(), "GBP");
        let jo = lead(&db, project, "Jo").await;
        hot_reply(&state, &jo).await;
        assert_eq!(deals_of(&db, &jo).await, [("Replied".to_string(), "open".to_string())]);

        // Removing a stage with deals needs somewhere of the same kind to put them.
        let without: Vec<StageInput> = now.iter().filter(|s| s.id != interested).map(input).collect();
        let refused = save_stages(&state, acme, body(without, vec![])).await.unwrap_err();
        assert!(matches!(&refused, AppError::Conflict(m) if m.contains("Replied")), "{refused:?}");
        let won = now.iter().find(|s| s.kind == "won").unwrap().id;
        let without: Vec<StageInput> = now.iter().filter(|s| s.id != interested).map(input).collect();
        assert!(save_stages(&state, acme, body(without, vec![StageMove { from: interested, to: won }])).await.is_err());
        assert_eq!(stages(&db, acme).await.unwrap().len(), 7);
        let proposal = now[0].id;
        let without: Vec<StageInput> = now.iter().filter(|s| s.id != interested).map(input).collect();
        save_stages(&state, acme, body(without, vec![StageMove { from: interested, to: proposal }])).await.unwrap();
        assert_eq!(deals_of(&db, &sarah).await, [("Proposal".to_string(), "open".to_string())]);

        // A stage with deals can't turn won, and there are at most ten and one of each kind.
        let mut list: Vec<StageInput> = stages(&db, acme).await.unwrap().iter().map(input).collect();
        list[0].kind = "won".into();
        assert!(matches!(save_stages(&state, acme, body(list, vec![])).await, Err(AppError::Conflict(_))));
        let mut list: Vec<StageInput> = stages(&db, acme).await.unwrap().iter().map(input).collect();
        for i in 0..5 {
            list.push(StageInput { id: None, name: format!("Extra {i}"), kind: "open".into(), probability: 5 });
        }
        assert!(matches!(save_stages(&state, acme, body(list, vec![])).await, Err(AppError::BadRequest(_))));
        let list: Vec<StageInput> = stages(&db, acme).await.unwrap().iter().filter(|s| s.kind != "lost").map(input).collect();
        assert!(matches!(save_stages(&state, acme, body(list, vec![])).await, Err(AppError::BadRequest(_))));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_forecast_adds_up(db: PgPool) {
        let state = state(db.clone());
        let (acme, project) = org(&db, "Acme").await;
        let (_, other_project) = org(&db, "Other").await;
        let deal = |name: &'static str, stage: &'static str, value: f64| {
            let (state, db) = (state.clone(), db.clone());
            async move {
                let lead = lead(&db, project, name).await;
                create(&state, acme, None, "manual", NewDeal { value: Some(value), stage: Some(stage.into()), ..new_deal(&lead) }).await.unwrap()
            }
        };
        deal("A", "Interested", 1000.0).await;
        deal("B", "Meeting booked", 2000.0).await;
        deal("C", "Negotiation", 3000.0).await;
        deal("D", "Won", 5000.0).await;
        deal("E", "Lost", 700.0).await;
        deal("F", "Lost", 300.0).await;
        let old = deal("G", "Won", 4000.0).await;
        sqlx::query("update deals set closed_at = now() - interval '120 days' where id = $1").bind(old.id).execute(&db).await.unwrap();
        let euro = deal("H", "Proposal", 100.0).await;
        update(&db, acme, euro.id, DealChanges { currency: Some("EUR".into()), ..Default::default() }).await.unwrap();

        let f = forecast(&db, acme, None).await.unwrap();
        let usd = &f.amounts[0];
        // 10% of 1000, 30% of 2000 and 70% of 3000.
        assert_eq!((usd.currency.as_str(), usd.open, usd.weighted, usd.won_this_month), ("USD", 600_000, 280_000, 500_000));
        assert_eq!((f.amounts[1].currency.as_str(), f.amounts[1].open, f.amounts[1].weighted), ("EUR", 10_000, 5_000));
        // One won and two lost in the last 90 days. The one won 120 days ago doesn't count.
        assert_eq!((f.won, f.lost), (1, 2));
        assert!((f.win_rate.unwrap() - 1.0 / 3.0).abs() < 1e-9);

        let board = board(&db, acme, Some(project)).await.unwrap();
        assert_eq!(board.deals.len(), 7);
        assert!(board.deals.iter().all(|d| d.id != old.id));
        let empty = forecast(&db, acme, Some(other_project)).await.unwrap();
        assert!(empty.amounts.is_empty() && empty.win_rate.is_none());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn deal_events_go_to_the_webhook(db: PgPool) {
        let state = state(db.clone());
        let (acme, project) = org(&db, "Acme").await;
        sqlx::query("insert into project_settings (project_id, webhook_events) values ($1, '{deal.created,deal.stage_changed,deal.won,deal.lost}')")
            .bind(project)
            .execute(&db)
            .await
            .unwrap();
        // Refused before connecting, as it isn't public, so nothing leaves the machine and the delivery log still keeps the payload.
        sqlx::query("insert into webhook_endpoints (project_id, url) values ($1, 'http://127.0.0.1/hook')").bind(project).execute(&db).await.unwrap();
        let sarah = lead(&db, project, "Sarah").await;
        hot_reply(&state, &sarah).await;
        let id: Uuid = sqlx::query_scalar("select id from deals where lead_id = $1").bind(sarah.id).fetch_one(&db).await.unwrap();
        booked(&state, &sarah).await;
        move_to(&state, acme, id, Move { value: Some(8000.0), ..to("Won") }).await.unwrap();
        move_to(&state, acme, id, to("Lost")).await.unwrap();
        // Removing a stage moves its deals, and says so.
        let tom = lead(&db, project, "Tom").await;
        hot_reply(&state, &tom).await;
        let (interested, proposal) = (stage_id(&db, acme, "Interested").await, stage_id(&db, acme, "Proposal").await);
        let kept = stages(&db, acme)
            .await
            .unwrap()
            .into_iter()
            .filter(|s| s.id != interested)
            .map(|s| StageInput { id: Some(s.id), name: s.name, kind: s.kind, probability: s.probability })
            .collect();
        save_stages(&state, acme, PipelineBody { currency: None, stages: kept, moves: vec![StageMove { from: interested, to: proposal }] }).await.unwrap();

        let mut sent: Vec<(String, Value)> = vec![];
        for _ in 0..50 {
            sent = sqlx::query_as("select event, payload from webhook_deliveries where project_id = $1 order by created_at")
                .bind(project)
                .fetch_all(&db)
                .await
                .unwrap();
            if sent.len() >= 8 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let mut events: Vec<(&str, &str, &str)> =
            sent.iter().map(|(e, p)| (e.as_str(), p["deal"]["stage"].as_str().unwrap(), p["deal"]["previous_stage"].as_str().unwrap_or(""))).collect();
        events.sort();
        assert_eq!(
            events,
            [
                ("deal.created", "Interested", ""),
                ("deal.created", "Interested", ""),
                ("deal.lost", "Lost", "Won"),
                ("deal.stage_changed", "Lost", "Won"),
                ("deal.stage_changed", "Meeting booked", "Interested"),
                ("deal.stage_changed", "Proposal", "Interested"),
                ("deal.stage_changed", "Won", "Meeting booked"),
                ("deal.won", "Won", "Meeting booked"),
            ]
        );
        let won = &sent.iter().find(|(e, _)| e == "deal.won").unwrap().1;
        assert_eq!((won["deal"]["value"].clone(), won["lead"]["email"].clone()), (json!(8000.0), json!("sarah@northwind.example")));
        assert_eq!(won["deal"]["url"], format!("https://app.test/app/pipeline?deal={id}"));
        assert_eq!(crm_line(&db, sarah.id).await.as_deref(), Some("Deal in Splee: Lost, 8,000 USD"));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn deals_stay_inside_their_org(db: PgPool) {
        let state = state(db.clone());
        let (acme, acme_project) = org(&db, "Acme").await;
        let (other, other_project) = org(&db, "Other").await;
        let ours = lead(&db, acme_project, "Sarah").await;
        let theirs = lead(&db, other_project, "Bo").await;
        hot_reply(&state, &theirs).await;
        let their_deal: Uuid = sqlx::query_scalar("select id from deals where lead_id = $1").bind(theirs.id).fetch_one(&db).await.unwrap();
        let their_stage = stage_id(&db, other, "Proposal").await;
        let stranger: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('x@other.test', 'X', 'x') returning id")
            .fetch_one(&db)
            .await
            .unwrap();

        assert!(matches!(create(&state, acme, None, "manual", new_deal(&theirs)).await, Err(AppError::NotFound)));
        assert!(matches!(move_to(&state, acme, their_deal, to("Won")).await, Err(AppError::NotFound)));
        assert!(matches!(get(&db, acme, their_deal).await, Err(AppError::NotFound)));
        assert!(matches!(update(&db, acme, their_deal, DealChanges::default()).await, Err(AppError::NotFound)));
        assert!(matches!(delete(&db, acme, their_deal).await, Err(AppError::NotFound)));
        let other_stage = NewDeal { stage_id: Some(their_stage), ..new_deal(&ours) };
        assert!(matches!(create(&state, acme, None, "manual", other_stage).await, Err(AppError::BadRequest(_))));
        let outsider = NewDeal { owner_id: Some(stranger), ..new_deal(&ours) };
        assert!(matches!(create(&state, acme, None, "manual", outsider).await, Err(AppError::BadRequest(_))));
        assert!(board(&db, acme, None).await.unwrap().deals.is_empty());
        assert_eq!(list(&db, acme, &DealFilter::default(), 50, 0).await.unwrap().1, 0);
        let theirs_listed = list(&db, other, &DealFilter { status: Some("open".into()), ..Default::default() }, 50, 0).await.unwrap();
        assert_eq!(theirs_listed.1, 1);
        assert_eq!(deals_of(&db, &theirs).await, [("Interested".to_string(), "open".to_string())]);
    }

    #[test]
    fn amounts_read_like_money() {
        assert_eq!(amount(0), "0");
        assert_eq!(amount(1_200_000), "12,000");
        assert_eq!(amount(123_456_789), "1,234,567.89");
        assert_eq!(amount(5), "0.05");
    }
}
