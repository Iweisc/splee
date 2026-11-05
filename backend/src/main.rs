mod agent;
mod auth;
mod billing;
mod config;
mod crypto;
mod error;
mod events;
mod mail;
mod models;
mod routes;
mod state;

use std::sync::Arc;

use axum::Router;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::broadcast;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let cfg = Config::from_env()?;
    let db = PgPoolOptions::new().max_connections(20).connect(&cfg.database_url).await?;
    sqlx::migrate!().run(&db).await?;

    let (events, _) = broadcast::channel(1024);
    let state = AppState {
        db,
        cfg: Arc::new(cfg.clone()),
        events,
        http: reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build()?,
    };

    agent::spawn_workers(state.clone());
    mail::spawn_workers(state.clone());

    let app = Router::new()
        .nest("/api", routes::router())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!("listening on {}", cfg.bind_addr);
    // ConnectInfo gives public routes the peer address for rate limiting.
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;
    Ok(())
}
