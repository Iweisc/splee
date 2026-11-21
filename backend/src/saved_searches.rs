//! Saved company database searches (routes/saved_searches.rs) and the check that finds the companies that match one and
//! didn't before. It tells the search's owner in their daily digest and by push, the project's chat when the search asks,
//! and can add the new companies to a campaign. It only reads the company database, and never starts an agent run.

use std::time::Duration;

use chrono::{SubsecRound, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::directory;
use crate::error::AppResult;
use crate::hooks::{self, ChatAlert};
use crate::models::{SavedSearch, Ts};
use crate::push::{self, Pref, Push};
use crate::routes::directory::{SearchQuery, add_domains};
use crate::state::AppState;

/// A search keeps every domain it has matched, so one that matches more than this isn't watched.
pub const SEEN_CAP: i64 = 10_000;
/// New matches one check adds to the search's campaign, so loading a big country can't flood it.
pub const ADD_CAP: i64 = 200;
/// Searches one tick checks, the longest unchecked first. Each is checked about hourly.
const PER_TICK: i64 = 50;

impl SavedSearch {
    pub fn query(&self) -> SearchQuery {
        SearchQuery {
            q: Some(self.q.clone()),
            country: Some(self.country.clone()),
            city: Some(self.city.clone()),
            category: Some(self.category.clone()),
            has_email: self.has_email,
            has_phone: self.has_phone,
            tech: Some(self.tech.clone()),
        }
    }
}

/// What a check found: when, how many new companies, and the campaign they went into with how many were added.
#[derive(Debug, PartialEq)]
pub struct Found {
    pub at: Ts,
    pub new: i64,
    pub added: Option<(String, u64)>,
}

/// Records the companies that match the search and didn't before. The first check records what already matches and
/// returns None, so nobody hears about the whole existing result set. So does a check that finds nothing new, and one
/// that finds the search has grown past SEEN_CAP, which stops watching it and drops its matches.
pub async fn check(db: &PgPool, search: &SavedSearch) -> AppResult<Option<Found>> {
    let filter = search.query().search()?;
    // Whole seconds, so the alert's link names exactly the found_at its companies have.
    let at = Utc::now().trunc_subsecs(0);
    let seen: i64 = sqlx::query_scalar("select count(*) from saved_search_matches where search_id = $1").bind(search.id).fetch_one(db).await?;
    let found_at = search.baseline_at.map(|_| at);
    let new = directory::record_unseen(db, &filter, search.id, found_at, SEEN_CAP - seen + 1).await? as i64;
    if seen + new > SEEN_CAP {
        sqlx::query("delete from saved_search_matches where search_id = $1").bind(search.id).execute(db).await?;
        sqlx::query("update saved_searches set too_broad = true, baseline_at = coalesce(baseline_at, $2), checked_at = $2 where id = $1")
            .bind(search.id)
            .bind(at)
            .execute(db)
            .await?;
        return Ok(None);
    }
    sqlx::query("update saved_searches set baseline_at = coalesce(baseline_at, $2), checked_at = $2 where id = $1").bind(search.id).bind(at).execute(db).await?;
    if found_at.is_none() || new == 0 {
        return Ok(None);
    }
    let added = match search.add_to_segment_id {
        Some(segment_id) => match add_new(db, search, segment_id, at).await {
            Ok(added) => added,
            Err(e) => {
                // Forgotten again, so the next check finds them and retries the campaign instead of skipping them for good.
                sqlx::query("delete from saved_search_matches where search_id = $1 and found_at = $2").bind(search.id).bind(at).execute(db).await?;
                return Err(e);
            }
        },
        None => None,
    };
    Ok(Some(Found { at, new, added }))
}

/// Adds the most reliable ADD_CAP of the companies found at `at` to the campaign, through the Companies page's add, so
/// companies the project has and do-not-contact ones are skipped. None when the campaign isn't in the project anymore.
async fn add_new(db: &PgPool, search: &SavedSearch, segment_id: Uuid, at: Ts) -> AppResult<Option<(String, u64)>> {
    let campaign: Option<(Option<Uuid>, String)> =
        sqlx::query_as("select p.org_id, s.name from segments s join projects p on p.id = s.project_id where s.id = $1 and s.project_id = $2")
            .bind(segment_id)
            .bind(search.project_id)
            .fetch_optional(db)
            .await?;
    let Some((Some(org_id), name)) = campaign else { return Ok(None) };
    let domains: Vec<String> = sqlx::query_scalar(
        "select m.domain from saved_search_matches m join directory_companies d on d.domain = m.domain
         where m.search_id = $1 and m.found_at = $2 order by d.confidence desc, d.domain limit $3",
    )
    .bind(search.id)
    .bind(at)
    .bind(ADD_CAP)
    .fetch_all(db)
    .await?;
    if domains.is_empty() {
        return Ok(None);
    }
    let added = add_domains(db, org_id, search.project_id, segment_id, &domains).await?;
    Ok(Some((name, added.added)))
}

/// Where an alert links: the Companies page with the search open and the companies found since `since` first.
pub fn link(public_url: &str, project_id: Uuid, search_id: Uuid, since: Ts) -> String {
    format!("{public_url}/app/p/{project_id}/companies?tab=database&search={search_id}&since={}", since.format("%Y-%m-%dT%H:%M:%SZ"))
}

/// "12 new companies match Dentists in Leeds"
fn matching(new: i64, name: &str) -> String {
    if new == 1 { format!("1 new company matches {name}") } else { format!("{new} new companies match {name}") }
}

pub fn summary(search: &SavedSearch, found: &Found) -> String {
    let mut text = format!("{}.", matching(found.new, &search.name));
    if let Some((campaign, added)) = &found.added {
        let of = if found.new > ADD_CAP { format!(" of the first {ADD_CAP}") } else { String::new() };
        text += &format!(" Added {added}{of} to {campaign}.");
    }
    text
}

/// Tells the owner by push, with the daily digest's preference, and the project's chat when the search asks.
pub async fn announce(state: &AppState, search: &SavedSearch, found: &Found) {
    let link = link(&state.cfg.public_url, search.project_id, search.id, found.at);
    let summary = summary(search, found);
    if let Some(user_id) = search.user_id {
        let push = Push { title: format!("New companies for {}", search.name), body: summary.clone(), url: link.clone(), tag: format!("saved-search-{}", search.id) };
        push::member_alert(state, search.project_id, user_id, Pref::Digest, &push).await;
    }
    if search.chat_alerts {
        let alert = ChatAlert { label: "New companies", summary: &summary, quote: None, link: &link, link_text: "Open the search" };
        hooks::post_chat(&state.db, search.project_id, &alert).await;
    }
}

/// One line for each of the member's searches in the organization that found companies in the last day, for their digest.
pub async fn digest_lines(db: &PgPool, public_url: &str, user_id: Uuid, org_id: Uuid) -> sqlx::Result<Vec<String>> {
    let rows: Vec<(Uuid, Uuid, String, i64, Ts)> = sqlx::query_as(
        "select s.id, s.project_id, s.name, count(*), min(m.found_at) from saved_searches s
         join projects p on p.id = s.project_id join saved_search_matches m on m.search_id = s.id
         where s.user_id = $1 and p.org_id = $2 and m.found_at > now() - interval '1 day'
         group by s.id order by s.name, s.id",
    )
    .bind(user_id)
    .bind(org_id)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(id, project_id, name, new, since)| format!("{}: {}", matching(new, &name), link(public_url, project_id, id, since))).collect())
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(600), || tick(&state), "saved searches").await });
}

/// Checks the searches that are due. One search's failure, like a tool Splee no longer detects, doesn't stop the rest.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    for search in claim(&state.db).await? {
        match check(&state.db, &search).await {
            Ok(Some(found)) => announce(state, &search, &found).await,
            Ok(None) => {}
            Err(e) => tracing::warn!(search_id = %search.id, error = %e, "couldn't check a saved search"),
        }
    }
    Ok(())
}

/// Marks the searches not checked in the last hour as checked, and returns them. Claiming first keeps a second backend
/// from checking the same ones.
async fn claim(db: &PgPool) -> sqlx::Result<Vec<SavedSearch>> {
    sqlx::query_as(
        "update saved_searches set checked_at = now()
         where id in (select id from saved_searches where not too_broad and (checked_at is null or checked_at <= now() - interval '1 hour')
                      order by checked_at nulls first limit $1 for update skip locked)
         returning *",
    )
    .bind(PER_TICK)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests;
