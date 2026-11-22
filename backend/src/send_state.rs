//! Whether a campaign's emails go out now and, if not, the one thing holding them: explee.com's campaign `status`. It reads the
//! gates mail/sender.rs checks, in the order a person would clear them: the campaign, the project's pause, mailboxes, what's
//! queued, then the month's cap, autopilot's lead cap, the day's limit, the campaign's own, mailbox room and the sending hours. Replies to leads skip
//! most of these, so they aren't counted here.

use chrono::Duration;
use chrono_tz::Tz;
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::AppResult;
use crate::models::{EmailMessage, Mailbox, ProjectSettings, Segment, Ts};
use crate::{agent, autopilot, mail, monthly_cap};

#[derive(Serialize, Debug)]
pub struct SendState {
    pub campaign_id: Uuid,
    /// sending, scheduled, archived, not_started, paused, project_paused, no_mailbox, awaiting_approval, nothing_queued,
    /// monthly_cap_reached, lead_cap_reached, daily_limit_reached, campaign_limit_reached, mailboxes_full or outside_hours.
    pub state: &'static str,
    /// True for sending and scheduled, when nothing holds the campaign.
    pub sending_now: bool,
    pub message: String,
    /// The API call that clears what holds it, or null when it only has to wait or the fix is in the app.
    pub next_action: Option<String>,
    pub today: Today,
    /// The campaign calls that change something now, of start, stop, approve, archive and unarchive.
    pub allowed_actions: Vec<&'static str>,
}

#[derive(Serialize, Debug)]
pub struct Today {
    /// The campaign's first emails and follow-ups sent since midnight in the project's timezone.
    pub sent: i64,
    /// Its first emails and follow-ups approved to go out, now or when they're scheduled.
    pub queued: i64,
    /// Its first emails and follow-ups waiting as drafts for approval.
    pub drafts: i64,
    /// Every email the project sent today, replies included, against the daily limit all its campaigns share.
    pub project_sent: i64,
    pub daily_email_limit: i32,
    /// The campaign's own cap on `sent`, or null when only the project's limit applies.
    pub campaign_limit: Option<i32>,
    /// When its next email can go out, or null while it sends now or nothing is queued.
    pub next_send_at: Option<Ts>,
}

#[derive(FromRow, Default, Debug)]
struct Queue {
    queued: i64,
    /// Queued and due now.
    due: i64,
    follow_ups_due: i64,
    /// The soonest queued email scheduled after now.
    next_scheduled: Option<Ts>,
    drafts: i64,
    sent: i64,
}

struct Window {
    tz: Tz,
    days: Vec<i32>,
    start_hour: i32,
    end_hour: i32,
    /// With lead_time_zone, each time zone the campaign's queued emails go out in and the soonest one of them is due there.
    /// Empty when sending hours are the project's.
    lead_zones: Vec<(Tz, Ts)>,
}

impl Window {
    /// The first moment from `from` that an email can go out, which with lead_time_zone is the first in any lead zone at or
    /// after an email there is due.
    fn opens(&self, from: Ts) -> Option<Ts> {
        let opens = |tz: Tz, from: Ts| mail::window_opens(tz, &self.days, self.start_hour, self.end_hour, from);
        if self.lead_zones.is_empty() {
            return opens(self.tz, from);
        }
        self.lead_zones.iter().filter_map(|&(tz, due)| opens(tz, from.max(due))).min()
    }

    fn tomorrow(&self, now: Ts) -> Ts {
        // Any time 36 hours after midnight is tomorrow, even on a 23 or 25 hour day.
        mail::day_start(self.tz, mail::day_start(self.tz, now) + Duration::hours(36))
    }

    fn local(&self, at: Ts) -> String {
        format!("{} {}", at.with_timezone(&self.tz).format("%a %b %-d, %H:%M"), self.tz)
    }
}

/// What decide reads, all taken at one moment.
struct Facts {
    campaign_id: Uuid,
    project_id: Uuid,
    status: String,
    archived: bool,
    project_paused: bool,
    active_mailboxes: i64,
    /// One of its due emails has a mailbox to go out from now.
    mailbox_ready: bool,
    queue: Queue,
    unwritten: usize,
    project_sent: i64,
    daily_email_limit: i32,
    campaign_limit: Option<i32>,
    /// While the monthly cap is reached, when it resets.
    cap_resumes_at: Option<Ts>,
    lead_cap_reached: bool,
    window: Window,
}

/// The campaign of `org_id` with its project's settings.
pub async fn load(db: &PgPool, org_id: Uuid, segment: &Segment, settings: &ProjectSettings, now: Ts) -> AppResult<SendState> {
    let (autopilot, lead_cap): (bool, i32) = sqlx::query_as("select autopilot, autopilot_lead_cap from project_settings where project_id = $1")
        .bind(segment.project_id)
        .fetch_one(db)
        .await?;
    let lead_cap_reached =
        autopilot && autopilot::contacted_since(db, segment.project_id, autopilot::month_start(now)).await? >= i64::from(lead_cap);
    let tz = settings.timezone.parse().unwrap_or(Tz::UTC);
    let mut window = Window {
        tz,
        days: settings.send_days.clone(),
        start_hour: settings.send_start_hour,
        end_hour: settings.send_end_hour,
        lead_zones: Vec::new(),
    };
    let mut closed = (Vec::new(), Vec::new());
    if settings.lead_time_zone {
        (window.lead_zones, closed) = lead_zones(db, segment.id, &window, lead_cap_reached, now).await?;
    }
    let day_start = mail::day_start(window.tz, now);
    let queue: Queue = sqlx::query_as(
        "select count(*) filter (where status in ('approved', 'scheduled')) as queued,
                count(*) filter (where status in ('approved', 'scheduled') and (scheduled_for is null or scheduled_for <= $2)) as due,
                count(*) filter (where status in ('approved', 'scheduled') and kind = 'follow_up'
                                   and (scheduled_for is null or scheduled_for <= $2)) as follow_ups_due,
                min(scheduled_for) filter (where status in ('approved', 'scheduled') and scheduled_for > $2) as next_scheduled,
                count(*) filter (where status = 'draft') as drafts,
                count(*) filter (where status = 'sent' and sent_at >= $3) as sent
         from email_messages where segment_id = $1 and direction = 'outbound' and kind in ('initial', 'follow_up')",
    )
    .bind(segment.id)
    .bind(now)
    .bind(day_start)
    .fetch_one(db)
    .await?;
    let project_sent: i64 = sqlx::query_scalar(
        "select count(*) from email_messages where project_id = $1 and direction = 'outbound' and status = 'sent' and sent_at >= $2",
    )
    .bind(segment.project_id)
    .bind(day_start)
    .fetch_one(db)
    .await?;
    let active_mailboxes: i64 = sqlx::query_scalar("select count(*) from mailboxes where org_id = $1 and status = 'active'")
        .bind(org_id)
        .fetch_one(db)
        .await?;
    let facts = Facts {
        campaign_id: segment.id,
        project_id: segment.project_id,
        status: segment.status.clone(),
        archived: segment.archived_at.is_some(),
        project_paused: settings.paused,
        active_mailboxes,
        mailbox_ready: mailbox_ready(db, mail::sender::mailboxes_with_room(db, org_id).await?, segment.id, lead_cap_reached, &closed, now).await?,
        queue,
        unwritten: agent::leads_without_initial(db, segment.project_id, Some(segment.id)).await?.len(),
        project_sent,
        daily_email_limit: settings.daily_email_limit,
        campaign_limit: segment.daily_limit,
        cap_resumes_at: monthly_cap::status(db, segment.project_id, now).await?.resumes_at,
        lead_cap_reached,
        window,
    };
    Ok(decide(&facts, now))
}

/// Phone prefixes and countries, in pairs, as the sender's closed_zones gives them.
type Pairs = (Vec<String>, Vec<String>);

/// Each time zone the campaign's queued emails go out in, the leads' own or else the project's, with the soonest any of them is
/// due there, and the leads whose window is closed at `now`, which the sender leaves out. Leads are grouped by what their zone
/// depends on, as the sender does. Past autopilot's lead cap only follow-ups can go, so only they count.
async fn lead_zones(db: &PgPool, segment_id: Uuid, project: &Window, follow_ups_only: bool, now: Ts) -> AppResult<(Vec<(Tz, Ts)>, Pairs)> {
    let leads: Vec<(String, String, Ts)> = sqlx::query_as(
        "select left(l.phone, 4), l.country_code, min(greatest(coalesce(e.scheduled_for, $2), $2))
         from email_messages e join leads l on l.id = e.lead_id
         where e.segment_id = $1 and e.direction = 'outbound' and e.kind in ('initial', 'follow_up') and e.status in ('approved', 'scheduled')
           and (not $3 or e.kind = 'follow_up')
         group by 1, 2",
    )
    .bind(segment_id)
    .bind(now)
    .bind(follow_ups_only)
    .fetch_all(db)
    .await?;
    let (mut zones, mut closed): (Vec<(Tz, Ts)>, Pairs) = Default::default();
    for (phone, country, due) in leads {
        let tz = crate::phone::time_zone(&phone, &country).unwrap_or(project.tz);
        match zones.iter_mut().find(|(z, _)| *z == tz) {
            Some((_, soonest)) => *soonest = (*soonest).min(due),
            None => zones.push((tz, due)),
        }
        if !mail::in_window(tz, &project.days, project.start_hour, project.end_hour, now) {
            closed.0.push(phone);
            closed.1.push(country);
        }
    }
    Ok((zones, closed))
}

/// How many due emails mailbox_ready looks at. Past a first email, which any mailbox with room takes, it's follow-ups whose
/// threads are on full mailboxes, and 50 of those in a row is as good as all of them.
const CHECKED: i64 = 50;

/// Whether one of the campaign's due emails has a mailbox to go out from now, as the sender picks it: a first email takes any
/// mailbox with room, and a follow-up waits for its thread's. Past autopilot's lead cap only follow-ups can go, so only they count.
async fn mailbox_ready(db: &PgPool, rooms: Vec<Mailbox>, segment_id: Uuid, follow_ups_only: bool, closed: &Pairs, now: Ts) -> AppResult<bool> {
    if rooms.is_empty() {
        return Ok(false);
    }
    // Leads outside their window in their own time zones are left out, as the sender leaves them out.
    let due: Vec<EmailMessage> = sqlx::query_as(
        "select e.* from email_messages e join leads l on l.id = e.lead_id
         where e.segment_id = $1 and e.direction = 'outbound' and e.kind in ('initial', 'follow_up') and e.status in ('approved', 'scheduled')
           and (e.scheduled_for is null or e.scheduled_for <= $2) and (not $4 or e.kind = 'follow_up')
           and (left(l.phone, 4), l.country_code) not in (select * from unnest($5::text[], $6::text[]))
         order by e.kind = 'initial' desc, e.scheduled_for nulls first, e.created_at limit $3",
    )
    .bind(segment_id)
    .bind(now)
    .bind(CHECKED)
    .bind(follow_ups_only)
    .bind(&closed.0)
    .bind(&closed.1)
    .fetch_all(db)
    .await?;
    // Every due lead is outside their window, so the mailboxes aren't what holds the campaign. decide says outside_hours.
    if due.is_empty() && !closed.0.is_empty() {
        return Ok(true);
    }
    for email in &due {
        if mail::sender::pick_from(db, rooms.clone(), email, None).await?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn decide(f: &Facts, now: Ts) -> SendState {
    let id = f.campaign_id;
    let q = &f.queue;
    let running = matches!(f.status.as_str(), "working" | "scaling");
    let start = || Some(format!("POST /v1/campaigns/{id}/start"));
    let (state, message, next_action, next_send_at): (_, String, _, _) = if f.archived {
        ("archived", "Archived. Unarchive it, then start it, to send again".into(), Some(format!("POST /v1/campaigns/{id}/unarchive")), None)
    } else if f.status == "draft" {
        ("not_started", "Not started. Starting it approves its drafts and sends them on the project's schedule".into(), start(), None)
    } else if !running {
        ("paused", "Paused. Its unsent emails wait as drafts until it starts again".into(), start(), None)
    } else if f.project_paused {
        let resume = format!("PATCH /v1/projects/{} with {{\"sending\": true}}", f.project_id);
        ("project_paused", "Sending is paused for the whole project".into(), Some(resume), None)
    } else if f.active_mailboxes == 0 {
        ("no_mailbox", "No mailbox can send. Connect one, or fix the ones that stopped, on the app's Domains page".into(), None, None)
    } else if q.queued == 0 && q.drafts > 0 {
        let message = format!("{} {} for approval", count(q.drafts, "email"), if q.drafts == 1 { "waits" } else { "wait" });
        ("awaiting_approval", message, Some(format!("POST /v1/campaigns/{id}/approve")), None)
    } else if q.queued == 0 && f.unwritten > 0 {
        let message = format!("Nothing is queued, and {} {} no first email yet", count(f.unwritten as i64, "lead"), has(f.unwritten));
        ("nothing_queued", message, Some(format!("POST /v1/segments/{id}/write-emails")), None)
    } else if q.queued == 0 {
        let message = "Nothing is queued. Every lead has its first email, and follow-ups are written as they come due";
        ("nothing_queued", message.into(), Some(format!("POST /v1/segments/{id}/find-people")), None)
    } else if q.due == 0 {
        let at = q.next_scheduled.and_then(|t| f.window.opens(t));
        let message = match at {
            Some(at) => format!("Its next emails go out {}", f.window.local(at)),
            None => "Its next emails are scheduled, but the project has no sending hours".into(),
        };
        ("scheduled", message, None, at)
    } else if let Some(resumes) = f.cap_resumes_at {
        let message = "This month's emails reached the project's monthly cap. First emails and follow-ups wait for the 1st or a higher cap";
        ("monthly_cap_reached", message.into(), None, f.window.opens(resumes))
    } else if f.lead_cap_reached && q.follow_ups_due == 0 {
        let message = "Autopilot emailed this month's cap of new leads. First emails wait for the 1st or a higher autopilot_lead_cap";
        ("lead_cap_reached", message.into(), None, f.window.opens(autopilot::next_month(now)))
    } else if f.project_sent >= i64::from(f.daily_email_limit) {
        let message = format!("The project sent its {} for today", count(i64::from(f.daily_email_limit), "email"));
        ("daily_limit_reached", message, None, f.window.opens(f.window.tomorrow(now)))
    } else if let Some(limit) = f.campaign_limit.filter(|&l| q.sent >= i64::from(l)) {
        let message = format!("This campaign sent its {} for today", count(i64::from(limit), "email"));
        ("campaign_limit_reached", message, None, f.window.opens(f.window.tomorrow(now)))
    } else if !f.mailbox_ready {
        let message = "The mailboxes its emails go out from sent their most for today or are still warming up";
        ("mailboxes_full", message.into(), None, f.window.opens(f.window.tomorrow(now)))
    } else if let Some(at) = f.window.opens(now).filter(|&at| at > now) {
        ("outside_hours", format!("Outside sending hours. The next emails go out {}", f.window.local(at)), None, Some(at))
    } else if f.window.opens(now).is_none() {
        ("outside_hours", "The project has no sending hours".into(), None, None)
    } else if f.status == "scaling" {
        ("sending", "Sending ahead of the project's other campaigns, since it gets hot leads for fewer emails".into(), None, None)
    } else {
        ("sending", "Sending on the project's schedule".into(), None, None)
    };
    let mut allowed_actions = if f.archived {
        vec!["unarchive"]
    } else if running {
        vec!["stop"]
    } else {
        vec!["start"]
    };
    if !f.archived && q.drafts > 0 {
        allowed_actions.push("approve");
    }
    if !f.archived {
        allowed_actions.push("archive");
    }
    SendState {
        campaign_id: id,
        state,
        sending_now: matches!(state, "sending" | "scheduled"),
        message,
        next_action,
        today: Today {
            sent: q.sent,
            queued: q.queued,
            drafts: q.drafts,
            project_sent: f.project_sent,
            daily_email_limit: f.daily_email_limit,
            campaign_limit: f.campaign_limit,
            next_send_at,
        },
        allowed_actions,
    }
}

fn count(n: i64, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

fn has(n: usize) -> &'static str {
    if n == 1 { "has" } else { "have" }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    /// A sending campaign with 3 emails due, inside a Monday to Friday 9:00 to 17:00 London window.
    fn sending() -> Facts {
        Facts {
            campaign_id: Uuid::nil(),
            project_id: Uuid::nil(),
            status: "working".into(),
            archived: false,
            project_paused: false,
            active_mailboxes: 1,
            mailbox_ready: true,
            queue: Queue { queued: 3, due: 3, ..Default::default() },
            unwritten: 0,
            project_sent: 0,
            daily_email_limit: 30,
            campaign_limit: None,
            cap_resumes_at: None,
            lead_cap_reached: false,
            window: Window { tz: chrono_tz::Europe::London, days: vec![1, 2, 3, 4, 5], start_hour: 9, end_hour: 17, lead_zones: Vec::new() },
        }
    }

    // Wednesday, November 18, 2026 at 12:00 in London, which is UTC in November.
    fn noon() -> Ts {
        Utc.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap()
    }

    #[test]
    fn the_first_gate_a_person_would_clear_is_the_one_reported() {
        let state = |change: &dyn Fn(&mut Facts)| {
            let mut f = sending();
            change(&mut f);
            decide(&f, noon()).state
        };
        assert_eq!(state(&|_| {}), "sending");
        assert_eq!(state(&|f| f.archived = true), "archived");
        assert_eq!(state(&|f| f.status = "draft".into()), "not_started");
        assert_eq!(state(&|f| f.status = "paused".into()), "paused");
        // The project's pause holds a campaign that's queued and within its hours.
        assert_eq!(state(&|f| f.project_paused = true), "project_paused");
        assert_eq!(state(&|f| f.active_mailboxes = 0), "no_mailbox");
        assert_eq!(state(&|f| f.queue = Queue { drafts: 2, ..Default::default() }), "awaiting_approval");
        assert_eq!(state(&|f| f.queue = Queue::default()), "nothing_queued");
        // Over the daily limit too, but nothing is due today, so the limit doesn't matter yet.
        let next_week = Utc.with_ymd_and_hms(2026, 11, 25, 10, 0, 0).unwrap();
        assert_eq!(state(&|f| {
            f.queue = Queue { queued: 1, next_scheduled: Some(next_week), ..Default::default() };
            f.project_sent = 99;
        }), "scheduled");
        assert_eq!(state(&|f| f.cap_resumes_at = Some(Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap())), "monthly_cap_reached");
        assert_eq!(state(&|f| f.lead_cap_reached = true), "lead_cap_reached");
        // Follow-ups go to leads already contacted, so autopilot's lead cap doesn't hold them.
        assert_eq!(state(&|f| {
            f.lead_cap_reached = true;
            f.queue.follow_ups_due = 1;
        }), "sending");
        assert_eq!(state(&|f| f.project_sent = 30), "daily_limit_reached");
        assert_eq!(state(&|f| (f.campaign_limit, f.queue.sent) = (Some(5), 5)), "campaign_limit_reached");
        assert_eq!(state(&|f| (f.campaign_limit, f.queue.sent) = (Some(5), 4)), "sending");
        assert_eq!(state(&|f| f.mailbox_ready = false), "mailboxes_full");
        assert_eq!(state(&|f| f.window.start_hour = 13), "outside_hours");
    }

    #[test]
    fn each_wait_says_when_sending_starts_again_in_the_projects_hours() {
        let at = |change: &dyn Fn(&mut Facts)| {
            let mut f = sending();
            change(&mut f);
            let s = decide(&f, noon());
            (s.state, s.today.next_send_at, s.message)
        };
        let (state, next, message) = at(&|f| f.window.start_hour = 13);
        assert_eq!((state, next), ("outside_hours", Some(Utc.with_ymd_and_hms(2026, 11, 18, 13, 0, 0).unwrap())));
        assert_eq!(message, "Outside sending hours. The next emails go out Wed Nov 18, 13:00 Europe/London");
        // Tomorrow's window, and Monday's from a Friday.
        assert_eq!(at(&|f| f.project_sent = 30).1, Some(Utc.with_ymd_and_hms(2026, 11, 19, 9, 0, 0).unwrap()));
        let friday = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
        let mut f = sending();
        f.project_sent = 30;
        assert_eq!(decide(&f, friday).today.next_send_at, Some(Utc.with_ymd_and_hms(2026, 11, 23, 9, 0, 0).unwrap()));
        // A follow-up scheduled for Saturday goes out on Monday at 9.
        let saturday = Utc.with_ymd_and_hms(2026, 11, 21, 10, 0, 0).unwrap();
        let (state, next, _) = at(&|f| f.queue = Queue { queued: 1, next_scheduled: Some(saturday), ..Default::default() });
        assert_eq!((state, next), ("scheduled", Some(Utc.with_ymd_and_hms(2026, 11, 23, 9, 0, 0).unwrap())));
        assert_eq!(at(&|f| f.cap_resumes_at = Some(Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap())).1,
                   Some(Utc.with_ymd_and_hms(2026, 12, 1, 9, 0, 0).unwrap()));
    }

    #[test]
    fn the_next_action_and_allowed_actions_follow_the_campaign() {
        let mut f = sending();
        f.queue.drafts = 1;
        let s = decide(&f, noon());
        assert_eq!((s.next_action, s.allowed_actions), (None, vec!["stop", "approve", "archive"]));
        f.status = "paused".into();
        let s = decide(&f, noon());
        assert_eq!(s.next_action.as_deref(), Some("POST /v1/campaigns/00000000-0000-0000-0000-000000000000/start"));
        assert_eq!(s.allowed_actions, ["start", "approve", "archive"]);
        f.archived = true;
        assert_eq!(decide(&f, noon()).allowed_actions, ["unarchive"]);

        let mut f = sending();
        f.queue = Queue::default();
        f.unwritten = 1;
        let s = decide(&f, noon());
        assert_eq!((s.message.as_str(), s.sending_now), ("Nothing is queued, and 1 lead has no first email yet", false));
        assert!(s.next_action.unwrap().ends_with("/write-emails"));
    }

    #[test]
    fn in_leads_time_zones_the_wait_ends_when_the_first_due_lead_can_be_emailed() {
        let utc = |d, h| Utc.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap();
        let mut f = sending();
        // Wednesday 03:00 UTC. A Tokyo lead is due at 13:00 UTC, 22:00 there, and a London lead at 18:00 UTC, after hours.
        f.window.lead_zones = vec![(chrono_tz::Asia::Tokyo, utc(18, 13)), (chrono_tz::Europe::London, utc(18, 18))];
        f.queue = Queue { queued: 2, next_scheduled: Some(utc(18, 13)), ..Default::default() };
        let s = decide(&f, utc(18, 3));
        assert_eq!((s.state, s.today.next_send_at), ("scheduled", Some(utc(19, 0))), "Thursday 09:00 in Tokyo, before London's");

        // Today's limit is used up, with a New York lead due. It starts again at midnight in the project's London, Wednesday
        // 19:00 in New York, so not at New York's Wednesday 09:00 but its Thursday 09:00.
        f.queue = Queue { queued: 1, due: 1, ..Default::default() };
        f.window.lead_zones = vec![(chrono_tz::America::New_York, utc(18, 3))];
        f.project_sent = 30;
        let s = decide(&f, utc(18, 3));
        assert_eq!((s.state, s.today.next_send_at), ("daily_limit_reached", Some(utc(19, 14))));
    }
}
