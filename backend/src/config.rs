use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub app_secret: String,
    /// Where the Next.js app is served. Used for links in emails (unsubscribe, invites).
    pub public_url: String,
    pub bind_addr: String,
    pub agent_model: String,
    pub agent_concurrency: usize,
    /// Off for a test backend on a database with live campaigns: approved emails wait instead of going out.
    pub sending_enabled: bool,
    /// Path to the `bird` CLI. When set, research runs can search and read X as the account bird is logged in as.
    pub bird_bin: Option<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            database_url: env::var("DATABASE_URL")?,
            app_secret: env::var("APP_SECRET")?,
            public_url: env::var("PUBLIC_URL").unwrap_or_else(|_| "http://localhost:3333".into()),
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".into()),
            agent_model: env::var("AGENT_MODEL").unwrap_or_else(|_| "claude-sonnet-5-5".into()),
            agent_concurrency: env::var("AGENT_CONCURRENCY").ok().and_then(|v| v.parse().ok()).unwrap_or(2),
            sending_enabled: env::var("SENDING_ENABLED").map(|v| v != "false").unwrap_or(true),
            bird_bin: env::var("BIRD_BIN").ok().filter(|v| !v.is_empty()),
        })
    }
}
