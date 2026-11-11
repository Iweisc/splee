use std::env;

/// The value in backend/.env.example. Keep the two in sync.
const EXAMPLE_APP_SECRET: &str = "dev-only-change-me-0123456789abcdef";

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub app_secret: String,
    /// Where the Next.js app is served. Used for links in emails (unsubscribe, invites).
    pub public_url: String,
    /// Where unsubscribe links point. PUBLIC_URL unless set, for a host whose app isn't reachable from the internet.
    pub unsubscribe_url: String,
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
    /// Porkbun's API. Only tests and sandboxes point it elsewhere, at a fake.
    pub porkbun_api_url: String,
    /// Lowercase emails of the people who run this instance. Only they can load countries into the company database,
    /// which every organization shares.
    pub operator_emails: Vec<String>,
    /// The Google and Microsoft apps people sign in with to connect a mailbox. None hides that provider's button.
    pub google: Option<OAuthApp>,
    pub microsoft: Option<OAuthApp>,
    /// The Splee Slack app. None hides Add to Slack.
    pub slack: Option<SlackApp>,
    /// The Tumblr app projects sign in with to post on Tumblr. None hides Tumblr on the integrations page.
    pub tumblr: Option<OAuthApp>,
    /// An IPinfo token. Website visitors' networks are looked up there first, else through reverse DNS and RDAP.
    pub ipinfo_token: Option<String>,
}

/// The Slack app an organization adds to its workspace. See slack.rs.
#[derive(Clone, Debug)]
pub struct SlackApp {
    pub client_id: String,
    pub client_secret: String,
    /// Checks that a request to /api/slack/interactions came from Slack.
    pub signing_secret: String,
    /// Slack's Web API. Only tests and sandboxes point it elsewhere, at a fake.
    pub api_url: String,
}

/// An OAuth app registered with Google, Microsoft or Tumblr. See mail::oauth and tumblr.
#[derive(Clone, Debug)]
pub struct OAuthApp {
    pub client_id: String,
    pub client_secret: String,
    /// Only tests point these at a fake.
    pub authorize_url: String,
    pub token_url: String,
}

impl OAuthApp {
    fn from_env(prefix: &str, authorize_url: String, token_url: String) -> Option<Self> {
        let var = |name: &str| env::var(format!("{prefix}_{name}")).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        Some(Self { client_id: var("CLIENT_ID")?, client_secret: var("CLIENT_SECRET")?, authorize_url, token_url })
    }
}

impl SlackApp {
    fn from_env() -> Option<Self> {
        let var = |name: &str| env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        Some(Self {
            client_id: var("SLACK_CLIENT_ID")?,
            client_secret: var("SLACK_CLIENT_SECRET")?,
            signing_secret: var("SLACK_SIGNING_SECRET")?,
            api_url: var("SLACK_API_URL").unwrap_or_else(|| crate::slack::API_URL.into()),
        })
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let public_url = env::var("PUBLIC_URL").unwrap_or_else(|_| "http://localhost:3333".into());
        let cfg = Self {
            database_url: env::var("DATABASE_URL")?,
            app_secret: env::var("APP_SECRET")?,
            unsubscribe_url: env::var("UNSUBSCRIBE_URL").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| public_url.clone()),
            public_url,
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".into()),
            agent_model: env::var("AGENT_MODEL").unwrap_or_else(|_| "claude-sonnet-5-5".into()),
            agent_concurrency: env::var("AGENT_CONCURRENCY").ok().and_then(|v| v.parse().ok()).unwrap_or(2),
            sending_enabled: env::var("SENDING_ENABLED").map(|v| v != "false").unwrap_or(true),
            bird_bin: env::var("BIRD_BIN").ok().filter(|v| !v.is_empty()),
            open_signup: env::var("OPEN_SIGNUP").map(|v| v != "false").unwrap_or(true),
            public_agent: env::var("PUBLIC_AGENT").map(|v| v != "false").unwrap_or(true),
            porkbun_api_url: env::var("PORKBUN_API_URL").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| crate::porkbun::API_URL.into()),
            operator_emails: env::var("OPERATOR_EMAILS").map(|v| emails(&v)).unwrap_or_default(),
            google: OAuthApp::from_env("GOOGLE", "https://accounts.google.com/o/oauth2/v2/auth".into(), "https://oauth2.googleapis.com/token".into()),
            microsoft: {
                let tenant = env::var("MICROSOFT_TENANT").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| "common".into());
                let base = format!("https://login.microsoftonline.com/{}/oauth2/v2.0", tenant.trim());
                OAuthApp::from_env("MICROSOFT", format!("{base}/authorize"), format!("{base}/token"))
            },
            slack: SlackApp::from_env(),
            tumblr: OAuthApp::from_env("TUMBLR", crate::tumblr::AUTHORIZE_URL.into(), crate::tumblr::TOKEN_URL.into()),
            ipinfo_token: env::var("IPINFO_TOKEN").ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()),
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

    pub fn is_operator(&self, email: &str) -> bool {
        self.operator_emails.iter().any(|e| e.eq_ignore_ascii_case(email.trim()))
    }

    /// The browser only gets the session cookie through PUBLIC_URL, so that decides whether it can be HTTPS-only.
    pub fn secure_cookies(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}

fn emails(list: &str) -> Vec<String> {
    list.split(',').map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(public_url: &str, app_secret: &str) -> Config {
        Config {
            database_url: String::new(),
            app_secret: app_secret.into(),
            public_url: public_url.into(),
            unsubscribe_url: public_url.into(),
            bind_addr: String::new(),
            agent_model: String::new(),
            agent_concurrency: 1,
            sending_enabled: false,
            bird_bin: None,
            open_signup: true,
            public_agent: true,
            porkbun_api_url: String::new(),
            operator_emails: emails(" Ops@Example.com,, admin@example.org "),
            google: None,
            microsoft: None,
            slack: None,
            tumblr: None,
            ipinfo_token: None,
        }
    }

    #[test]
    fn operators_are_matched_by_email_in_any_case() {
        let cfg = cfg("http://localhost:3333", EXAMPLE_APP_SECRET);
        assert!(cfg.is_operator("ops@example.com"));
        assert!(cfg.is_operator("ADMIN@example.org"));
        assert!(!cfg.is_operator("someone@example.com"));
        assert!(!Config { operator_emails: Vec::new(), ..cfg }.is_operator(""));
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
