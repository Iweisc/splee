mod ab_test;
mod agent;
mod airtable;
mod auth;
mod autopilot;
mod baserow;
mod bluesky;
mod config;
mod crm;
mod crypto;
mod dev;
mod discourse;
mod domains;
mod directory;
mod directory_queue;
mod error;
mod events;
mod fingerprints;
mod grist;
mod hashnode;
mod hiring;
mod hooks;
mod jobs;
mod lemmy;
mod mail;
mod mastodon;
mod models;
mod nocodb;
mod nostr;
mod notion;
mod porkbun;
mod reddit;
mod routes;
mod slack;
mod social;
mod stackexchange;
mod state;
mod teable;
mod tech;
mod threads;
mod tumblr;
mod unipile;
mod verify;
mod waterfall;
mod youtube;

use std::sync::Arc;

use axum::Router;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::broadcast;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::state::AppState;

enum Command {
    ResetPassword(String),
    ImportCompanies(Vec<String>),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match args.as_slice() {
        [] => None,
        [cmd, email] if cmd == "reset-password" => Some(Command::ResetPassword(email.clone())),
        [cmd, countries @ ..] if cmd == "import-companies" => Some(Command::ImportCompanies(countries.to_vec())),
        _ => anyhow::bail!("usage: splee [reset-password <email> | import-companies [COUNTRY…]]"),
    };

    let cfg = Config::from_env()?;
    let db = PgPoolOptions::new().max_connections(20).connect(&cfg.database_url).await?;
    sqlx::migrate!().run(&db).await?;

    // These run once and exit, without the API or workers, so they're safe next to a running server.
    match command {
        Some(Command::ResetPassword(email)) => {
            let Some(password) = auth::reset_password(&db, &email).await? else {
                anyhow::bail!("no user with email {email}");
            };
            println!("New password for {email}: {password}");
            println!("They're signed out everywhere and can change it under Account after they sign in.");
            return Ok(());
        }
        Some(Command::ImportCompanies(countries)) => return directory::import(&db, directory::CATALOG, &countries, None).await.map(drop),
        None => {}
    }

    let (events, _) = broadcast::channel(1024);
    let state = AppState {
        db,
        cfg: Arc::new(cfg.clone()),
        events,
    };

    if let Err(e) = domains::recover(&state.db).await {
        tracing::error!(error = ?e, "could not settle domain purchases a restart interrupted");
    }
    agent::spawn_workers(state.clone());
    mail::spawn_workers(state.clone());
    directory_queue::spawn_worker(state.db.clone());
    hiring::spawn_worker(state.db.clone());
    tech::spawn_worker(state.db.clone());
    waterfall::spawn_worker(state.clone());

    let app = Router::new()
        .nest("/api", routes::router())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!("listening on {}", cfg.bind_addr);
    // ConnectInfo gives public routes the peer address for rate limiting.
    let server = axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>());
    // Without this, SIGTERM ends the process on the spot and the `claude` processes of running agent calls live on,
    // still spending. Returning drops the runtime's tasks, and the vendored SDK spawns the CLI with kill_on_drop.
    // Not axum's graceful shutdown: it would wait for the SSE streams, which never end on their own.
    tokio::select! {
        served = server => served?,
        () = stop_signal() => tracing::info!("stopping"),
    }
    Ok(())
}

async fn stop_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler installs");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
