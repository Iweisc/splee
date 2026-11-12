//! Lead events written to a tab, one row each: a hot lead, a booked meeting or a won deal. The row is kept when the event
//! happens (`queue`, from hooks::send_lead_event), with the cells as they were then, and the worker writes every waiting row of a
//! tab in one append a tick. A lead's event gets one row per tab, ever: the row is unique per tab, lead and event, a tab is
//! written by one tick at a time, and a tab has one export.
//!
//! Google can write an append and still answer with an error, or not in time, and Splee can fail to note a write that worked.
//! So each row ends with its Splee ID, and every tick reads the tab first: a waiting row whose ID is already there is marked
//! written instead of being sent again.

use std::collections::HashSet;

use chrono_tz::Tz;
use uuid::Uuid;

use super::{SheetError, authed, range};
use crate::models::Lead;
use crate::state::AppState;

/// The events an export can write, with how the Event column names them.
pub const EVENTS: [(&str, &str); 3] = [("hot_lead", "Hot lead"), ("meeting_booked", "Meeting booked"), ("deal.won", "Deal won")];

/// Written first when the tab is empty. Splee ID is last, so it's out of the way.
pub const HEADER: [&str; 10] = ["Date", "Event", "Name", "Email", "Company", "Campaign", "Status", "Reply", "Link in Splee", "Splee ID"];

/// The last cell of the row for this lead's event, the same on every tab.
pub fn splee_id(lead_id: Uuid, event: &str) -> String {
    format!("{lead_id}:{event}")
}

/// The most rows one append writes. A tab with more waiting gets the rest on the next tick.
const BATCH: i64 = 1_000;

/// How long rows sent without a clear answer wait before the tab is read for them. Google can still be writing them after
/// Splee gave up on the answer, and a read before then would miss them and send them again.
pub const SETTLE_SECS: f64 = 600.0;

/// A reply as long as this is plenty to read in a cell, which takes 50,000 characters.
const MAX_REPLY: usize = 2_000;

fn status_label(status: &str) -> &str {
    match status {
        "new" => "New",
        "queued" => "Queued",
        "contacted" => "Contacted",
        "replied" => "Replied",
        "hot" => "Hot",
        "meeting_booked" => "Meeting booked",
        "not_interested" => "Not interested",
        "bounced" => "Bounced",
        "unsubscribed" => "Unsubscribed",
        "do_not_contact" => "Do not contact",
        other => other,
    }
}

/// Keeps a row for each of the project's export tabs that writes this event. `reply` is the reply that set it off, when the
/// event came with one, and otherwise the lead's latest. Failures are logged, they never fail the caller.
pub async fn queue(state: &AppState, project_id: Uuid, event: &str, lead: &Lead, reply: Option<&str>) {
    let Some((_, label)) = EVENTS.iter().find(|(e, _)| *e == event) else { return };
    if let Err(e) = keep(state, project_id, event, label, lead, reply).await {
        tracing::warn!(error = %e, event, "google sheets: couldn't keep the export row");
    }
}

async fn keep(state: &AppState, project_id: Uuid, event: &str, label: &str, lead: &Lead, reply: Option<&str>) -> sqlx::Result<()> {
    let wanted: bool = sqlx::query_scalar("select exists (select 1 from sheet_connections where project_id = $1 and kind = 'export' and $2 = any(events))")
        .bind(project_id)
        .bind(event)
        .fetch_one(&state.db)
        .await?;
    if !wanted {
        return Ok(());
    }
    // The lead's status now, since the one passed in can be from before the event changed it.
    let (status, campaign, timezone, latest): (String, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "select l.status, s.name, ps.timezone,
           (select body from email_messages m where m.lead_id = l.id and m.direction = 'inbound' order by m.created_at desc limit 1)
         from leads l left join segments s on s.id = l.segment_id left join project_settings ps on ps.project_id = l.project_id
         where l.id = $1",
    )
    .bind(lead.id)
    .fetch_one(&state.db)
    .await?;
    let reply = reply.or(latest.as_deref()).map(crate::mail::inbound::strip_quoted).unwrap_or_default();
    let reply: String = reply.trim().chars().take(MAX_REPLY).collect();
    let tz: Tz = timezone.as_deref().and_then(|t| t.parse().ok()).unwrap_or(chrono_tz::UTC);
    let cells = vec![
        chrono::Utc::now().with_timezone(&tz).format("%Y-%m-%d %H:%M").to_string(),
        label.to_string(),
        lead.full_name.trim().to_string(),
        lead.email.clone().unwrap_or_default(),
        lead.company_name.trim().to_string(),
        campaign.unwrap_or_default(),
        status_label(&status).to_string(),
        reply,
        crate::hooks::lead_url(&state.cfg.public_url, project_id, lead.id),
    ];
    sqlx::query(
        "insert into sheet_export_rows (connection_id, lead_id, event, cells)
         select id, $2, $3, $4 from sheet_connections where project_id = $1 and kind = 'export' and $3 = any(events)
         on conflict (connection_id, lead_id, event) do nothing",
    )
    .bind(project_id)
    .bind(lead.id)
    .bind(event)
    .bind(&cells)
    .execute(&state.db)
    .await
    .map(drop)
}

/// Writes the waiting rows of every export tab whose Google account isn't signed out or held.
pub async fn flush_all(state: &AppState) -> sqlx::Result<()> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select distinct r.connection_id from sheet_export_rows r
           join sheet_connections c on c.id = r.connection_id join sheet_accounts a on a.project_id = c.project_id
         where r.written_at is null and not a.needs_reconnect and (a.held_until is null or a.held_until <= now())",
    )
    .fetch_all(&state.db)
    .await?;
    for id in ids {
        if let Err(e) = flush(state, id).await {
            tracing::info!(connection = %id, error = e.message(), "google sheets: export failed");
        }
    }
    Ok(())
}

/// Writes the tab's waiting rows in one append, with the header first when the tab is empty, and keeps how it went. How many
/// rows it wrote: none when another tick is writing the tab right now.
pub async fn flush(state: &AppState, id: Uuid) -> Result<usize, SheetError> {
    let result = write(state, id).await;
    if !matches!(result, Ok(0)) {
        super::record(&state.db, id, &result).await?;
    }
    result
}

async fn write(state: &AppState, id: Uuid) -> Result<usize, SheetError> {
    // Held until the rows are marked written, so a second tick waits its turn and then finds nothing left.
    let mut tx = state.db.begin().await?;
    let mine: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended('sheet-export:' || $1::text, 0))").bind(id).fetch_one(&mut *tx).await?;
    if !mine {
        return Ok(0);
    }
    let conn: Option<(Uuid, String, String)> = sqlx::query_as("select project_id, spreadsheet_id, tab from sheet_connections where id = $1 and kind = 'export'")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some((project_id, spreadsheet, tab)) = conn else { return Ok(0) };
    let pending: Vec<(Uuid, Uuid, String, Vec<String>)> = sqlx::query_as(
        "select id, lead_id, event, cells from sheet_export_rows
          where connection_id = $1 and written_at is null and (sent_at is null or sent_at < now() - make_interval(secs => $3))
          order by created_at, id limit $2",
    )
    .bind(id)
    .bind(BATCH)
    .bind(SETTLE_SECS)
    .fetch_all(&mut *tx)
    .await?;
    if pending.is_empty() {
        return Ok(0);
    }

    let (whole, all) = (range(&tab, ""), range(&tab, "!A1"));
    let sheet = authed(state, project_id, |c| {
        let (spreadsheet, whole) = (spreadsheet.clone(), whole.clone());
        async move { c.rows(&spreadsheet, &whole).await }
    })
    .await?;
    // Any cell will do, so a column someone moved still counts. An ID can't turn up anywhere by chance.
    let there: HashSet<&str> = sheet.iter().flatten().map(|cell| cell.trim()).collect();
    let (mut found, mut missing) = (Vec::new(), Vec::new());
    for (row, lead_id, event, mut cells) in pending {
        let key = splee_id(lead_id, &event);
        if there.contains(key.as_str()) {
            found.push(row);
        } else {
            cells.push(key);
            missing.push((row, cells));
        }
    }
    mark_written(&mut tx, id, &found).await?;
    if missing.is_empty() {
        tx.commit().await?;
        return Ok(found.len());
    }

    let (ids, mut rows): (Vec<Uuid>, Vec<Vec<String>>) = missing.into_iter().unzip();
    if sheet.first().is_none_or(|first| first.iter().all(|cell| cell.trim().is_empty())) {
        rows.insert(0, HEADER.iter().map(|h| h.to_string()).collect());
    }
    // Noted outside the transaction, so it holds even when the transaction doesn't.
    sqlx::query("update sheet_export_rows set sent_at = now() where id = any($1)").bind(&ids).execute(&state.db).await?;
    let appended = authed(state, project_id, |c| {
        let (spreadsheet, all, rows) = (spreadsheet.clone(), all.clone(), rows.clone());
        async move { c.append(&spreadsheet, &all, &rows).await }
    })
    .await;
    // Whatever the append said, the rows the sheet already had stay marked. The others wait for the next tick's read.
    if let Err(e) = appended {
        // Google turned these down, so nothing was written and they can go again as soon as the account may.
        if matches!(e, SheetError::Limited(_) | SheetError::Unauthorized(_)) {
            sqlx::query("update sheet_export_rows set sent_at = null where id = any($1)").bind(&ids).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        return Err(e);
    }
    mark_written(&mut tx, id, &ids).await?;
    tx.commit().await?;
    Ok(found.len() + ids.len())
}

async fn mark_written(tx: &mut sqlx::PgConnection, id: Uuid, rows: &[Uuid]) -> sqlx::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    sqlx::query("update sheet_export_rows set written_at = now() where id = any($1)").bind(rows).execute(&mut *tx).await?;
    sqlx::query("update sheet_connections set rows_added = rows_added + $2 where id = $1").bind(id).bind(rows.len() as i64).execute(&mut *tx).await?;
    Ok(())
}
