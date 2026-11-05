use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::broadcast;

use crate::config::Config;
use crate::models::AgentEvent;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub cfg: Arc<Config>,
    /// Every agent event is persisted first, then broadcast here so SSE handlers can push it live.
    pub events: broadcast::Sender<AgentEvent>,
    pub http: reqwest::Client,
}
