//! A/B tests of a campaign's angle. Each new lead gets a variant round-robin when its first email is written, the writer
//! follows that variant's instructions, and follow-ups keep it. Results compare variants on hot rate (reply rate while no
//! variant has a hot lead) with a two-proportion z-test against the leader.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::models::{Lead, Segment, Ts, Variant};

pub const MAX_VARIANTS: usize = 4;
/// Below this many sends per variant the rates swing too much to compare, whatever the test says.
pub const MIN_SENDS: i64 = 30;
/// Two-sided 95%.
const Z_95: f64 = 1.96;

/// Gives each lead that has no variant of its own campaign the next one round-robin, and saves it on the lead.
/// A lead keeps its variant from then on, so rewrites and follow-ups stay on the same angle.
pub async fn assign(db: &PgPool, leads: &mut [Lead]) -> sqlx::Result<()> {
    let segments: HashSet<Uuid> = leads.iter().filter_map(|l| l.segment_id).collect();
    for segment_id in segments {
        let mut tx = db.begin().await?;
        // Two write runs for one campaign could otherwise both count the same totals and pick the same variant.
        sqlx::query("select 1 from segments where id = $1 for update").bind(segment_id).execute(&mut *tx).await?;
        let rows: Vec<(Uuid, bool, i64)> = sqlx::query_as(
            "select v.id, v.paused_at is null, (select count(*) from leads l where l.variant_id = v.id)
             from segment_variants v where v.segment_id = $1 order by v.position, v.created_at",
        )
        .bind(segment_id)
        .fetch_all(&mut *tx)
        .await?;
        let known: HashSet<Uuid> = rows.iter().map(|r| r.0).collect();
        let mut active: Vec<(Uuid, i64)> = rows.iter().filter(|r| r.1).map(|r| (r.0, r.2)).collect();
        // Read again under the lock: another run may have given these leads a variant since they were loaded.
        let ids: Vec<Uuid> = leads.iter().filter(|l| l.segment_id == Some(segment_id)).map(|l| l.id).collect();
        let current: HashMap<Uuid, Option<Uuid>> =
            sqlx::query_as("select id, variant_id from leads where id = any($1)").bind(&ids).fetch_all(&mut *tx).await?.into_iter().collect();
        for lead in leads.iter_mut().filter(|l| l.segment_id == Some(segment_id)) {
            if let Some(variant) = current.get(&lead.id) {
                lead.variant_id = *variant;
            }
            if lead.variant_id.is_some_and(|id| known.contains(&id)) {
                continue;
            }
            // A lead moved from another campaign drops that campaign's variant.
            let next = next_variant(&mut active);
            if next != lead.variant_id {
                sqlx::query("update leads set variant_id = $2 where id = $1").bind(lead.id).bind(next).execute(&mut *tx).await?;
                lead.variant_id = next;
            }
        }
        tx.commit().await?;
    }
    Ok(())
}

/// Round-robin by count rather than by turn: the active variant with the fewest leads, the first in order on a tie. A variant
/// added later catches up before the others get more, so their send counts stay comparable.
fn next_variant(active: &mut [(Uuid, i64)]) -> Option<Uuid> {
    let next = active.iter_mut().min_by_key(|(_, n)| *n)?;
    next.1 += 1;
    Some(next.0)
}

/// A variant's numbers, by lead: leads whose first email of this variant went out, and of those, how many replied (bounces
/// aside), turned hot, bounced or unsubscribed. Follow-ups keep the variant, so a reply to one counts too.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct VariantStats {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub variant: Variant,
    pub sent: i64,
    pub replies: i64,
    pub hot: i64,
    pub bounced: i64,
    pub unsubscribed: i64,
}

impl VariantStats {
    fn hits(&self, metric: Metric) -> i64 {
        match metric {
            Metric::Hot => self.hot,
            Metric::Reply => self.replies,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Hot,
    Reply,
}

impl Metric {
    fn label(self) -> &'static str {
        match self {
            Metric::Hot => "hot rate",
            Metric::Reply => "reply rate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Fewer than two variants with sends, or one under MIN_SENDS.
    TooFew,
    /// Enough sends, but the leader isn't ahead of every other variant at 95%.
    Unclear,
    /// The leader beats every other variant at 95%.
    Winner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Comparison {
    pub metric: Metric,
    /// Index into the compared slice.
    pub leader: usize,
    pub verdict: Verdict,
}

/// Whether `a` hits beats `b` hits at 95%, by a pooled two-proportion z-test.
pub fn beats(a_hits: i64, a_sent: i64, b_hits: i64, b_sent: i64) -> bool {
    if a_sent == 0 || b_sent == 0 {
        return false;
    }
    let (n1, n2) = (a_sent as f64, b_sent as f64);
    let (p1, p2) = (a_hits as f64 / n1, b_hits as f64 / n2);
    let pooled = (a_hits + b_hits) as f64 / (n1 + n2);
    let se = (pooled * (1.0 - pooled) * (1.0 / n1 + 1.0 / n2)).sqrt();
    se > 0.0 && (p1 - p2) / se > Z_95
}

/// Compares the variants that have sends. None when fewer than two have any.
pub fn compare(variants: &[&VariantStats]) -> Option<Comparison> {
    let tested: Vec<&VariantStats> = variants.iter().copied().filter(|v| v.sent > 0).collect();
    if tested.len() < 2 {
        return None;
    }
    let metric = if tested.iter().any(|v| v.hot > 0) { Metric::Hot } else { Metric::Reply };
    let rate = |v: &VariantStats| v.hits(metric) as f64 / v.sent as f64;
    // max_by keeps the last of equals, so walk backwards to keep the first in order on a tie.
    let (leader, best) = tested.iter().enumerate().rev().max_by(|a, b| rate(a.1).total_cmp(&rate(b.1)))?;
    // Every variant counts here, so one with no sends yet keeps the others from being called.
    let verdict = if variants.iter().any(|v| v.sent < MIN_SENDS) {
        Verdict::TooFew
    } else if tested.iter().enumerate().all(|(i, v)| i == leader || beats(best.hits(metric), best.sent, v.hits(metric), v.sent)) {
        Verdict::Winner
    } else {
        Verdict::Unclear
    };
    let leader = variants.iter().position(|v| v.variant.id == best.variant.id)?;
    Some(Comparison { metric, leader, verdict })
}

/// A campaign's test as the campaign page, the API and MCP show it.
#[derive(Debug, Serialize)]
pub struct AbTest {
    pub variants: Vec<VariantStats>,
    /// What the variants are compared on: hot rate, or reply rate while none has a hot lead.
    pub metric: Metric,
    pub verdict: Verdict,
    pub leader_id: Option<Uuid>,
    /// The verdict in plain words.
    pub note: String,
    /// Auto-pick's N: sends per variant before it keeps a clear winner. Null when it's off.
    pub pick_after: Option<i32>,
    pub winner_id: Option<Uuid>,
    pub picked_at: Option<Ts>,
    pub pick_note: String,
}

impl AbTest {
    fn new(segment: &Segment, variants: Vec<VariantStats>) -> Self {
        let all: Vec<&VariantStats> = variants.iter().collect();
        let comparison = compare(&all);
        let note = match comparison {
            _ if variants.len() < 2 => "Add a second variant to start comparing.".to_string(),
            None => format!("Not enough data yet. Each variant needs at least {MIN_SENDS} sends before the rates mean much."),
            Some(c) => {
                let leader = &variants[c.leader].variant.name;
                match c.verdict {
                    Verdict::TooFew => format!("Not enough data yet. Each variant needs at least {MIN_SENDS} sends before the rates mean much."),
                    Verdict::Unclear => format!("Not enough data yet. {leader} leads on {}, but the gap could still be chance.", c.metric.label()),
                    Verdict::Winner => format!("{leader} wins on {}. The gap is too big to be chance, at 95% confidence.", c.metric.label()),
                }
            }
        };
        Self {
            metric: comparison.map_or(Metric::Hot, |c| c.metric),
            verdict: comparison.map_or(Verdict::TooFew, |c| c.verdict),
            leader_id: comparison.map(|c| variants[c.leader].variant.id),
            note,
            pick_after: segment.variant_pick_after,
            winner_id: segment.variant_winner_id,
            picked_at: segment.variant_picked_at,
            pick_note: segment.variant_pick_note.clone(),
            variants,
        }
    }
}

/// The test of each of these campaigns that has variants.
pub async fn load(db: &PgPool, segments: &[&Segment]) -> sqlx::Result<HashMap<Uuid, AbTest>> {
    let ids: Vec<Uuid> = segments.iter().map(|s| s.id).collect();
    let rows = sqlx::query_as::<_, VariantStats>(
        "with sent as (
           select e.variant_id, l.status,
                  exists (select 1 from email_messages i where i.lead_id = e.lead_id and i.direction = 'inbound'
                          and i.classification is distinct from 'bounce') as replied
           from email_messages e join leads l on l.id = e.lead_id
           where e.variant_id in (select id from segment_variants where segment_id = any($1))
             and e.kind = 'initial' and e.direction = 'outbound' and e.status = 'sent'
         )
         select v.*, count(s.variant_id) as sent,
                count(*) filter (where s.replied) as replies,
                count(*) filter (where s.status in ('hot', 'meeting_booked')) as hot,
                count(*) filter (where s.status = 'bounced') as bounced,
                count(*) filter (where s.status = 'unsubscribed') as unsubscribed
         from segment_variants v left join sent s on s.variant_id = v.id
         where v.segment_id = any($1)
         group by v.id
         order by v.position, v.created_at",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?;
    let mut by_segment: HashMap<Uuid, Vec<VariantStats>> = HashMap::new();
    for row in rows {
        by_segment.entry(row.variant.segment_id).or_default().push(row);
    }
    Ok(segments.iter().filter_map(|s| Some((s.id, AbTest::new(s, by_segment.remove(&s.id)?)))).collect())
}

pub async fn load_one(db: &PgPool, segment: &Segment) -> sqlx::Result<AbTest> {
    Ok(load(db, &[segment]).await?.remove(&segment.id).unwrap_or_else(|| AbTest::new(segment, Vec::new())))
}

/// The winner auto-pick would keep, with its note, once every active variant has `after` sends (and at least MIN_SENDS)
/// and one beats each of the others at 95%.
fn pick(test: &AbTest, after: i32) -> Option<(Uuid, String)> {
    let active: Vec<&VariantStats> = test.variants.iter().filter(|v| v.variant.paused_at.is_none()).collect();
    let needed = i64::from(after).max(MIN_SENDS);
    if active.len() < 2 || active.iter().any(|v| v.sent < needed) {
        return None;
    }
    let c = compare(&active).filter(|c| c.verdict == Verdict::Winner)?;
    let winner = active[c.leader];
    let rate = |v: &VariantStats| v.hits(c.metric) as f64 / v.sent as f64 * 100.0;
    let runner_up = active.iter().enumerate().filter(|(i, _)| *i != c.leader).map(|(_, v)| rate(v)).fold(0.0, f64::max);
    let note = format!(
        "After {needed} sends each, {} had a {} of {:.1}% against {:.1}% for the next best, too big a gap to be chance. \
         The other variants are paused for new leads. Emails already written keep their variant.",
        winner.variant.name,
        c.metric.label(),
        rate(winner),
        runner_up,
    );
    Some((winner.variant.id, note))
}

/// Hourly, with the optimizer: campaigns with auto-pick on keep their winner once it's clear. Only new leads are affected.
pub async fn auto_pick(db: &PgPool) -> anyhow::Result<()> {
    let segments = sqlx::query_as::<_, Segment>("select * from segments where variant_pick_after is not null and variant_picked_at is null")
        .fetch_all(db)
        .await?;
    let tests = load(db, &segments.iter().collect::<Vec<_>>()).await?;
    for segment in &segments {
        let (Some(test), Some(after)) = (tests.get(&segment.id), segment.variant_pick_after) else { continue };
        if pick(test, after).is_none() {
            continue;
        }
        // Decided again under the campaign row lock the variant routes and assignment take, since the test may have been
        // reset, its threshold raised or a variant added since it was loaded. While it's held nothing can change them.
        let mut tx = db.begin().await?;
        let locked: Option<Segment> =
            sqlx::query_as("select * from segments where id = $1 and variant_pick_after is not null and variant_picked_at is null for update")
                .bind(segment.id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(locked) = locked else { continue };
        let Some(after) = locked.variant_pick_after else { continue };
        let Some((winner, note)) = pick(&load_one(db, &locked).await?, after) else { continue };
        tracing::info!(segment_id = %segment.id, %winner, "auto-pick kept a variant");
        sqlx::query("update segment_variants set paused_at = now() where segment_id = $1 and id <> $2 and paused_at is null")
            .bind(segment.id)
            .bind(winner)
            .execute(&mut *tx)
            .await?;
        sqlx::query("update segments set variant_winner_id = $2, variant_picked_at = now(), variant_pick_note = $3 where id = $1")
            .bind(segment.id)
            .bind(winner)
            .bind(note)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use chrono::Utc;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::{Metric, MIN_SENDS, Verdict, VariantStats, assign, auto_pick, beats, compare, load_one, next_variant};
    use crate::models::{Lead, Segment, Variant};

    fn stats(sent: i64, replies: i64, hot: i64) -> VariantStats {
        let variant = Variant {
            id: Uuid::new_v4(),
            segment_id: Uuid::nil(),
            name: String::new(),
            instructions: String::new(),
            position: 0,
            paused_at: None,
            created_at: Utc::now(),
        };
        VariantStats { variant, sent, replies, hot, bounced: 0, unsubscribed: 0 }
    }

    #[test]
    fn round_robin_takes_turns_and_lets_a_new_variant_catch_up() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut active = vec![(a, 0), (b, 0), (c, 0)];
        let picks: Vec<_> = (0..6).map(|_| next_variant(&mut active).unwrap()).collect();
        assert_eq!(picks, vec![a, b, c, a, b, c]);

        let d = Uuid::new_v4();
        active.push((d, 0));
        let picks: Vec<_> = (0..3).map(|_| next_variant(&mut active).unwrap()).collect();
        assert_eq!(picks, vec![d, d, a]);
        assert_eq!(next_variant(&mut []), None);
    }

    #[test]
    fn two_proportion_test_needs_a_real_gap() {
        // 15% against 3% of 100 sends each: z is about 2.97.
        assert!(beats(15, 100, 3, 100));
        assert!(!beats(3, 100, 15, 100));
        // 8% against 4%: z is about 1.19, which could be chance.
        assert!(!beats(8, 100, 4, 100));
        // The same 8% against 4% over 1000 sends each: z is about 3.7.
        assert!(beats(80, 1000, 40, 1000));
        assert!(!beats(0, 100, 0, 100));
        assert!(!beats(1, 0, 0, 100));
    }

    #[test]
    fn compares_on_hot_rate_and_falls_back_to_replies() {
        let (a, b) = (stats(100, 20, 15), stats(100, 25, 3));
        assert_eq!(compare(&[&a, &b]), Some(super::Comparison { metric: Metric::Hot, leader: 0, verdict: Verdict::Winner }));

        let (a, b) = (stats(100, 4, 0), stats(100, 16, 0));
        assert_eq!(compare(&[&a, &b]), Some(super::Comparison { metric: Metric::Reply, leader: 1, verdict: Verdict::Winner }));

        let (a, b) = (stats(100, 8, 8), stats(100, 4, 4));
        assert_eq!(compare(&[&a, &b]).unwrap().verdict, Verdict::Unclear);

        // A huge gap still says too few below MIN_SENDS.
        let (a, b) = (stats(MIN_SENDS - 1, 20, 20), stats(100, 0, 0));
        assert_eq!(compare(&[&a, &b]).unwrap().verdict, Verdict::TooFew);

        // The leader has to beat every other variant, not just the worst.
        let (a, b, c) = (stats(100, 15, 15), stats(100, 10, 10), stats(100, 2, 2));
        assert_eq!(compare(&[&a, &b, &c]), Some(super::Comparison { metric: Metric::Hot, leader: 0, verdict: Verdict::Unclear }));

        // A variant nobody has been sent yet keeps the others from being called.
        let (a, b, c) = (stats(100, 15, 15), stats(100, 2, 2), stats(0, 0, 0));
        assert_eq!(compare(&[&a, &b, &c]).unwrap().verdict, Verdict::TooFew);
        assert_eq!(compare(&[&a, &c]), None);
    }

    pub(crate) struct Campaign {
        pub project: Uuid,
        pub segment: Uuid,
        pub variants: Vec<Uuid>,
    }

    /// A project with one campaign and `variants` variants named A, B, C...
    pub(crate) async fn seed(db: &PgPool, variants: usize) -> Campaign {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(db).await.unwrap();
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name, status) values ($1, 'Dentists', 'working') returning id")
            .bind(project)
            .fetch_one(db)
            .await
            .unwrap();
        let mut ids = Vec::new();
        for (i, name) in ["A", "B", "C", "D"].iter().take(variants).enumerate() {
            let id: Uuid = sqlx::query_scalar(
                "insert into segment_variants (segment_id, name, instructions, position) values ($1, $2, $3, $4) returning id",
            )
            .bind(segment)
            .bind(name)
            .bind(format!("Lead with angle {name}."))
            .bind(i as i32)
            .fetch_one(db)
            .await
            .unwrap();
            ids.push(id);
        }
        Campaign { project, segment, variants: ids }
    }

    pub(crate) async fn add_leads(db: &PgPool, c: &Campaign, n: usize) -> Vec<Lead> {
        sqlx::query_as::<_, Lead>(
            "insert into leads (project_id, segment_id, email, first_name, created_at)
             select $1, $2, gen_random_uuid() || '@x.com', 'P' || g, now() + g * interval '1 second' from generate_series(1, $3) g returning *",
        )
        .bind(c.project)
        .bind(c.segment)
        .bind(n as i32)
        .fetch_all(db)
        .await
        .unwrap()
    }

    /// Sends a first email of `variant` to `n` new leads, `hot` of whom turn hot and `replied` more of whom reply.
    async fn send(db: &PgPool, c: &Campaign, variant: Uuid, n: i32, hot: i32, replied: i32) {
        sqlx::query(
            "with l as (insert into leads (project_id, segment_id, variant_id, status)
                        select $1, $2, $3, case when g <= $5 then 'hot' when g <= $5 + $6 then 'replied' else 'contacted' end
                        from generate_series(1, $4) g returning id, status),
                  e as (insert into email_messages (project_id, lead_id, segment_id, variant_id, direction, kind, status, sent_at)
                        select $1, id, $2, $3, 'outbound', 'initial', 'sent', now() from l)
             insert into email_messages (project_id, lead_id, direction, kind, status, classification)
             select $1, id, 'inbound', 'inbound', 'received', case when status = 'hot' then 'hot' else 'question' end
             from l where status <> 'contacted'",
        )
        .bind(c.project)
        .bind(c.segment)
        .bind(variant)
        .bind(n)
        .bind(hot)
        .bind(replied)
        .execute(db)
        .await
        .unwrap();
    }

    async fn segment(db: &PgPool, id: Uuid) -> Segment {
        sqlx::query_as::<_, Segment>("select * from segments where id = $1").bind(id).fetch_one(db).await.unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn assignment_is_round_robin_and_stable(db: PgPool) {
        let c = seed(&db, 3).await;
        let mut leads = add_leads(&db, &c, 4).await;
        assign(&db, &mut leads).await.unwrap();
        let first: Vec<_> = leads.iter().map(|l| l.variant_id.unwrap()).collect();
        assert_eq!(first, vec![c.variants[0], c.variants[1], c.variants[2], c.variants[0]]);

        // Assigning again, as a rewrite does, changes nothing. The next new leads carry on from where the turns stopped.
        let mut again: Vec<Lead> =
            sqlx::query_as("select * from leads where segment_id = $1 order by created_at").bind(c.segment).fetch_all(&db).await.unwrap();
        assign(&db, &mut again).await.unwrap();
        assert_eq!(again.iter().map(|l| l.variant_id.unwrap()).collect::<Vec<_>>(), first);
        let mut more = add_leads(&db, &c, 2).await;
        assign(&db, &mut more).await.unwrap();
        assert_eq!(more.iter().map(|l| l.variant_id.unwrap()).collect::<Vec<_>>(), vec![c.variants[1], c.variants[2]]);

        // A lead another run gave a variant since this one loaded it keeps that variant.
        let mut stale = add_leads(&db, &c, 1).await;
        sqlx::query("update leads set variant_id = $2 where id = $1").bind(stale[0].id).bind(c.variants[2]).execute(&db).await.unwrap();
        assign(&db, &mut stale).await.unwrap();
        assert_eq!(stale[0].variant_id, Some(c.variants[2]));

        // A paused variant gets no new leads.
        sqlx::query("update segment_variants set paused_at = now() where id = $1").bind(c.variants[0]).execute(&db).await.unwrap();
        let mut more = add_leads(&db, &c, 2).await;
        assign(&db, &mut more).await.unwrap();
        assert!(more.iter().all(|l| l.variant_id != Some(c.variants[0])));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn campaigns_without_variants_assign_nothing(db: PgPool) {
        let c = seed(&db, 0).await;
        let mut leads = add_leads(&db, &c, 3).await;
        assign(&db, &mut leads).await.unwrap();
        assert!(leads.iter().all(|l| l.variant_id.is_none()));
        let n: i64 = sqlx::query_scalar("select count(*) from leads where variant_id is not null").fetch_one(&db).await.unwrap();
        assert_eq!(n, 0);
        let test = load_one(&db, &segment(&db, c.segment).await).await.unwrap();
        assert!(test.variants.is_empty());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn results_count_leads_by_variant(db: PgPool) {
        let c = seed(&db, 2).await;
        send(&db, &c, c.variants[0], 40, 6, 4).await;
        send(&db, &c, c.variants[1], 10, 0, 1).await;
        sqlx::query("update leads set status = 'bounced' where id = (select id from leads where variant_id = $1 and status = 'contacted' limit 1)")
            .bind(c.variants[1])
            .execute(&db)
            .await
            .unwrap();
        let test = load_one(&db, &segment(&db, c.segment).await).await.unwrap();
        let counts: Vec<_> = test.variants.iter().map(|v| (v.sent, v.replies, v.hot, v.bounced)).collect();
        assert_eq!(counts, vec![(40, 10, 6, 0), (10, 1, 0, 1)]);
        assert_eq!((test.verdict, test.leader_id), (Verdict::TooFew, Some(c.variants[0])));
        assert!(test.note.starts_with("Not enough data yet"), "{}", test.note);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn auto_pick_keeps_only_a_significant_winner(db: PgPool) {
        let c = seed(&db, 2).await;
        sqlx::query("update segments set variant_pick_after = 50 where id = $1").bind(c.segment).execute(&db).await.unwrap();
        // Ahead but not by enough: 8% against 4%.
        send(&db, &c, c.variants[0], 50, 4, 0).await;
        send(&db, &c, c.variants[1], 50, 2, 0).await;
        auto_pick(&db).await.unwrap();
        let s = segment(&db, c.segment).await;
        assert_eq!((s.variant_winner_id, s.variant_picked_at), (None, None));
        let paused: i64 = sqlx::query_scalar("select count(*) from segment_variants where paused_at is not null").fetch_one(&db).await.unwrap();
        assert_eq!(paused, 0);

        // 50 more each makes it 11% against 3%, which is clear.
        send(&db, &c, c.variants[0], 50, 7, 0).await;
        send(&db, &c, c.variants[1], 50, 1, 0).await;
        let written: Uuid = sqlx::query_scalar(
            "insert into email_messages (project_id, lead_id, segment_id, variant_id, direction, kind, subject, body, status)
             select $1, id, $2, $3, 'outbound', 'initial', 'Hi', 'Hello', 'approved' from leads where variant_id = $3 limit 1 returning id",
        )
        .bind(c.project)
        .bind(c.segment)
        .bind(c.variants[1])
        .fetch_one(&db)
        .await
        .unwrap();
        auto_pick(&db).await.unwrap();
        let s = segment(&db, c.segment).await;
        assert_eq!(s.variant_winner_id, Some(c.variants[0]));
        assert!(s.variant_picked_at.is_some());
        assert!(s.variant_pick_note.contains("hot rate of 11.0% against 3.0%"), "{}", s.variant_pick_note);
        let paused: Vec<Uuid> = sqlx::query_scalar("select id from segment_variants where paused_at is not null").fetch_all(&db).await.unwrap();
        assert_eq!(paused, vec![c.variants[1]]);
        // The loser's written email is left as it was.
        let (status, variant): (String, Option<Uuid>) =
            sqlx::query_as("select status, variant_id from email_messages where id = $1").bind(written).fetch_one(&db).await.unwrap();
        assert_eq!((status.as_str(), variant), ("approved", Some(c.variants[1])));

        // New leads now all get the winner.
        let mut leads = add_leads(&db, &c, 3).await;
        assign(&db, &mut leads).await.unwrap();
        assert!(leads.iter().all(|l| l.variant_id == Some(c.variants[0])));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn auto_pick_waits_for_n_sends_per_variant(db: PgPool) {
        let c = seed(&db, 2).await;
        sqlx::query("update segments set variant_pick_after = 200 where id = $1").bind(c.segment).execute(&db).await.unwrap();
        send(&db, &c, c.variants[0], 100, 15, 0).await;
        send(&db, &c, c.variants[1], 100, 1, 0).await;
        auto_pick(&db).await.unwrap();
        assert_eq!(segment(&db, c.segment).await.variant_winner_id, None);
        let test = load_one(&db, &segment(&db, c.segment).await).await.unwrap();
        assert_eq!(test.verdict, Verdict::Winner);
    }
}
