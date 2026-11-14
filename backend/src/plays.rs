//! Plays: "when this happens, do that" rules each project runs on its own. Every 5 minutes the worker reads each enabled
//! play's trigger for events after its `checked_at`, and acts once per company or lead, up to its daily cap. Events past
//! the cap wait for the next day instead of being dropped.

use std::time::Duration;

use chrono::Utc;
use serde_json::json;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::agent::{self, kind};
use crate::hooks::{self, ChatAlert};
use crate::models::Ts;
use crate::state::AppState;

pub const TRIGGERS: [&str; 6] = ["funding", "news", "hiring", "visit", "job_change", "score"];
/// Triggers that happen at a company. The others happen to a lead.
pub const COMPANY_TRIGGERS: [&str; 4] = ["funding", "news", "hiring", "visit"];
pub const ACTIONS: [&str; 3] = ["add_to_campaign", "write_email", "notify"];
pub const MAX_PLAYS: i64 = 20;

/// Whether `action` can follow `trigger`: companies go into campaigns, leads get emails, and anything can ping the chats.
pub fn fits(trigger: &str, action: &str) -> bool {
    match action {
        "add_to_campaign" => COMPANY_TRIGGERS.contains(&trigger),
        "write_email" => !COMPANY_TRIGGERS.contains(&trigger),
        _ => true,
    }
}

pub fn spawn(state: AppState) {
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(300), || tick(&state), "plays").await });
}

#[derive(Debug, Clone, FromRow)]
pub struct Play {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub trigger: String,
    pub action: String,
    pub segment_id: Option<Uuid>,
    pub min_score: i32,
    pub daily_cap: i32,
    pub checked_at: Ts,
}

/// Something that happened, as a trigger found it.
#[derive(Debug, FromRow)]
pub struct Event {
    pub subject: String,
    pub domain: Option<String>,
    pub lead_id: Option<Uuid>,
    /// Who it's about, for the chats: a company's name or a lead's.
    pub name: String,
    pub what: String,
    pub at: Ts,
}

const PLAY_COLUMNS: &str = "id, project_id, name, trigger, action, segment_id, min_score, daily_cap, checked_at";

pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    let plays: Vec<Uuid> = sqlx::query_scalar(
        "select p.id from plays p join projects pr on pr.id = p.project_id where p.enabled and pr.org_id is not null order by p.checked_at",
    )
    .fetch_all(&state.db)
    .await?;
    for play_id in plays {
        if let Err(e) = run(state, play_id, Utc::now()).await {
            tracing::error!(%play_id, error = ?e, "play failed");
        }
    }
    Ok(())
}

/// Acts on the play's events up to `until` and moves its `checked_at` past the ones it handled. Returns how many it acted on.
/// One backend runs a play at a time, so two never act on the same event, and it reads the play again under the lock, so
/// one switched off or reset meanwhile is seen as it is now.
pub async fn run(state: &AppState, play_id: Uuid, until: Ts) -> anyhow::Result<usize> {
    let db = &state.db;
    let mut lock = db.begin().await?;
    let owned: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended('play:' || $1::text, 0))").bind(play_id).fetch_one(&mut *lock).await?;
    if !owned {
        return Ok(0);
    }
    let Some(play) = sqlx::query_as::<_, Play>(&format!("select {PLAY_COLUMNS} from plays where id = $1 and enabled")).bind(play_id).fetch_optional(&mut *lock).await? else {
        return Ok(0);
    };
    let play = &play;
    let fired_today: i64 = sqlx::query_scalar("select count(*) from play_firings where play_id = $1 and created_at > $2 - interval '1 day'")
        .bind(play.id)
        .bind(until)
        .fetch_one(db)
        .await?;
    let mut room = (i64::from(play.daily_cap) - fired_today).max(0) as usize;
    let mut checked = until;
    // The last event handled. Past the cap, the watermark stops there, and events at that same moment come again next
    // time, since events() reads from the watermark on and leaves out the subjects already fired.
    let mut handled = play.checked_at;
    let mut acted = 0;
    for event in events(db, play, until).await? {
        if room == 0 {
            checked = handled;
            break;
        }
        handled = event.at;
        let (outcome, note, run_id) = match act(state, play, &event).await {
            Ok(Done { skipped: Some(why), .. }) => ("skipped", why, None),
            Ok(Done { run_id, .. }) => ("done", String::new(), run_id),
            Err(e) => ("failed", format!("{e:#}"), None),
        };
        let inserted = sqlx::query(
            "insert into play_firings (play_id, project_id, subject, domain, lead_id, what, outcome, note, run_id) values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             on conflict (play_id, subject) do nothing",
        )
        .bind(play.id)
        .bind(play.project_id)
        .bind(&event.subject)
        .bind(&event.domain)
        .bind(event.lead_id)
        .bind(&event.what)
        .bind(outcome)
        .bind(&note)
        .bind(run_id)
        .execute(db)
        .await?;
        if inserted.rows_affected() > 0 {
            room -= 1;
            acted += 1;
        }
    }
    // Re-enabling a play while it ran moved its watermark to now, and that wins over this run's.
    sqlx::query("update plays set checked_at = $2 where id = $1 and checked_at = $3").bind(play.id).bind(checked).bind(play.checked_at).execute(&mut *lock).await?;
    lock.commit().await?;
    Ok(acted)
}

/// The play's events from its `checked_at` up to `until`, oldest first, one per company or lead, leaving out the ones it
/// already fired for. The watermark itself is included: a cap can stop between events at the same moment.
pub async fn events(db: &PgPool, play: &Play, until: Ts) -> sqlx::Result<Vec<Event>> {
    // Each source as (domain, lead, name, what, at). Company sources only count companies in the project.
    let source = match play.trigger.as_str() {
        "funding" | "news" => {
            "select g.domain::text as domain, null::uuid as lead_id, coalesce(nullif(c.name, ''), g.domain::text) as name,
                    g.kind, g.title, g.amount, g.offered, g.happened_on, null::text as what, g.first_seen as at, c.score as score
             from company_signals g join companies c on c.project_id = g.project_id and c.domain = g.domain
             where g.project_id = $1 and ($4::text <> 'funding' or g.kind like 'funding%')"
        }
        "hiring" => {
            "select j.domain::text, null::uuid, coalesce(nullif(c.name, ''), j.domain::text), null, null, null::bigint, null::bigint, null::date,
                    'Hiring: ' || j.title, j.first_seen, c.score
             from job_openings j join companies c on c.project_id = j.project_id and c.domain = j.domain
             where j.project_id = $1 and j.closed_at is null"
        }
        "visit" => {
            "select h.domain::text, null::uuid, coalesce(nullif(c.name, ''), nullif(h.company, ''), h.domain::text), null, null, null::bigint,
                    null::bigint, null::date, 'Visited ' || h.path, h.created_at, coalesce(c.score, 0::smallint)
             from visitor_hits h left join companies c on c.project_id = h.project_id and c.domain = h.domain
             where h.project_id = $1 and h.domain is not null"
        }
        "job_change" => {
            "select null::text, l.id, l.full_name, null, null, null::bigint, null::bigint, null::date,
                    'Moved to ' || j.new_company || coalesce(' from ' || nullif(j.old_company, ''), ''), j.seen_at, l.score
             from job_changes j join leads l on l.id = j.new_lead_id
             where j.project_id = $1 and j.dismissed_at is null"
        }
        _ => {
            "select null::text, l.id, l.full_name, null, null, null::bigint, null::bigint, null::date, 'Scored ' || l.score, l.scored_at, l.score
             from leads l where l.project_id = $1 and l.scored_at is not null and l.score >= greatest($5, 1)"
        }
    };
    #[derive(FromRow)]
    struct Row {
        domain: Option<String>,
        lead_id: Option<Uuid>,
        name: String,
        kind: Option<String>,
        title: Option<String>,
        amount: Option<i64>,
        offered: Option<i64>,
        happened_on: Option<chrono::NaiveDate>,
        what: Option<String>,
        at: Ts,
    }
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "select domain, lead_id, name, kind, title, amount, offered, happened_on, what, at
         from (select distinct on (coalesce(lower(domain), lead_id::text)) * from ({source}) s(domain, lead_id, name, kind, title, amount, offered, happened_on, what, at, score)
               where at >= $2 and at <= $3 and score >= $5 and $4::text <> ''
               order by coalesce(lower(domain), lead_id::text), at) e
         where not exists (select 1 from play_firings f where f.play_id = $6 and f.subject = coalesce('domain:' || lower(e.domain), 'lead:' || e.lead_id::text))
         order by at"
    ))
    .bind(play.project_id)
    .bind(play.checked_at)
    .bind(until)
    .bind(&play.trigger)
    .bind(play.min_score)
    .bind(play.id)
    .fetch_all(db)
    .await?;
    let today = Utc::now().date_naive();
    Ok(rows
        .into_iter()
        .map(|r| {
            let what = match (r.what, r.kind, r.happened_on) {
                (Some(what), ..) => what,
                (None, Some(kind), Some(on)) => crate::signals::headline(&kind, r.title.as_deref().unwrap_or_default(), r.amount, r.offered, on, today),
                _ => String::new(),
            };
            let subject = match (&r.domain, r.lead_id) {
                (Some(d), _) => format!("domain:{}", d.to_lowercase()),
                (None, Some(id)) => format!("lead:{id}"),
                _ => String::new(),
            };
            Event { subject, domain: r.domain, lead_id: r.lead_id, name: r.name, what, at: r.at }
        })
        .filter(|e| !e.subject.is_empty())
        .collect())
}

pub struct Done {
    /// Why nothing was done, when nothing was.
    pub skipped: Option<String>,
    pub run_id: Option<Uuid>,
}

impl Done {
    fn skip(why: &str) -> Self {
        Self { skipped: Some(why.into()), run_id: None }
    }
}

async fn act(state: &AppState, play: &Play, event: &Event) -> anyhow::Result<Done> {
    let db = &state.db;
    let org_id: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(play.project_id).fetch_one(db).await?;
    match play.action.as_str() {
        "add_to_campaign" => {
            let (Some(domain), Some(segment_id)) = (&event.domain, play.segment_id) else { return Ok(Done::skip("No campaign is picked")) };
            // A visitor's company may not be in the project yet.
            sqlx::query("insert into companies (project_id, domain, name, fit_reason) values ($1, $2, $3, $4) on conflict (project_id, domain) do nothing")
                .bind(play.project_id)
                .bind(domain)
                .bind(&event.name)
                .bind(format!("Added by the play {}: {}", play.name, event.what))
                .execute(db)
                .await?;
            let moved: Option<Uuid> = sqlx::query_scalar(
                "update companies c set segment_id = $3, status = 'new' where c.project_id = $1 and c.domain = $2::citext and c.status <> 'excluded'
                   and not exists (select 1 from leads l where l.project_id = c.project_id and (l.company_id = c.id or l.company_domain = c.domain))
                 returning c.id",
            )
            .bind(play.project_id)
            .bind(domain)
            .bind(segment_id)
            .fetch_optional(db)
            .await?;
            let Some(company_id) = moved else { return Ok(Done::skip("It already has leads, or it's excluded")) };
            let run = agent::enqueue(db, kind::FIND_PEOPLE, Some(play.project_id), Some(org_id), json!({ "segment_id": segment_id, "company_ids": [company_id] })).await?;
            Ok(Done { skipped: None, run_id: Some(run) })
        }
        "write_email" => {
            let Some(lead_id) = event.lead_id else { return Ok(Done::skip("No lead")) };
            let (email, segment_id, emailed): (Option<String>, Option<Uuid>, bool) = sqlx::query_as(
                "select l.email, l.segment_id, exists (select 1 from email_messages e where e.lead_id = l.id and e.kind = 'initial' and e.status <> 'cancelled')
                 from leads l where l.id = $1",
            )
            .bind(lead_id)
            .fetch_one(db)
            .await?;
            if emailed {
                return Ok(Done::skip("Already has a first email"));
            }
            if email.is_none() {
                return Ok(Done::skip("No email address yet"));
            }
            let run = agent::enqueue(db, kind::WRITE_EMAILS, Some(play.project_id), Some(org_id), json!({ "segment_id": segment_id, "lead_ids": [lead_id] })).await?;
            Ok(Done { skipped: None, run_id: Some(run) })
        }
        _ => {
            let public = state.cfg.public_url.trim_end_matches('/');
            let link = match event.lead_id {
                Some(id) => format!("{public}/app/p/{}/leads?lead={id}", play.project_id),
                None => format!("{public}/app/p/{}/companies", play.project_id),
            };
            let summary = format!("{}: {}", event.name, event.what);
            let label = format!("Play: {}", play.name);
            let alert = ChatAlert { label: &label, summary: &summary, quote: None, link: &link, link_text: "Open it in Splee" };
            hooks::post_chat(db, play.project_id, &alert).await;
            Ok(Done { skipped: None, run_id: None })
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration as Days;

    use super::*;

    #[test]
    fn companies_go_into_campaigns_and_leads_get_emails() {
        assert!(fits("funding", "add_to_campaign") && !fits("funding", "write_email"));
        assert!(fits("job_change", "write_email") && !fits("score", "add_to_campaign"));
        assert!(TRIGGERS.iter().all(|t| fits(t, "notify")));
    }

    struct Seeded {
        project: Uuid,
        segment: Uuid,
    }

    async fn seed(db: &PgPool) -> Seeded {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(db).await.unwrap();
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Funded') returning id").bind(project).fetch_one(db).await.unwrap();
        Seeded { project, segment }
    }

    async fn play(db: &PgPool, s: &Seeded, trigger: &str, action: &str, cap: i32, since: Ts) -> Play {
        sqlx::query_as::<_, Play>(
            "insert into plays (project_id, name, trigger, action, segment_id, daily_cap, checked_at) values ($1, 'Raised money', $2, $3, $4, $5, $6)
             returning id, project_id, name, trigger, action, segment_id, min_score, daily_cap, checked_at",
        )
        .bind(s.project)
        .bind(trigger)
        .bind(action)
        .bind(s.segment)
        .bind(cap)
        .bind(since)
        .fetch_one(db)
        .await
        .unwrap()
    }

    async fn signal(db: &PgPool, s: &Seeded, domain: &str, kind: &str, key: &str, seen: Ts) {
        sqlx::query("insert into companies (project_id, domain, name) values ($1, $2, $2) on conflict do nothing").bind(s.project).bind(domain).execute(db).await.unwrap();
        sqlx::query(
            "insert into company_signals (project_id, domain, kind, source, key, happened_on, title, amount, url, first_seen)
             values ($1, $2, $3, 'sec', $4, current_date, 'Acme, Inc.', 12000000, 'https://www.sec.gov/x', $5)",
        )
        .bind(s.project)
        .bind(domain)
        .bind(kind)
        .bind(key)
        .bind(seen)
        .execute(db)
        .await
        .unwrap();
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_funding_play_adds_each_company_once_and_holds_the_rest_past_its_cap(db: PgPool) {
        let s = seed(&db).await;
        let state = crate::autopilot::tests::state(db.clone());
        let now = Utc::now();
        let p = play(&db, &s, "funding", "add_to_campaign", 2, now - Days::hours(1)).await;
        signal(&db, &s, "old.example", "funding_filing", "a", now - Days::hours(2)).await;
        signal(&db, &s, "one.example", "funding_filing", "b", now - Days::minutes(50)).await;
        signal(&db, &s, "one.example", "funding_post", "c", now - Days::minutes(40)).await;
        signal(&db, &s, "launch.example", "launch", "d", now - Days::minutes(30)).await;
        signal(&db, &s, "two.example", "funding_post", "e", now - Days::minutes(20)).await;
        signal(&db, &s, "three.example", "funding_post", "f", now - Days::minutes(10)).await;
        sqlx::query("insert into leads (project_id, company_domain, full_name) values ($1, 'two.example', 'Ana')").bind(s.project).execute(&db).await.unwrap();

        assert_eq!(run(&state, p.id, now).await.unwrap(), 2);
        let fired: Vec<(String, String, String, Option<Uuid>)> =
            sqlx::query_as("select domain::text, outcome, what, run_id from play_firings order by created_at, domain").fetch_all(&db).await.unwrap();
        assert_eq!(fired.len(), 2, "before the play, a launch and past the cap are left out: {fired:?}");
        assert_eq!((fired[0].0.as_str(), fired[0].1.as_str()), ("one.example", "done"));
        assert!(fired[0].2.starts_with("Raised $12M"), "{}", fired[0].2);
        assert_eq!((fired[1].0.as_str(), fired[1].1.as_str()), ("two.example", "skipped"), "it already has a lead");
        let (segment, input): (Option<Uuid>, serde_json::Value) = sqlx::query_as(
            "select c.segment_id, r.input from companies c, agent_runs r where c.domain = 'one.example' and r.id = $1",
        )
        .bind(fired[0].3.unwrap())
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(segment, Some(s.segment));
        assert_eq!(input["segment_id"], json!(s.segment));

        // three.example waits for room, and comes up once the day's firings age out.
        let checked: Ts = sqlx::query_scalar("select checked_at from plays").fetch_one(&db).await.unwrap();
        assert!(checked < now);
        assert_eq!(run(&state, p.id, now).await.unwrap(), 0);
        sqlx::query("update play_firings set created_at = now() - interval '2 days'").execute(&db).await.unwrap();
        assert_eq!(run(&state, p.id, now).await.unwrap(), 1);
        let three: String = sqlx::query_scalar("select outcome from play_firings where domain = 'three.example'").fetch_one(&db).await.unwrap();
        assert_eq!(three, "done");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_job_change_play_writes_the_first_email_only_when_it_can(db: PgPool) {
        let s = seed(&db).await;
        let state = crate::autopilot::tests::state(db.clone());
        let now = Utc::now();
        let watched: Uuid = sqlx::query_scalar("insert into watched_people (project_id, full_name, relationship) values ($1, 'Ana', 'won') returning id")
            .bind(s.project)
            .fetch_one(&db)
            .await
            .unwrap();
        for (name, email) in [("Ana", Some("ana@new.example")), ("Bo", None)] {
            let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, email) values ($1, $2, $3) returning id")
                .bind(s.project)
                .bind(name)
                .bind(email)
                .fetch_one(&db)
                .await
                .unwrap();
            sqlx::query("insert into job_changes (project_id, watched_id, move_number, old_company, new_company, new_lead_id, seen_at) values ($1, $2, $3, 'Acme', 'New Co', $4, $5)")
                .bind(s.project)
                .bind(watched)
                .bind(if email.is_some() { 1 } else { 2 })
                .bind(lead)
                .bind(now - Days::minutes(5))
                .execute(&db)
                .await
                .unwrap();
        }
        let p = play(&db, &s, "job_change", "write_email", 20, now - Days::hours(1)).await;
        assert_eq!(run(&state, p.id, now).await.unwrap(), 2);
        let fired: Vec<(String, String, String)> =
            sqlx::query_as("select l.full_name, f.outcome, f.what from play_firings f join leads l on l.id = f.lead_id order by l.full_name").fetch_all(&db).await.unwrap();
        assert_eq!(fired[0], ("Ana".into(), "done".into(), "Moved to New Co from Acme".into()));
        assert_eq!((fired[1].1.as_str()), "skipped", "Bo has no address yet");
        let runs: i64 = sqlx::query_scalar("select count(*) from agent_runs where kind = 'write_emails'").fetch_one(&db).await.unwrap();
        assert_eq!(runs, 1);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_cap_that_stops_between_events_at_the_same_moment_loses_none(db: PgPool) {
        let s = seed(&db).await;
        let state = crate::autopilot::tests::state(db.clone());
        let now = Utc::now();
        let at = now - Days::minutes(10);
        for (domain, key) in [("a.example", "a"), ("b.example", "b"), ("c.example", "c")] {
            signal(&db, &s, domain, "funding_post", key, at).await;
        }
        let p = play(&db, &s, "funding", "notify", 1, now - Days::hours(1)).await;
        for _ in 0..3 {
            assert_eq!(run(&state, p.id, now).await.unwrap(), 1);
            sqlx::query("update play_firings set created_at = now() - interval '2 days'").execute(&db).await.unwrap();
        }
        let fired: i64 = sqlx::query_scalar("select count(distinct subject) from play_firings").fetch_one(&db).await.unwrap();
        assert_eq!(fired, 3);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn two_backends_never_act_on_the_same_event_and_a_switched_off_play_does_nothing(db: PgPool) {
        let s = seed(&db).await;
        let state = crate::autopilot::tests::state(db.clone());
        let now = Utc::now();
        signal(&db, &s, "one.example", "funding_post", "a", now - Days::minutes(10)).await;
        let p = play(&db, &s, "funding", "add_to_campaign", 20, now - Days::hours(1)).await;

        // Another backend holds the play.
        let mut other = db.begin().await.unwrap();
        sqlx::query("select pg_advisory_xact_lock(hashtextextended('play:' || $1::text, 0))").bind(p.id).execute(&mut *other).await.unwrap();
        assert_eq!(run(&state, p.id, now).await.unwrap(), 0);
        other.rollback().await.unwrap();

        let (a, b) = tokio::join!(run(&state, p.id, now), run(&state, p.id, now));
        assert_eq!(a.unwrap() + b.unwrap(), 1);
        let runs: i64 = sqlx::query_scalar("select count(*) from agent_runs where kind = 'find_people'").fetch_one(&db).await.unwrap();
        assert_eq!(runs, 1, "one find_people run, not one per backend");

        signal(&db, &s, "two.example", "funding_post", "b", now - Days::minutes(5)).await;
        sqlx::query("update plays set enabled = false").execute(&db).await.unwrap();
        assert_eq!(run(&state, p.id, now).await.unwrap(), 0);
    }
}
