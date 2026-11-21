//! Writes a won deal's handoff (crate::handoffs) from everything the sale left behind, and carries its close plan on as
//! the buyer's onboarding plan: an onboarding goal and the steps both sides take to get them going. Writing it again keeps
//! the onboarding steps already done and replaces the rest.

use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::close_plans::{DraftStep, MAX_WRITTEN, step_schema, tidy};
use super::deal_rooms::load;
use super::sdk::Call;
use super::{Ctx, Outcome};
use crate::handoffs::{Content, Person, Promise};
use crate::hooks::{self, ChatAlert};
use crate::text::plain;

const WRITER: &str = "You write the handoff from sales to whoever onboards a new B2B customer, for Splee, an outbound sales \
tool. They weren't on the deal, so tell them what they need to keep every promise and get the customer going. Use only \
what you're given. Never invent facts, names, people, numbers or commitments.";
/// How far ahead the onboarding plan reaches.
const DAYS: i64 = 60;

#[derive(Deserialize)]
struct Input {
    handoff_id: Uuid,
}

#[derive(Deserialize)]
struct Answer {
    summary: String,
    bought: String,
    promises: Vec<Promise>,
    people: Vec<Person>,
    success: Vec<String>,
    risks: Vec<String>,
    goal: String,
    steps: Vec<DraftStep>,
}

fn schema() -> Value {
    let text = json!({ "type": "string" });
    let list = json!({ "type": "array", "items": text });
    json!({
        "type": "object",
        "properties": {
            "summary": text, "bought": text,
            "promises": { "type": "array", "items": { "type": "object", "properties": { "promise": text, "quote": text }, "required": ["promise", "quote"] } },
            "people": { "type": "array", "items": { "type": "object", "properties": { "name": text, "role": text, "note": text }, "required": ["name", "role", "note"] } },
            "success": list, "risks": list, "goal": text,
            "steps": { "type": "array", "items": step_schema() }
        },
        "required": ["summary", "bought", "promises", "people", "success", "risks", "goal", "steps"]
    })
}

fn clip(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

fn items(list: Vec<String>, n: usize) -> Vec<String> {
    list.iter().map(|s| clip(s, 300)).filter(|s| !s.is_empty()).take(n).collect()
}

/// The handoff worth saving: trimmed, with each quote kept only when it's in `evidence` word for word, so nobody repeats
/// words the customer never said.
fn tidy_content(a: &Answer, evidence: &str) -> Content {
    let evidence = plain(evidence);
    let promises = a
        .promises
        .iter()
        .map(|p| {
            let quote = clip(&p.quote, 300);
            let quote = if !plain(&quote).is_empty() && evidence.contains(&plain(&quote)) { quote } else { String::new() };
            Promise { promise: clip(&p.promise, 300), quote }
        })
        .filter(|p| !p.promise.is_empty())
        .take(8)
        .collect();
    let people = a
        .people
        .iter()
        .map(|p| Person { name: clip(&p.name, 100), role: clip(&p.role, 100), note: clip(&p.note, 300) })
        .filter(|p| !p.name.is_empty())
        .take(8)
        .collect();
    Content {
        summary: clip(&a.summary, 600),
        bought: clip(&a.bought, 400),
        promises,
        people,
        success: items(a.success.clone(), 5),
        risks: items(a.risks.clone(), 5),
    }
}

pub async fn write(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let (deal_id, status, value_cents, currency, notes): (Uuid, String, i64, String, String) = sqlx::query_as(
        "select d.id, d.status, d.value_cents, d.currency, d.notes from handoffs h join deals d on d.id = h.deal_id where h.id = $1 and h.project_id = $2",
    )
    .bind(input.handoff_id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the handoff was deleted")?;
    anyhow::ensure!(status == "won", "the deal isn't won");
    let deal = load(ctx, deal_id).await?;
    let before = onboarding_steps(db, deal_id).await?;

    let proposal = crate::proposals::for_deal(db, "", deal_id).await?.filter(|p| p.title.is_some());
    let bought = match &proposal {
        Some(p) if !p.items.is_empty() => p
            .items
            .iter()
            .map(|i| format!("{} x {} at {} {} ({})", i.quantity, i.name, i.unit_price, p.currency.to_uppercase(), i.period.replace('_', " ")))
            .collect::<Vec<_>>()
            .join("; "),
        _ if value_cents > 0 => format!("A deal worth {} {}, with no itemized proposal.", crate::deals::amount(value_cents), currency.to_uppercase()),
        _ => "No proposal or value on record.".to_string(),
    };
    let proposal_text = proposal
        .as_ref()
        .map(|p| {
            let sections = p.sections.iter().map(|s| format!("{}: {}", s.title, s.body)).collect::<Vec<_>>().join("\n");
            format!("\n\nThe proposal they accepted, \"{}\":\n{}\n{sections}", p.title.as_deref().unwrap_or_default(), p.intro)
        })
        .unwrap_or_default();
    let room: Vec<(String, String)> = sqlx::query_as(
        "select q.question, q.answer from deal_room_questions q join deal_rooms r on r.id = q.room_id where r.deal_id = $1 and q.answer is not null order by q.created_at",
    )
    .bind(deal_id)
    .fetch_all(db)
    .await?;
    let room = if room.is_empty() {
        String::new()
    } else {
        format!("\n\nWhat they asked in their deal room, and our answers:\n{}", room.iter().map(|(q, a)| format!("- Q: {q}\n  A: {a}")).collect::<Vec<_>>().join("\n"))
    };
    let review: Option<(String, String)> =
        sqlx::query_as("select story, turning_point from deal_reviews where deal_id = $1 and outcome = 'won' and story <> ''").bind(deal_id).fetch_optional(db).await?;
    let review = review.map(|(story, turn)| format!("\n\nWhy we won: {story} {turn}")).unwrap_or_default();
    let people: Vec<(String, String)> = sqlx::query_as(
        "select full_name, title from leads where project_id = $1 and id <> $2 and full_name <> '' and company_name <> '' and company_name = $3 order by created_at limit 10",
    )
    .bind(deal.lead.project_id)
    .bind(deal.lead.id)
    .bind(&deal.lead.company_name)
    .fetch_all(db)
    .await?;
    let colleagues = if people.is_empty() {
        String::new()
    } else {
        format!("\nOthers at their company we know: {}.", people.iter().map(|(n, t)| if t.is_empty() { n.clone() } else { format!("{n} ({t})") }).collect::<Vec<_>>().join(", "))
    };
    let plan: Vec<String> = sqlx::query_scalar(
        "select s.title || case when s.done_at is not null then ' (done)' else '' end from close_plan_steps s join close_plans p on p.id = s.plan_id
         where p.deal_id = $1 and s.phase = 'close' order by s.position",
    )
    .bind(deal_id)
    .fetch_all(db)
    .await?;
    let plan = if plan.is_empty() { String::new() } else { format!("\n\nThe close plan we shared: {}.", plan.join("; ")) };
    let done: Vec<String> = sqlx::query_scalar(
        "select s.title from close_plan_steps s join close_plans p on p.id = s.plan_id where p.deal_id = $1 and s.phase = 'onboarding' and s.done_at is not null order by s.position",
    )
    .bind(deal_id)
    .fetch_all(db)
    .await?;
    let done = if done.is_empty() { String::new() } else { format!("\n\nOnboarding steps already done, so leave them out: {}.", done.join("; ")) };
    let notes = if notes.trim().is_empty() { String::new() } else { format!("\nThe deal's notes: {}", notes.trim()) };

    let today = Utc::now().date_naive();
    let end = today + chrono::Duration::days(DAYS);
    let about = format!("{}, {}{colleagues}", deal.who(), deal.lead.title);
    let evidence = format!("{}{proposal_text}{room}", deal.context);
    let prompt = format!(
        "{us} just won this deal. What we sell: {name}. {description}\n\nThe customer: {about}\nWhat they bought: {bought}{notes}\n\n\
         What else Splee knows:\n{evidence}{review}{plan}{done}\n\n\
         Today is {today}. Write the handoff for the teammate who onboards them, and the onboarding plan we share with the customer.\n\
         1. summary: two or three sentences on who they are, why they bought and what they need first.\n\
         2. bought: what they bought, in one sentence, with the numbers from above.\n\
         3. promises: up to 8 things we committed to, like a feature, a date, a discount's terms or help with setup, each with \
         quote, the exact words from an email, call or answer above that it rests on, or empty when there are none. Never \
         paraphrase in quote. Only what was said.\n\
         4. people: up to 8 people at their company and their part, like \"Champion\" or \"Signed the order form\", with a short \
         note. Only people named above.\n\
         5. success: up to 5 things that would make this a success to them, in their terms.\n\
         6. risks: up to 5 things that could go wrong in the first months, from what was said.\n\
         7. goal: under 15 words for the onboarding plan, naming their company, what they'll have working and by when, like \
         \"Quanta Forms' 8 reps taking notes in Fieldnote by 9 December\".\n\
         8. steps: 4 to {max} onboarding steps in order, written to the customer. Each has a title of under 8 words starting \
         with a verb, like \"Invite the team\"; a detail of one short sentence or empty; side \"them\" or \"us\"; and due, a date \
         as YYYY-MM-DD from {today} to {end}. Build them from what was promised and what they need first. {language}",
        us = deal.us,
        name = deal.name,
        description = deal.description,
        max = MAX_WRITTEN,
        language = deal.language,
    );
    ctx.log(format!("writing the handoff for {}…", deal.who())).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.4, ..Call::writing(WRITER, prompt, schema()) };
    let answer: Answer = ctx.ask(&call).await?;
    let content = tidy_content(&answer, &evidence);
    let goal = clip(&answer.goal, 200);
    let steps = tidy(answer.steps, today, end);
    anyhow::ensure!(!content.summary.is_empty() && !goal.is_empty() && !steps.is_empty(), "the agent wrote an empty handoff");

    let mut tx = db.begin().await?;
    // Checked again under a lock, since the deal could have been reopened or lost while the agent wrote.
    let won: bool = sqlx::query_scalar("select status = 'won' from deals where id = $1 for update").bind(deal_id).fetch_one(&mut *tx).await?;
    if !won {
        return Ok(Outcome::new("The deal isn't won anymore, so the handoff wasn't saved"));
    }
    let plan_id: Uuid = sqlx::query_scalar("select id from close_plans where deal_id = $1 for update").bind(deal_id).fetch_one(&mut *tx).await?;
    // The steps it replaces, locked, and only when nobody changed them since the agent read them. A plan the team edited in
    // the meantime stays theirs, and the handoff is still saved.
    let locked: Vec<Uuid> = sqlx::query_scalar("select id from close_plan_steps where plan_id = $1 and phase = 'onboarding' and done_at is null for update")
        .bind(plan_id)
        .fetch_all(&mut *tx)
        .await?;
    let edited = onboarding_steps(&mut *tx, deal_id).await? != before;
    if !edited {
        sqlx::query("delete from close_plan_steps where id = any($1)").bind(&locked).execute(&mut *tx).await?;
        let after: i32 =
            sqlx::query_scalar("select coalesce(max(position) + 1, 0) from close_plan_steps where plan_id = $1").bind(plan_id).fetch_one(&mut *tx).await?;
        for (i, (title, detail, side, due)) in steps.iter().enumerate() {
            sqlx::query("insert into close_plan_steps (plan_id, position, title, detail, side, due_date, phase) values ($1, $2, $3, $4, $5, $6, 'onboarding')")
                .bind(plan_id)
                .bind(after + i as i32)
                .bind(title)
                .bind(detail)
                .bind(side)
                .bind(due)
                .execute(&mut *tx)
                .await?;
        }
    }
    // A plan the team edited keeps its steps, and still gets a goal the first time, since without one it never turns to onboarding.
    sqlx::query("update close_plans set onboarding_goal = $2 where id = $1 and ($3 or onboarding_goal is null)")
        .bind(plan_id)
        .bind(&goal)
        .bind(!edited)
        .execute(&mut *tx)
        .await?;
    let first: bool = sqlx::query_scalar("select handoff is null from handoffs where id = $1 for update").bind(input.handoff_id).fetch_one(&mut *tx).await?;
    sqlx::query("update handoffs set handoff = $2 where id = $1").bind(input.handoff_id).bind(sqlx::types::Json(&content)).execute(&mut *tx).await?;
    tx.commit().await?;

    if first {
        let link = format!("{}/app/pipeline?deal={deal_id}", ctx.state.cfg.public_url.trim_end_matches('/'));
        let summary = format!("The handoff for {} is ready. Pick who takes them on.", deal.who());
        let alert = ChatAlert { label: "Handoff", summary: &summary, quote: Some(&content.summary), link: &link, link_text: "Read the handoff" };
        hooks::post_chat(db, deal.lead.project_id, &alert).await;
    }
    Ok(Outcome::new(if edited {
        format!("Wrote the handoff with {} promises, and kept the onboarding plan the team changed while it wrote", content.promises.len())
    } else {
        format!("Wrote the handoff with {} promises and an onboarding plan of {} steps", content.promises.len(), steps.len())
    }))
}

/// The deal's onboarding steps as they stand, to tell whether the team changed them while the agent wrote.
async fn onboarding_steps<'e>(db: impl sqlx::PgExecutor<'e>, deal_id: Uuid) -> sqlx::Result<String> {
    sqlx::query_scalar(
        "select coalesce(string_agg(concat_ws('|', s.id, s.title, s.detail, s.side, s.due_date, s.done_at), E'\\n' order by s.position, s.id), '')
         from close_plan_steps s join close_plans p on p.id = s.plan_id where p.deal_id = $1 and s.phase = 'onboarding'",
    )
    .bind(deal_id)
    .fetch_one(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::practice::fixture;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_won_deal_gets_a_handoff_and_its_plan_carries_on_as_the_onboarding_plan(db: sqlx::PgPool) {
        let t = fixture::team(&db).await;
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, sent_at) values ($1, $2, 'inbound', 'inbound', 'Re: rollout', 'We need the Salesforce sync live before the January kickoff.', 'received', now())")
            .bind(t.project)
            .bind(sqlx::query_scalar::<_, Uuid>("select lead_id from deals where id = $1").bind(t.deal).fetch_one(&db).await.unwrap())
            .execute(&db)
            .await
            .unwrap();
        crate::close_plans::add_step(&db, t.project, t.deal, "close", crate::close_plans::NewStep { title: "Sign the order form".into(), detail: String::new(), side: "them".into(), due_date: None })
            .await
            .unwrap();
        sqlx::query("update close_plans set goal = 'Lumenfold live by January'").execute(&db).await.unwrap();
        let won: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Won', 'won', 9) returning id").bind(t.org).fetch_one(&db).await.unwrap();
        sqlx::query("update deals set status = 'won', stage_id = $2, closed_at = now() where id = $1").bind(t.deal).bind(won).execute(&db).await.unwrap();

        crate::handoffs::request(&db, t.deal, t.project, t.org).await.unwrap();
        let writing = crate::close_plans::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        assert_eq!(writing.status, "writing", "the plan shows the onboarding steps being written");
        let due = (Utc::now().date_naive() + chrono::Duration::days(10)).to_string();
        let answer = json!({
            "summary": "Lumenfold bought Fieldnote for its sales team.", "bought": "8 seats, yearly.",
            "promises": [
                { "promise": "Salesforce sync live before kickoff", "quote": "We need the Salesforce sync live before the January kickoff." },
                { "promise": "Free training", "quote": "You said training is free." }
            ],
            "people": [{ "name": "Ines Duarte", "role": "Champion", "note": "" }, { "name": " ", "role": "", "note": "" }],
            "success": ["Reps log every call"], "risks": [], "goal": "Lumenfold's reps on Fieldnote by January",
            "steps": [{ "title": "Connect Salesforce", "detail": "", "side": "us", "due": due }, { "title": "Invite the team", "detail": "", "side": "them", "due": due }]
        });
        let (outcome, prompt) = crate::agent::fake::run_next(&db, crate::agent::kind::HANDOFF, answer).await;
        assert!(prompt.contains("Salesforce sync live before the January kickoff") && prompt.contains("Sign the order form"), "{prompt}");
        assert!(outcome.message.contains("2 promises"), "{}", outcome.message);

        let handoff = crate::handoffs::for_deal(&db, t.deal).await.unwrap().unwrap();
        let content = handoff.handoff.unwrap().0;
        assert_eq!(handoff.status, "ready");
        assert_eq!(content.promises[1].quote, "", "a quote nobody said is dropped");
        assert!(!content.promises[0].quote.is_empty());
        assert_eq!(content.people.len(), 1);

        let plan = crate::close_plans::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        assert_eq!(plan.onboarding_goal.as_deref(), Some("Lumenfold's reps on Fieldnote by January"));
        let phases: Vec<_> = plan.steps.iter().map(|s| (s.title.as_str(), s.phase.as_str())).collect();
        assert_eq!(phases, vec![("Sign the order form", "close"), ("Connect Salesforce", "onboarding"), ("Invite the team", "onboarding")]);
        let page = crate::close_plans::public(&db, "https://app.example", &plan.token).await.unwrap().unwrap();
        assert_eq!((page.phase.as_str(), page.onboarding_goal.as_deref()), ("onboarding", Some("Lumenfold's reps on Fieldnote by January")));
        let signed = &page.steps[0];
        assert_eq!((signed.phase.as_str(), signed.title.as_str()), ("close", "Sign the order form"));
        assert!(signed.detail.is_empty() && signed.due_date.is_none() && signed.done_by.is_none(), "the buyer gets the close steps' titles only");

        let invite = plan.steps[2].id;
        crate::close_plans::tick(&db, &plan.token, invite, true).await.unwrap().unwrap();
        crate::handoffs::request(&db, t.deal, t.project, t.org).await.unwrap();
        let again = json!({
            "summary": "Again.", "bought": "8 seats.", "promises": [], "people": [], "success": [], "risks": [], "goal": "Lumenfold live",
            "steps": [{ "title": "Run the training", "detail": "", "side": "us", "due": due }]
        });
        let (_, prompt) = crate::agent::fake::run_next(&db, crate::agent::kind::HANDOFF, again).await;
        assert!(prompt.contains("Onboarding steps already done, so leave them out: Invite the team."), "{prompt}");
        let plan = crate::close_plans::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        let titles: Vec<_> = plan.steps.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["Sign the order form", "Invite the team", "Run the training"]);

        // Reopened, the buyer is back on the close plan, and the onboarding steps can't be ticked from it.
        sqlx::query("update deals set status = 'open', closed_at = null where id = $1").bind(t.deal).execute(&db).await.unwrap();
        let page = crate::close_plans::public(&db, "https://app.example", &plan.token).await.unwrap().unwrap();
        assert_eq!(page.phase, "close");
        assert_eq!(page.steps.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), vec!["Sign the order form"]);
        assert!(crate::close_plans::tick(&db, &plan.token, invite, false).await.unwrap().is_none());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_step_the_team_adds_while_it_writes_stays_and_the_plan_still_turns_to_onboarding(db: sqlx::PgPool) {
        let t = fixture::team(&db).await;
        crate::close_plans::add_step(&db, t.project, t.deal, "close", crate::close_plans::NewStep { title: "Sign the order form".into(), detail: String::new(), side: "them".into(), due_date: None })
            .await
            .unwrap();
        let won: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Won', 'won', 9) returning id").bind(t.org).fetch_one(&db).await.unwrap();
        sqlx::query("update deals set status = 'won', stage_id = $2, closed_at = now() where id = $1").bind(t.deal).bind(won).execute(&db).await.unwrap();
        crate::handoffs::request(&db, t.deal, t.project, t.org).await.unwrap();

        let started = sqlx::query_as::<_, crate::models::AgentRun>("update agent_runs set status = 'running' where kind = $1 and status = 'queued' returning *")
            .bind(crate::agent::kind::HANDOFF)
            .fetch_one(&db)
            .await
            .unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: Default::default() };
        let (options, deal) = (db.connect_options().as_ref().clone(), t.deal);
        let due = (Utc::now().date_naive() + chrono::Duration::days(10)).to_string();
        crate::agent::fake::answer_with(ctx.run.id, move |_| {
            // The team adds a step while the agent writes. The test's runtime is blocked in here, so it goes in from a thread of its own.
            let options = options.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                runtime.block_on(async {
                    let mut conn = sqlx::ConnectOptions::connect(&options).await.unwrap();
                    sqlx::query("insert into close_plan_steps (plan_id, position, title, side, phase) select id, 5, 'Kickoff call', 'us', 'onboarding' from close_plans where deal_id = $1")
                        .bind(deal)
                        .execute(&mut conn)
                        .await
                        .unwrap();
                })
            })
            .join()
            .unwrap();
            json!({
                "summary": "Lumenfold bought Fieldnote.", "bought": "8 seats.", "promises": [], "people": [], "success": [], "risks": [],
                "goal": "Lumenfold live by January", "steps": [{ "title": "Connect Salesforce", "detail": "", "side": "us", "due": due }]
            })
        });
        let outcome = super::super::dispatch(&ctx).await.unwrap();
        assert!(outcome.message.contains("kept the onboarding plan the team changed"), "{}", outcome.message);

        let plan = crate::close_plans::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        assert_eq!(plan.steps.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), vec!["Sign the order form", "Kickoff call"]);
        assert_eq!(plan.onboarding_goal.as_deref(), Some("Lumenfold live by January"));
        let page = crate::close_plans::public(&db, "https://app.example", &plan.token).await.unwrap().unwrap();
        assert_eq!(page.phase, "onboarding", "the buyer sees the team's onboarding step");
    }
}
