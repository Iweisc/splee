//! The review window (migration 0179): with `project_settings.review_window_hours` set, a draft nobody reviewed in that time is
//! approved on its own and goes through the sender like any approved email, so the send window, daily limits, suppressions and
//! warmup still apply. It covers first emails and follow-ups in a sending campaign of an unpaused project. A draft someone edited
//! keeps waiting, and so do auto-replies and meeting follow-ups, which answer a person.

use std::sync::LazyLock;

use sqlx::PgPool;

/// The drafts the window covers, over `email_messages e`, `project_settings ps` and `segments s`. The Emails page and the Slack
/// digest say when each one sends with SENDS_AT, so this is the one place that decides it.
const COVERED: &str = "e.direction = 'outbound' and e.kind in ('initial', 'follow_up') and e.status = 'draft' and e.edited_at is null
       and ps.review_window_hours is not null and not ps.paused and s.status in ('working', 'scaling')";

/// When a draft sends on its own, or null when it waits for a person. A time in the past means the sender's next tick.
pub static SENDS_AT: LazyLock<String> =
    LazyLock::new(|| format!("case when {COVERED} then coalesce(e.written_at, e.created_at) + make_interval(hours => ps.review_window_hours) end"));

/// Approves the drafts whose window has passed, in one statement, and returns how many. A second backend running it at the same
/// time skips the rows this one holds, and the status check under the lock approves each draft once.
pub async fn approve_due(db: &PgPool) -> sqlx::Result<i64> {
    sqlx::query_scalar(&format!(
        "with due as (
           select e.id from email_messages e
             join projects p on p.id = e.project_id
             join project_settings ps on ps.project_id = e.project_id
             join segments s on s.id = e.segment_id
           where p.org_id is not null and {COVERED} and coalesce(e.written_at, e.created_at) <= now() - make_interval(hours => ps.review_window_hours)
           for update of e skip locked
         ), approved as (
           update email_messages e set status = 'approved', approved_via = 'review_window', scheduled_for = coalesce(e.scheduled_for, now())
           from due where e.id = due.id and e.status = 'draft'
           returning e.lead_id
         ), queued as (
           update leads set status = 'queued' where id in (select lead_id from approved) and status = 'new'
         )
         select count(*) from approved"
    ))
    .fetch_one(db)
    .await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;
    use crate::models::Ts;
    use crate::routes::emails::{UpdateBody, update_one};

    struct Seeded {
        org: Uuid,
        project: Uuid,
        campaign: Uuid,
    }

    /// A sending project with a 24 hour window and one working campaign.
    async fn seed(db: &PgPool) -> Seeded {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name, status) values ($1, 'Fieldnote', 'active') returning id").bind(org).fetch_one(db).await.unwrap();
        sqlx::query("insert into project_settings (project_id, paused, review_window_hours) values ($1, false, 24)").bind(project).execute(db).await.unwrap();
        let campaign: Uuid = sqlx::query_scalar("insert into segments (project_id, name, status) values ($1, 'Physiotherapists', 'working') returning id")
            .bind(project)
            .fetch_one(db)
            .await
            .unwrap();
        Seeded { org, project, campaign }
    }

    /// A draft of `kind` for a new lead in `campaign`, written `hours` ago.
    async fn draft(db: &PgPool, project: Uuid, campaign: Uuid, kind: &str, hours: i32) -> Uuid {
        sqlx::query_scalar(
            "with l as (insert into leads (project_id, segment_id, email) values ($1, $2, gen_random_uuid()::text || '@physio.example') returning id)
             insert into email_messages (project_id, lead_id, segment_id, direction, kind, step, subject, body, status, created_at)
             select $1, id, $2, 'outbound', $3, case when $3 = 'follow_up' then 1 else 0 end, 'Hi', 'Hello', 'draft', now() - make_interval(hours => $4)
             from l returning id",
        )
        .bind(project)
        .bind(campaign)
        .bind(kind)
        .bind(hours)
        .fetch_one(db)
        .await
        .unwrap()
    }

    async fn state_of(db: &PgPool, email: Uuid) -> (String, Option<String>, String) {
        sqlx::query_as(
            "select e.status, e.approved_via, l.status from email_messages e join leads l on l.id = e.lead_id where e.id = $1",
        )
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap()
    }

    /// What the Emails page shows, through an update that changes nothing.
    async fn sends_at(db: &PgPool, org: Uuid, email: Uuid) -> Option<Ts> {
        update_one(db, org, email, UpdateBody::default()).await.unwrap().sends_at
    }

    fn waiting() -> (String, Option<String>, String) {
        ("draft".into(), None, "new".into())
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_draft_past_the_window_goes_out_once_and_one_inside_it_waits(db: PgPool) {
        let s = seed(&db).await;
        let old = draft(&db, s.project, s.campaign, "initial", 25).await;
        let follow_up = draft(&db, s.project, s.campaign, "follow_up", 30).await;
        let fresh = draft(&db, s.project, s.campaign, "initial", 23).await;

        // Two backends at once, then a later tick: each draft is approved once.
        let (a, b) = tokio::join!(approve_due(&db), approve_due(&db));
        assert_eq!(a.unwrap() + b.unwrap(), 2);
        assert_eq!(approve_due(&db).await.unwrap(), 0);
        assert_eq!(state_of(&db, old).await, ("approved".into(), Some("review_window".into()), "queued".into()));
        assert_eq!(state_of(&db, follow_up).await.1.as_deref(), Some("review_window"));
        assert_eq!(state_of(&db, fresh).await, waiting());
        let due: bool = sqlx::query_scalar("select scheduled_for <= now() from email_messages where id = $1").bind(old).fetch_one(&db).await.unwrap();
        assert!(due, "it joins the queue now, for the sender's next tick");

        // The Emails page says when the waiting one sends, and a person's approval is recorded as theirs.
        let now = chrono::Utc::now();
        assert!(sends_at(&db, s.org, fresh).await.is_some_and(|t| t > now && t < now + chrono::Duration::hours(1)));
        assert_eq!(sends_at(&db, s.org, old).await, None);
        let approved = update_one(&db, s.org, fresh, UpdateBody { status: Some("approved".into()), ..Default::default() }).await.unwrap();
        assert_eq!(approved.email.approved_via.as_deref(), Some("person"));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn edited_paused_and_reply_drafts_wait_for_a_person(db: PgPool) {
        let s = seed(&db).await;

        // Saving an edit keeps it out of the window. Approving without changing the text doesn't count as one.
        let edited = draft(&db, s.project, s.campaign, "initial", 30).await;
        let row = update_one(&db, s.org, edited, UpdateBody { subject: Some("Quick idea".into()), ..Default::default() }).await.unwrap();
        assert_eq!(row.sends_at, None);
        let unchanged = draft(&db, s.project, s.campaign, "initial", 30).await;
        update_one(&db, s.org, unchanged, UpdateBody { subject: Some("Hi".into()), ..Default::default() }).await.unwrap();

        // Auto-replies and meeting follow-ups answer a person, so they wait too.
        let auto_reply = draft(&db, s.project, s.campaign, "auto_reply", 30).await;
        let meeting = draft(&db, s.project, s.campaign, "meeting_follow_up", 30).await;

        // A paused campaign's drafts wait, and so does a campaign that was never launched.
        let paused: Uuid = sqlx::query_scalar("insert into segments (project_id, name, status) values ($1, 'Dentists', 'paused') returning id")
            .bind(s.project)
            .fetch_one(&db)
            .await
            .unwrap();
        let in_paused = draft(&db, s.project, paused, "initial", 30).await;
        let unlaunched: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Vets') returning id")
            .bind(s.project)
            .fetch_one(&db)
            .await
            .unwrap();
        let in_unlaunched = draft(&db, s.project, unlaunched, "initial", 30).await;

        assert_eq!(approve_due(&db).await.unwrap(), 1, "only the draft saved without a change");
        assert_eq!(state_of(&db, unchanged).await.1.as_deref(), Some("review_window"));
        for id in [edited, auto_reply, meeting, in_paused, in_unlaunched] {
            assert_eq!(state_of(&db, id).await, waiting());
        }

        // A paused project holds the rest.
        let held = draft(&db, s.project, s.campaign, "initial", 30).await;
        sqlx::query("update project_settings set paused = true").execute(&db).await.unwrap();
        assert_eq!(approve_due(&db).await.unwrap(), 0);
        assert_eq!(state_of(&db, held).await, waiting());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_rewritten_draft_gets_a_full_window_and_an_edit_takes_back_the_windows_approval(db: PgPool) {
        let s = seed(&db).await;

        // The agent rewrote it an hour ago, so the new text waits its own 24 hours.
        let rewritten = draft(&db, s.project, s.campaign, "initial", 30).await;
        sqlx::query("update email_messages set written_at = now() - interval '1 hour' where id = $1").bind(rewritten).execute(&db).await.unwrap();
        assert_eq!(approve_due(&db).await.unwrap(), 0);
        assert_eq!(state_of(&db, rewritten).await, waiting());

        // The window approved it while someone was editing. Their changed text goes back to wait for a person.
        let raced = draft(&db, s.project, s.campaign, "initial", 25).await;
        assert_eq!(approve_due(&db).await.unwrap(), 1);
        let edit = UpdateBody { body: Some("Hello, rewritten by hand".into()), ..Default::default() };
        let row = update_one(&db, s.org, raced, edit).await.unwrap();
        assert_eq!((row.email.status.as_str(), row.email.approved_via.as_deref()), ("draft", None));
        assert_eq!(approve_due(&db).await.unwrap(), 0);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn without_a_window_nothing_changes(db: PgPool) {
        let s = seed(&db).await;
        sqlx::query("update project_settings set review_window_hours = null").execute(&db).await.unwrap();
        let old = draft(&db, s.project, s.campaign, "initial", 24 * 30).await;
        assert_eq!(approve_due(&db).await.unwrap(), 0);
        assert_eq!(state_of(&db, old).await, waiting());
        assert_eq!(sends_at(&db, s.org, old).await, None);
    }
}
