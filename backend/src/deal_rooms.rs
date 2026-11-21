//! Deal rooms: one page per deal for the buyer (PUBLIC_URL/room/{token}) with the agent's welcome, the close plan, the
//! proposal, the case studies from Materials that fit them best, and their questions. A question goes to the project's
//! chats and gets an answer the agent drafts (agent/deal_rooms.rs), which the team edits and publishes. The page shows
//! nothing until the welcome is written, and nothing once the deal is lost.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::hooks::{self, ChatAlert};
use crate::models::Ts;

/// Case studies a room shows at most.
pub const MAX_PROOF: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pick {
    pub material_id: Uuid,
    pub why: String,
}

/// What the agent writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Content {
    pub headline: String,
    pub intro: String,
    pub proof: Vec<Pick>,
}

/// A picked case study, while it's still in Materials.
#[derive(Debug, Serialize, FromRow)]
pub struct Proof {
    pub material_id: Uuid,
    pub title: String,
    pub content: String,
    pub why: String,
}

#[derive(Debug, Serialize, FromRow)]
pub struct Question {
    pub id: Uuid,
    pub name: String,
    pub question: String,
    pub draft: String,
    pub answer: Option<String>,
    pub answered_at: Option<Ts>,
    /// The agent is still drafting the answer.
    pub drafting: bool,
    pub created_at: Ts,
}

/// The deal's room for the app.
#[derive(Debug, Serialize, FromRow)]
pub struct Room {
    pub id: Uuid,
    pub deal_id: Uuid,
    pub token: String,
    /// writing while the agent writes the welcome, ready once it's written, else failed.
    pub status: String,
    pub room: Option<Json<Content>>,
    pub views: i32,
    pub first_viewed_at: Option<Ts>,
    pub last_viewed_at: Option<Ts>,
    #[sqlx(skip)]
    pub url: String,
    #[sqlx(skip)]
    pub proof: Vec<Proof>,
    #[sqlx(skip)]
    pub questions: Vec<Question>,
}

pub fn url(public_url: &str, token: &str) -> String {
    format!("{}/room/{token}", public_url.trim_end_matches('/'))
}

/// The picked case studies in the agent's order, leaving out any deleted since.
async fn proof(db: &PgPool, room_id: Uuid) -> sqlx::Result<Vec<Proof>> {
    sqlx::query_as(
        "select m.id as material_id, m.title, m.content, p.why
         from deal_rooms r, rows from (jsonb_to_recordset(r.room->'proof') as (material_id uuid, why text)) with ordinality as p(material_id, why, n)
         join materials m on m.id = p.material_id
         where r.id = $1 and m.project_id = r.project_id order by p.n",
    )
    .bind(room_id)
    .fetch_all(db)
    .await
}

async fn questions(db: &PgPool, room_id: Uuid, answered_only: bool) -> sqlx::Result<Vec<Question>> {
    sqlx::query_as(
        "select q.id, q.name, q.question, q.draft, q.answer, q.answered_at, coalesce(r.status in ('queued', 'running'), false) as drafting, q.created_at
         from deal_room_questions q left join agent_runs r on r.id = q.run_id
         where q.room_id = $1 and (not $2 or q.answer is not null) order by q.created_at",
    )
    .bind(room_id)
    .bind(answered_only)
    .fetch_all(db)
    .await
}

pub async fn for_deal(db: &PgPool, public_url: &str, deal_id: Uuid) -> sqlx::Result<Option<Room>> {
    let room = sqlx::query_as::<_, Room>(
        "select d.id, d.deal_id, d.token, d.room, d.views, d.first_viewed_at, d.last_viewed_at,
                case when r.status in ('queued', 'running') then 'writing' when d.room is not null then 'ready' else 'failed' end as status
         from deal_rooms d left join agent_runs r on r.id = d.run_id where d.deal_id = $1",
    )
    .bind(deal_id)
    .fetch_optional(db)
    .await?;
    let Some(room) = room else { return Ok(None) };
    Ok(Some(Room { url: url(public_url, &room.token), proof: proof(db, room.id).await?, questions: questions(db, room.id, false).await?, ..room }))
}

/// Has the agent write the room's welcome now, keeping its link and questions. One being written is reused. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let (id, writing): (Uuid, Option<Uuid>) = sqlx::query_as(
        "with room as (
           insert into deal_rooms (project_id, deal_id) values ($1, $2)
           on conflict (deal_id) do update set project_id = excluded.project_id returning id, run_id)
         select room.id, r.id from room left join agent_runs r on r.id = room.run_id and r.status in ('queued', 'running')",
    )
    .bind(project_id)
    .bind(deal_id)
    .fetch_one(&mut *tx)
    .await?;
    if let Some(run) = writing {
        tx.commit().await?;
        return Ok(run);
    }
    let run = crate::agent::enqueue(&mut *tx, crate::agent::kind::DEAL_ROOM, Some(project_id), Some(org_id), json!({ "room_id": id })).await?;
    sqlx::query("update deal_rooms set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(run)
}

/// The plan's part of the buyer's page, once it has a goal.
#[derive(Debug, Serialize)]
pub struct PlanPart {
    pub goal: String,
    pub url: String,
    pub steps: Vec<crate::close_plans::Step>,
}

/// The proposal's part, once it has a title. Prices stay on its own page.
#[derive(Debug, Serialize)]
pub struct ProposalPart {
    pub title: String,
    pub url: String,
    pub held: bool,
    pub accepted: bool,
}

#[derive(Debug, Serialize)]
pub struct Answered {
    pub name: String,
    pub question: String,
    pub answer: String,
    pub answered_at: Option<Ts>,
}

/// What the buyer's page shows, nothing the team keeps to itself: no drafts, unanswered questions or prices.
#[derive(Debug, Serialize, FromRow)]
pub struct Public {
    pub room: Json<Content>,
    pub first_name: String,
    pub company: String,
    pub company_domain: Option<String>,
    pub from_name: String,
    pub from_domain: Option<String>,
    pub sender_name: String,
    /// The mailbox that last wrote to them: its booking link, else its Splee booking page, else the project's.
    pub booking_url: Option<String>,
    #[sqlx(skip)]
    pub plan: Option<PlanPart>,
    #[sqlx(skip)]
    pub proposal: Option<ProposalPart>,
    #[sqlx(skip)]
    pub proof: Vec<Proof>,
    #[sqlx(skip)]
    pub questions: Vec<Answered>,
}

pub async fn public(db: &PgPool, public_url: &str, token: &str) -> sqlx::Result<Option<Public>> {
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select r.id, r.deal_id from deal_rooms r join deals d on d.id = r.deal_id where r.token = $1 and r.room is not null and d.status <> 'lost'",
    )
    .bind(token)
    .fetch_optional(db)
    .await?;
    let Some((room_id, deal_id)) = row else { return Ok(None) };
    let page = sqlx::query_as::<_, Public>(
        "select r.room, l.first_name, l.company_name as company, l.company_domain::text as company_domain,
                coalesce(nullif(pr.company->>'name', ''), pr.name) as from_name, pr.domain::text as from_domain,
                coalesce(nullif(m.from_name, ''), s.sender_name, '') as sender_name,
                coalesce(nullif(m.booking_link, ''), $2 || '/book/' || b.slug, nullif(s.booking_link, '')) as booking_url
         from deal_rooms r join deals d on d.id = r.deal_id join leads l on l.id = d.lead_id join projects pr on pr.id = r.project_id
         left join project_settings s on s.project_id = r.project_id
         left join lateral (select e.mailbox_id from email_messages e where e.lead_id = l.id and e.direction = 'outbound' and e.mailbox_id is not null
                            and e.status = 'sent' order by e.sent_at desc limit 1) e on true
         left join mailboxes m on m.id = e.mailbox_id
         left join booking_pages b on b.mailbox_id = m.id
         where r.id = $1",
    )
    .bind(room_id)
    .bind(public_url.trim_end_matches('/'))
    .fetch_one(db)
    .await?;
    let plan = crate::close_plans::for_deal(db, public_url, deal_id)
        .await?
        .and_then(|p| Some(PlanPart { goal: p.goal?, url: p.url, steps: p.steps }));
    let proposal = crate::proposals::for_deal(db, public_url, deal_id)
        .await?
        .and_then(|p| Some(ProposalPart { title: p.title?, url: p.url, held: p.held, accepted: p.accepted_at.is_some() }));
    let questions = questions(db, room_id, true)
        .await?
        .into_iter()
        .map(|q| Answered { name: q.name, question: q.question, answer: q.answer.unwrap_or_default(), answered_at: q.answered_at })
        .collect();
    Ok(Some(Public { plan, proposal, proof: proof(db, room_id).await?, questions, ..page }))
}

/// The deal's link and who it's with, for the project's chats.
async fn about(db: &PgPool, public_url: &str, room_id: Uuid) -> sqlx::Result<(Uuid, String, String)> {
    let (project_id, deal_id, who): (Uuid, Uuid, String) = sqlx::query_as(
        "select r.project_id, r.deal_id, coalesce(nullif(trim(l.full_name), ''), l.email::text, 'The buyer')
                || coalesce(' (' || nullif(trim(l.company_name), '') || ')', '')
         from deal_rooms r join deals d on d.id = r.deal_id join leads l on l.id = d.lead_id where r.id = $1",
    )
    .bind(room_id)
    .fetch_one(db)
    .await?;
    Ok((project_id, format!("{}/app/pipeline?deal={deal_id}", public_url.trim_end_matches('/')), who))
}

/// Counts a look at the page and tells the project's chats about the first one.
pub async fn record_view(db: &PgPool, public_url: &str, token: &str) -> sqlx::Result<()> {
    let seen: Option<(Uuid, bool)> = sqlx::query_as(
        "update deal_rooms set views = views + 1, first_viewed_at = coalesce(first_viewed_at, now()), last_viewed_at = now()
         where token = $1 and room is not null returning id, views = 1",
    )
    .bind(token)
    .fetch_optional(db)
    .await?;
    if let Some((room_id, true)) = seen {
        let (project_id, link, who) = about(db, public_url, room_id).await?;
        let summary = format!("{who} opened the deal room");
        hooks::post_chat(db, project_id, &ChatAlert { label: "Deal room opened", summary: &summary, quote: None, link: &link, link_text: "Open the deal" }).await;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct Ask {
    pub name: String,
    pub question: String,
}

/// Saves the buyer's question, has the agent draft an answer and tells the project's chats. Returns the run.
pub async fn ask(db: &PgPool, public_url: &str, token: &str, form: &Ask) -> AppResult<Uuid> {
    let mut tx = db.begin().await?;
    let room: Option<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "select r.id, r.project_id, d.org_id from deal_rooms r join deals d on d.id = r.deal_id
         where r.token = $1 and r.room is not null and d.status <> 'lost'",
    )
    .bind(token)
    .fetch_optional(&mut *tx)
    .await?;
    let (room_id, project_id, org_id) = room.ok_or(AppError::NotFound)?;
    let (name, question) = (form.name.trim(), form.question.trim());
    let id: Uuid = sqlx::query_scalar("insert into deal_room_questions (room_id, name, question) values ($1, $2, $3) returning id")
        .bind(room_id)
        .bind(name)
        .bind(question)
        .fetch_one(&mut *tx)
        .await?;
    let run = crate::agent::enqueue(&mut *tx, crate::agent::kind::ROOM_ANSWER, Some(project_id), Some(org_id), json!({ "question_id": id })).await?;
    sqlx::query("update deal_room_questions set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    let (_, link, who) = about(db, public_url, room_id).await?;
    let summary = format!("{name} asked in {who}'s deal room. The agent is drafting an answer for you to check.");
    hooks::post_chat(db, project_id, &ChatAlert { label: "Deal room question", summary: &summary, quote: Some(question), link: &link, link_text: "Answer it" }).await;
    Ok(run)
}

/// The deal a question is on, when it's in the organization.
pub async fn deal_of_question(db: &PgPool, org_id: Uuid, question_id: Uuid) -> AppResult<Uuid> {
    sqlx::query_scalar("select r.deal_id from deal_room_questions q join deal_rooms r on r.id = q.room_id join deals d on d.id = r.deal_id where q.id = $1 and d.org_id = $2")
        .bind(question_id)
        .bind(org_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// Publishes the answer on the page, or takes it down when it's empty.
pub async fn answer(db: &PgPool, question_id: Uuid, user_id: Uuid, text: &str) -> sqlx::Result<()> {
    let text = text.trim();
    sqlx::query(
        "update deal_room_questions set answer = nullif($2, ''), answered_by = case when $2 = '' then null else $3 end,
                answered_at = case when $2 = '' then null else now() end where id = $1",
    )
    .bind(question_id)
    .bind(text)
    .bind(user_id)
    .execute(db)
    .await?;
    Ok(())
}
