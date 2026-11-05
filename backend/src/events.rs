use serde_json::Value;
use uuid::Uuid;

use crate::models::AgentEvent;
use crate::state::AppState;

/// Persists an agent event and pushes it to live SSE listeners.
/// kind: phase | log | tool | done | error (see agent_events check constraint).
pub async fn emit(state: &AppState, run_id: Uuid, step: i32, kind: &str, message: &str, data: Value) -> sqlx::Result<AgentEvent> {
    let event = sqlx::query_as::<_, AgentEvent>(
        "insert into agent_events (run_id, step, kind, message, data) values ($1, $2, $3, $4, $5) returning *",
    )
    .bind(run_id)
    .bind(step)
    .bind(kind)
    .bind(message)
    .bind(data)
    .fetch_one(&state.db)
    .await?;
    // No receivers is normal when nobody has the page open.
    let _ = state.events.send(event.clone());
    Ok(event)
}
