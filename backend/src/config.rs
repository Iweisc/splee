use std::env;

/// The value in backend/.env.example. Keep the two in sync.
const EXAMPLE_APP_SECRET: &str = "dev-only-change-me-0123456789abcdef";

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
    /// False makes sign-up invite-only after the first account, so strangers can't run agents on the host's Claude account.
    pub open_signup: bool,
    /// False turns off agent runs for signed-out visitors: the demo, the company explorer and the support chat.
    pub public_agent: bool,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let cfg = Self {
            database_url: env::var("DATABASE_URL")?,
            app_secret: env::var("APP_SECRET")?,
            public_url: env::var("PUBLIC_URL").unwrap_or_else(|_| "http://localhost:3333".into()),
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".into()),
            agent_model: env::var("AGENT_MODEL").unwrap_or_else(|_| "claude-sonnet-5-5".into()),
            agent_concurrency: env::var("AGENT_CONCURRENCY").ok().and_then(|v| v.parse().ok()).unwrap_or(2),
            sending_enabled: env::var("SENDING_ENABLED").map(|v| v != "false").unwrap_or(true),
            bird_bin: env::var("BIRD_BIN").ok().filter(|v| !v.is_empty()),
            open_signup: env::var("OPEN_SIGNUP").map(|v| v != "false").unwrap_or(true),
            public_agent: env::var("PUBLIC_AGENT").map(|v| v != "false").unwrap_or(true),
        };
        cfg.check_app_secret()?;
        Ok(cfg)
    }

    /// Local dev runs on the example secret, so this only bites on an HTTPS deployment, where the example
    /// (public in the repo) or a short value would let anyone decrypt the saved mailbox passwords.
    fn check_app_secret(&self) -> anyhow::Result<()> {
        let weak = self.app_secret == EXAMPLE_APP_SECRET || self.app_secret.len() < 32;
        anyhow::ensure!(
            !(self.secure_cookies() && weak),
            "APP_SECRET is the example value or shorter than 32 characters. Set a long random one, e.g. `openssl rand -hex 32`."
        );
        Ok(())
    }

    /// The browser only gets the session cookie through PUBLIC_URL, so that decides whether it can be HTTPS-only.
    pub fn secure_cookies(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(public_url: &str, app_secret: &str) -> Config {
        Config {
            database_url: String::new(),
            app_secret: app_secret.into(),
            public_url: public_url.into(),
            bind_addr: String::new(),
            agent_model: String::new(),
            agent_concurrency: 1,
            sending_enabled: false,
            bird_bin: None,
            open_signup: true,
            public_agent: true,
        }
    }

    #[test]
    fn https_deployments_need_a_real_app_secret() {
        let strong = "4f9c2a7e1b3d5f6a8c0e2b4d6f8a1c3e";
        assert!(cfg("https://splee.example.com", EXAMPLE_APP_SECRET).check_app_secret().is_err());
        assert!(cfg("https://splee.example.com", "short").check_app_secret().is_err());
        assert!(cfg("https://splee.example.com", strong).check_app_secret().is_ok());
        assert!(cfg("http://localhost:3333", EXAMPLE_APP_SECRET).check_app_secret().is_ok());
    }
}
