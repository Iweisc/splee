//! The Splee Slack app. An organization adds it to its workspace (routes/slack.rs), each project picks a channel, and Splee
//! posts there with the bot token: a hot reply with the drafted answer, and once a day the emails waiting for approval.
//! The buttons come back to /api/slack/interactions, where routes/slack.rs acts through the same code as the web app.
//! Chat webhooks (hooks.rs) are separate and still get every alert.

use std::time::Duration;

use anyhow::Context;
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::Sha256;
use uuid::Uuid;

use crate::config::SlackApp;
use crate::crypto;
use crate::models::Ts;
use crate::state::AppState;

pub const API_URL: &str = "https://slack.com/api";
pub const AUTHORIZE_URL: &str = "https://slack.com/oauth/v2/authorize";
/// users:read.email maps the person who pressed a button to a Splee member, and Slack only grants it with users:read.
/// Keep in sync with docs/slack-app-manifest.yml.
pub const SCOPES: &str = "chat:write,chat:write.public,channels:read,groups:read,commands,users:read,users:read.email";
/// Slack drops a request whose timestamp is further off than this, so a captured one can't be replayed later.
const MAX_SKEW_SECONDS: i64 = 300;
/// Up to this many emails, the digest has Approve and Skip on each.
pub const DIGEST_EACH_UP_TO: usize = 5;
const DIGEST_CAMPAIGNS: usize = 10;

/// Slack's v0 signature: `v0=` and the hex HMAC-SHA256 of `v0:{timestamp}:{body}` with the signing secret.
/// The comparison is constant-time, and a timestamp more than 5 minutes from `now` fails.
pub fn verify_signature(signing_secret: &str, timestamp: &str, body: &[u8], signature: &str, now: i64) -> bool {
    let Ok(ts) = timestamp.parse::<i64>() else { return false };
    if (now - ts).abs() > MAX_SKEW_SECONDS {
        return false;
    }
    let Some(Ok(sig)) = signature.strip_prefix("v0=").map(hex::decode) else { return false };
    let mut mac = Hmac::<Sha256>::new_from_slice(signing_secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(format!("v0:{timestamp}:").as_bytes());
    mac.update(body);
    mac.verify_slice(&sig).is_ok()
}

pub fn redirect_uri(public_url: &str) -> String {
    format!("{}/api/slack/oauth/callback", public_url.trim_end_matches('/'))
}

/// A Slack error code, said the way the Notifications page and Slack replies show it.
#[derive(Debug)]
pub struct ApiError(pub String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let said = match self.0.as_str() {
            "not_in_channel" => "Splee isn't in this channel. Invite it with /invite @Splee, or pick another channel.",
            "channel_not_found" => "Slack can't find this channel. Pick it again.",
            "is_archived" => "This channel is archived. Pick another one.",
            "invalid_auth" | "token_revoked" | "account_inactive" | "not_authed" => "Slack signed Splee out. Add Splee to Slack again.",
            "missing_scope" => "The Splee app is missing a permission. Add Splee to Slack again.",
            code => return write!(f, "Slack refused it ({code})"),
        };
        f.write_str(said)
    }
}

impl std::error::Error for ApiError {}

pub fn signed_out(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ApiError>().is_some_and(|e| matches!(e.0.as_str(), "invalid_auth" | "token_revoked" | "account_inactive" | "not_authed"))
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("a client with a timeout builds")
}

/// Slack answers 200 with `ok: false` and a code for most failures.
async fn answer(res: reqwest::Response) -> anyhow::Result<Value> {
    let status = res.status();
    let body: Value = res.json().await.with_context(|| format!("Slack answered {status}"))?;
    if body["ok"].as_bool() != Some(true) {
        return Err(ApiError(body["error"].as_str().unwrap_or("unknown_error").to_string()).into());
    }
    Ok(body)
}

/// Slack's Web API as an installed workspace's bot.
pub struct Client {
    api: String,
    token: String,
    http: reqwest::Client,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub private: bool,
}

impl Client {
    pub fn new(api_url: &str, token: &str) -> Self {
        Self { api: api_url.trim_end_matches('/').to_string(), token: token.to_string(), http: http() }
    }

    /// For the workspace an organization added Splee to, with its token decrypted.
    pub fn for_install(state: &AppState, token_enc: &str) -> anyhow::Result<Self> {
        let app = state.cfg.slack.as_ref().context("Slack isn't set up on this Splee")?;
        Ok(Self::new(&app.api_url, &crypto::decrypt(&state.cfg.app_secret, token_enc)?))
    }

    /// Write methods take JSON.
    async fn post(&self, method: &str, body: &Value) -> anyhow::Result<Value> {
        let res = self.http.post(format!("{}/{method}", self.api)).bearer_auth(&self.token).json(body).send().await;
        answer(res.map_err(|e| e.without_url())?).await
    }

    /// Read methods only take a form or a query.
    async fn form(&self, method: &str, form: &[(&str, &str)]) -> anyhow::Result<Value> {
        let res = self.http.post(format!("{}/{method}", self.api)).bearer_auth(&self.token).form(form).send().await;
        answer(res.map_err(|e| e.without_url())?).await
    }

    pub async fn post_message(&self, channel: &str, text: &str, blocks: &Value) -> anyhow::Result<String> {
        let body = self.post("chat.postMessage", &json!({ "channel": channel, "text": text, "blocks": blocks, "unfurl_links": false })).await?;
        Ok(body["ts"].as_str().unwrap_or_default().to_string())
    }

    pub async fn update_message(&self, channel: &str, ts: &str, text: &str, blocks: &Value) -> anyhow::Result<()> {
        self.post("chat.update", &json!({ "channel": channel, "ts": ts, "text": text, "blocks": blocks })).await.map(drop)
    }

    pub async fn open_view(&self, trigger_id: &str, view: &Value) -> anyhow::Result<()> {
        self.post("views.open", &json!({ "trigger_id": trigger_id, "view": view })).await.map(drop)
    }

    /// The address on the person's Slack profile. Slack leaves it out for bots and for guests who hide it.
    pub async fn user_email(&self, user: &str) -> anyhow::Result<Option<String>> {
        let body = self.form("users.info", &[("user", user)]).await?;
        Ok(body["user"]["profile"]["email"].as_str().map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty()))
    }

    /// The Slack user with this address, or None when the workspace has nobody with it.
    pub async fn user_by_email(&self, email: &str) -> anyhow::Result<Option<String>> {
        match self.form("users.lookupByEmail", &[("email", email)]).await {
            Ok(body) => Ok(body["user"]["id"].as_str().map(str::to_string)),
            Err(e) if e.downcast_ref::<ApiError>().is_some_and(|e| e.0 == "users_not_found") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Public channels, and the private ones the bot was invited to, by name. Slack pages them 200 at a time.
    pub async fn channels(&self) -> anyhow::Result<Vec<Channel>> {
        let mut out = vec![];
        let mut cursor = String::new();
        // Five pages covers any workspace a team would scroll a picker for.
        for _ in 0..5 {
            let query = [("types", "public_channel,private_channel"), ("exclude_archived", "true"), ("limit", "200"), ("cursor", cursor.as_str())];
            let body = self.form("conversations.list", &query).await?;
            for c in body["channels"].as_array().into_iter().flatten() {
                let (Some(id), Some(name)) = (c["id"].as_str(), c["name"].as_str()) else { continue };
                out.push(Channel { id: id.into(), name: name.into(), private: c["is_private"].as_bool().unwrap_or(false) });
            }
            cursor = body["response_metadata"]["next_cursor"].as_str().unwrap_or_default().to_string();
            if cursor.is_empty() {
                break;
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub async fn revoke(&self) -> anyhow::Result<()> {
        self.form("auth.revoke", &[]).await.map(drop)
    }
}

/// The bot token and workspace for an authorization code, traded with the PKCE verifier.
pub async fn oauth_access(app: &SlackApp, code: &str, redirect_uri: &str, verifier: &str) -> anyhow::Result<Value> {
    let form = [
        ("client_id", app.client_id.as_str()),
        ("client_secret", app.client_secret.as_str()),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ];
    let res = http().post(format!("{}/oauth.v2.access", app.api_url.trim_end_matches('/'))).form(&form).send().await;
    answer(res.map_err(|e| e.without_url())?).await
}

/// A note only the person who pressed the button sees, through the interaction's response_url.
pub async fn tell(response_url: &str, text: &str) {
    if response_url.is_empty() {
        return;
    }
    let body = json!({ "response_type": "ephemeral", "replace_original": false, "text": text });
    if let Err(e) = http().post(response_url).json(&body).send().await.and_then(|r| r.error_for_status()) {
        tracing::warn!(error = %e.without_url(), "couldn't answer a Slack action");
    }
}

/// Slack reads &, < and > as markup, so text from leads has them escaped.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn cut(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max - 1).collect::<String>().trim_end())
}

/// Escaped and cut, with each line as a Slack quote, in at most `max` characters, since escaping and the quote marks add some.
fn quote(text: &str, max: usize) -> String {
    let mut lines = vec![];
    let mut room = max;
    for line in escape(&cut(text, max)).lines() {
        // The quote mark and the newline.
        if room < 4 {
            break;
        }
        let mut line = line.to_string();
        if line.chars().count() > room - 2 {
            line = cut(&line, room - 2);
            // Cutting may have split an escaped character, which Slack would show as "&am".
            if let Some(amp) = line.rfind('&').filter(|&i| !line[i..].contains(';')) {
                line = format!("{}…", line[..amp].trim_end());
            }
        }
        room -= line.chars().count() + 2;
        lines.push(format!(">{line}"));
    }
    lines.join("\n")
}

fn mrkdwn(text: String) -> Value {
    json!({ "type": "mrkdwn", "text": text })
}

fn plain(text: &str) -> Value {
    json!({ "type": "plain_text", "text": text, "emoji": false })
}

fn section(text: String) -> Value {
    json!({ "type": "section", "text": mrkdwn(text) })
}

fn context(text: String) -> Value {
    json!({ "type": "context", "elements": [mrkdwn(text)] })
}

fn button(action_id: &str, text: &str, value: impl ToString) -> Value {
    json!({ "type": "button", "action_id": action_id, "text": plain(text), "value": value.to_string() })
}

fn link_button(action_id: &str, text: &str, url: &str) -> Value {
    json!({ "type": "button", "action_id": action_id, "text": plain(text), "url": url })
}

fn primary(mut button: Value) -> Value {
    button["style"] = json!("primary");
    button
}

/// A hot reply as its Slack message shows it, built again from the database after every action.
pub struct HotReply {
    pub lead_id: Uuid,
    pub lead_name: String,
    pub company: String,
    pub project_name: String,
    pub reply: String,
    /// The draft waiting for approval, or what went out once someone answered.
    pub answer: Option<String>,
    pub draft_id: Option<Uuid>,
    pub answered: bool,
    /// Who did what, under the buttons.
    pub note: Option<String>,
    pub link: String,
    /// The lead owner's Slack user, mentioned so Slack tells them.
    pub owner: Option<String>,
}

impl HotReply {
    fn who(&self) -> String {
        if self.company.trim().is_empty() { self.lead_name.clone() } else { format!("{} ({})", self.lead_name, self.company) }
    }

    /// " for <@U…>" when the lead has an owner on Slack.
    fn for_owner(&self) -> String {
        self.owner.as_ref().map(|id| format!(" for <@{id}>")).unwrap_or_default()
    }

    /// Notifications and screen readers get this instead of the blocks.
    pub fn text(&self) -> String {
        format!("Hot reply: {} replied to {}{}", self.who(), self.project_name, self.for_owner())
    }

    pub fn blocks(&self) -> Value {
        let headline = format!("*Hot reply:* {} replied to {}{}", escape(&self.who()), escape(&self.project_name), self.for_owner());
        let mut blocks = vec![section(headline), section(quote(&self.reply, 2500))];
        match (&self.answer, self.answered) {
            (Some(answer), false) => blocks.push(section(format!("*Drafted answer*\n{}", quote(answer, 2500)))),
            (Some(answer), true) => blocks.push(section(format!("*Reply sent*\n{}", quote(answer, 2500)))),
            (None, false) => blocks.push(context("Splee didn't draft an answer. Use Write a reply to answer from here.".into())),
            (None, true) => {}
        }
        if let Some(note) = &self.note {
            blocks.push(context(escape(note)));
        }
        let open = link_button("open", "Open in Splee", &self.link);
        let actions = match (self.answered, self.draft_id) {
            (true, _) => vec![open],
            (false, Some(draft)) => vec![
                primary(button("send_reply", "Send reply", draft)),
                button("edit_reply", "Edit", self.lead_id),
                button("snooze", "Snooze 1 day", self.lead_id),
                open,
            ],
            (false, None) => vec![button("edit_reply", "Write a reply", self.lead_id), button("snooze", "Snooze 1 day", self.lead_id), open],
        };
        blocks.push(json!({ "type": "actions", "block_id": "hot_reply", "elements": actions }));
        Value::Array(blocks)
    }
}

/// The modal Edit opens, with the draft to change or an empty box. `metadata` comes back with the submission.
pub fn reply_modal(lead_name: &str, reply: &str, draft: Option<&str>, metadata: &Value) -> Value {
    let mut input = json!({ "type": "plain_text_input", "action_id": "body", "multiline": true, "max_length": 3000 });
    if let Some(draft) = draft {
        input["initial_value"] = json!(cut(draft, 3000));
    }
    json!({
        "type": "modal",
        "callback_id": "edit_reply",
        // Slack cuts a modal's title at 24 characters.
        "title": plain(if draft.is_some() { "Edit the reply" } else { "Write a reply" }),
        "submit": plain("Send"),
        "close": plain("Cancel"),
        "private_metadata": metadata.to_string(),
        "blocks": [
            context(format!("*{}* wrote:\n{}", escape(lead_name), quote(reply, 1500))),
            { "type": "input", "block_id": "reply", "label": plain("Your reply"), "element": input },
        ],
    })
}

pub struct DigestEmail {
    pub id: Uuid,
    pub lead_name: String,
    pub company: String,
    pub subject: String,
    pub body: String,
}

pub struct DigestCampaign {
    pub id: Uuid,
    pub name: String,
    pub count: i64,
}

/// The emails a project has waiting for approval. `before` is when it was counted, so Approve all only approves what it said.
pub struct Digest {
    pub project_name: String,
    pub total: i64,
    pub campaigns: Vec<DigestCampaign>,
    /// Filled when there are few enough to approve one by one.
    pub emails: Vec<DigestEmail>,
    pub before: Ts,
    pub link: String,
}

fn emails(n: i64) -> String {
    if n == 1 { "1 email".into() } else { format!("{n} emails") }
}

impl Digest {
    pub fn text(&self) -> String {
        format!("{} waiting for approval in {}", emails(self.total), self.project_name)
    }

    pub fn blocks(&self) -> Value {
        let verb = if self.total == 1 { "is" } else { "are" };
        let mut blocks = vec![section(format!("*{} {verb} waiting for approval* in {}", emails(self.total), escape(&self.project_name)))];
        for c in self.campaigns.iter().take(DIGEST_CAMPAIGNS) {
            let value = json!({ "segment": c.id, "before": self.before });
            let label = if c.count == 1 { "Approve 1".to_string() } else { format!("Approve all {}", c.count) };
            let mut approve = button("approve_campaign", &label, value);
            approve["confirm"] = json!({
                "title": plain(&format!("Approve {}?", emails(c.count))),
                "text": plain(&cut(&format!("They go out from your mailboxes on the {} campaign's schedule.", c.name), 300)),
                "confirm": plain("Approve"),
                "deny": plain("Cancel"),
            });
            let mut block = section(format!("*{}*\n{} waiting", escape(&cut(&c.name, 200)), emails(c.count)));
            block["block_id"] = json!(format!("campaign:{}", c.id));
            block["accessory"] = approve;
            blocks.push(block);
        }
        if self.campaigns.len() > DIGEST_CAMPAIGNS {
            blocks.push(context(format!("{} more campaigns have emails waiting in Splee.", self.campaigns.len() - DIGEST_CAMPAIGNS)));
        }
        if !self.emails.is_empty() {
            blocks.push(json!({ "type": "divider" }));
        }
        for e in &self.emails {
            let who = if e.company.trim().is_empty() { escape(&e.lead_name) } else { format!("{} at {}", escape(&e.lead_name), escape(&e.company)) };
            blocks.push(section(format!("*{who}*\n{}\n{}", escape(&cut(&e.subject, 150)), quote(&e.body, 400))));
            let actions = [primary(button("approve_email", "Approve", e.id)), button("skip_email", "Skip", e.id)];
            blocks.push(json!({ "type": "actions", "block_id": format!("email:{}", e.id), "elements": actions }));
        }
        blocks.push(json!({ "type": "actions", "block_id": "review", "elements": [link_button("review", "Review in Splee", &self.link)] }));
        Value::Array(blocks)
    }
}

/// The digest's blocks with one campaign's or email's buttons swapped for what happened to it, so nobody presses them again.
pub fn mark_done(blocks: &Value, block_id: &str, note: &str) -> Value {
    let marked = blocks.as_array().into_iter().flatten().flat_map(|b| {
        if b["block_id"].as_str() != Some(block_id) {
            return vec![b.clone()];
        }
        let done = context(escape(note));
        if b["type"] == "section" {
            let mut plain = b.clone();
            if let Some(o) = plain.as_object_mut() {
                o.remove("accessory");
                o.remove("block_id");
            }
            vec![plain, done]
        } else {
            vec![done]
        }
    });
    Value::Array(marked.collect())
}

/// The emails a digest counts and Approve all approves: first emails and follow-ups the writer drafted, in project $1.
pub const WAITING: &str = "e.project_id = $1 and e.direction = 'outbound' and e.kind in ('initial', 'follow_up') and e.status = 'draft'";

/// The hot reply message for a lead's latest reply, as it stands now: the draft if one waits, or the answer that went out
/// after it and who sent it. Also returns the reply's id. `note` shows while nobody has answered.
pub async fn load_hot_reply(db: &sqlx::PgPool, public_url: &str, lead_id: Uuid, note: Option<String>) -> sqlx::Result<Option<(HotReply, Uuid)>> {
    let row: Option<(Uuid, String, String, String, Uuid, String, Ts)> = sqlx::query_as(
        "select l.project_id, l.full_name, l.company_name, p.name, i.id, i.body, i.created_at from leads l
         join projects p on p.id = l.project_id
         cross join lateral (select id, body, created_at from email_messages where lead_id = l.id and direction = 'inbound' order by created_at desc limit 1) i
         where l.id = $1",
    )
    .bind(lead_id)
    .fetch_optional(db)
    .await?;
    let Some((project_id, lead_name, company, project_name, reply_id, reply, replied_at)) = row else { return Ok(None) };
    // What owner_slack_user cached, so rebuilding the message after a press calls nobody.
    let owner: Option<String> = sqlx::query_scalar(
        "select c.slack_user_id from leads l join projects p on p.id = l.project_id join slack_installs si on si.org_id = p.org_id
         join slack_user_ids c on c.team_id = si.team_id and c.user_id = l.owner_id where l.id = $1",
    )
    .bind(lead_id)
    .fetch_optional(db)
    .await?
    .flatten();
    let answer: Option<(Uuid, String)> = sqlx::query_as(
        "select id, body from email_messages where lead_id = $1 and direction = 'outbound' and kind in ('manual_reply', 'auto_reply')
           and status not in ('draft', 'cancelled') and created_at >= $2
         order by created_at desc limit 1",
    )
    .bind(lead_id)
    .bind(replied_at)
    .fetch_optional(db)
    .await?;
    let draft: Option<(Uuid, String)> = match answer {
        Some(_) => None,
        None => {
            sqlx::query_as("select id, body from email_messages where lead_id = $1 and kind = 'auto_reply' and status = 'draft' order by created_at desc limit 1")
                .bind(lead_id)
                .fetch_optional(db)
                .await?
        }
    };
    let note = match &answer {
        Some((sent_id, _)) => {
            // A reply written in Slack is claimed on the lead's message, a sent draft on itself.
            let by: Option<Option<String>> = sqlx::query_scalar(
                "select u.name from slack_actions a left join users u on u.id = a.user_id where a.target_id = any($1) and a.action = 'send_reply'",
            )
            .bind([*sent_id, reply_id])
            .fetch_optional(db)
            .await?;
            Some(match by {
                Some(name) => format!("{} sent this reply from Slack.", name.unwrap_or_else(|| "Someone".into())),
                None => "Answered in Splee.".into(),
            })
        }
        None => note,
    };
    let hot = HotReply {
        lead_id,
        lead_name,
        company,
        project_name,
        reply,
        answered: answer.is_some(),
        answer: answer.as_ref().or(draft.as_ref()).map(|(_, body)| body.clone()),
        draft_id: draft.map(|(id, _)| id),
        note,
        link: crate::hooks::lead_url(public_url, project_id, lead_id),
        owner,
    };
    Ok(Some((hot, reply_id)))
}

/// Looks up the lead owner's Slack user by their Splee email, once per person and workspace. Nobody found is kept for a day,
/// so someone who joins the workspace later is found then. A changed email is looked up again.
pub async fn owner_slack_user(db: &sqlx::PgPool, client: &Client, team_id: &str, lead_id: Uuid) -> anyhow::Result<Option<String>> {
    let owner: Option<(Uuid, String)> =
        sqlx::query_as("select u.id, u.email::text from leads l join users u on u.id = l.owner_id where l.id = $1").bind(lead_id).fetch_optional(db).await?;
    let Some((user_id, email)) = owner else { return Ok(None) };
    let cached: Option<Option<String>> = sqlx::query_scalar(
        "select slack_user_id from slack_user_ids where team_id = $1 and user_id = $2 and email = $3::citext
           and (slack_user_id is not null or looked_up_at > now() - interval '1 day')",
    )
    .bind(team_id)
    .bind(user_id)
    .bind(&email)
    .fetch_optional(db)
    .await?;
    if let Some(cached) = cached {
        return Ok(cached);
    }
    let found = client.user_by_email(&email).await?;
    sqlx::query(
        "insert into slack_user_ids (team_id, user_id, email, slack_user_id) values ($1, $2, $3, $4)
         on conflict (team_id, user_id) do update set email = excluded.email, slack_user_id = excluded.slack_user_id, looked_up_at = now()",
    )
    .bind(team_id)
    .bind(user_id)
    .bind(&email)
    .bind(&found)
    .execute(db)
    .await?;
    Ok(found)
}

/// Saves how a post to a project's channel went, for the Notifications page. Matching on the channel keeps a slow post to
/// the old channel from marking one picked since.
async fn record(db: &sqlx::PgPool, project_id: Uuid, channel: &str, result: &anyhow::Result<()>) {
    let error = result.as_ref().err().map(|e| format!("{e:#}").chars().take(500).collect::<String>());
    if let Err(e) = &result {
        tracing::warn!(error = %e, "Slack post failed");
    }
    let saved = sqlx::query(
        "update slack_channels set error = $3, failed_at = case when $3::text is null then null else now() end where project_id = $1 and channel_id = $2",
    )
    .bind(project_id)
    .bind(channel)
    .bind(error)
    .execute(db)
    .await;
    if let Err(e) = saved {
        tracing::warn!(error = %e, "couldn't save the Slack post outcome");
    }
}

/// Posts a hot reply to the lead's project's channel, when its organization added Splee to Slack and picked one.
/// Failures are logged and shown on the Notifications page, never passed on.
pub async fn post_hot_reply(state: &AppState, lead_id: Uuid) {
    if state.cfg.slack.is_none() {
        return;
    }
    let target: sqlx::Result<Option<(Uuid, String, String, String)>> = sqlx::query_as(
        "select c.project_id, c.channel_id, i.bot_token_enc, i.team_id from leads l join slack_channels c on c.project_id = l.project_id
         join projects p on p.id = l.project_id join slack_installs i on i.org_id = p.org_id where l.id = $1",
    )
    .bind(lead_id)
    .fetch_optional(&state.db)
    .await;
    let (project_id, channel, token_enc, team_id) = match target {
        Ok(Some(t)) => t,
        Ok(None) => return,
        Err(e) => return tracing::warn!(error = %e, "couldn't load the Slack channel"),
    };
    let result = async {
        let client = Client::for_install(state, &token_enc)?;
        // A failed lookup only costs the mention, so the post still goes out.
        if let Err(e) = owner_slack_user(&state.db, &client, &team_id, lead_id).await {
            tracing::warn!(error = %e, "couldn't find the lead owner on Slack");
        }
        let (hot, _) = load_hot_reply(&state.db, &state.cfg.public_url, lead_id, None).await?.context("the lead has no reply")?;
        client.post_message(&channel, &hot.text(), &hot.blocks()).await.map(drop)
    }
    .await;
    record(&state.db, project_id, &channel, &result).await;
}

pub async fn load_digest(db: &sqlx::PgPool, public_url: &str, project_id: Uuid) -> sqlx::Result<Option<Digest>> {
    let before = chrono::Utc::now();
    let (project_name, total): (String, i64) = sqlx::query_as(&format!(
        "select p.name, (select count(*) from email_messages e where {WAITING} and e.created_at <= $2) from projects p where p.id = $1"
    ))
    .bind(project_id)
    .bind(before)
    .fetch_one(db)
    .await?;
    if total == 0 {
        return Ok(None);
    }
    let campaigns: Vec<(Uuid, String, i64)> = sqlx::query_as(&format!(
        "select s.id, s.name, count(*) from email_messages e join segments s on s.id = e.segment_id where {WAITING} and e.created_at <= $2
         group by s.id, s.name order by count(*) desc, s.name"
    ))
    .bind(project_id)
    .bind(before)
    .fetch_all(db)
    .await?;
    let emails: Vec<(Uuid, String, String, String, String)> = if total as usize <= DIGEST_EACH_UP_TO {
        sqlx::query_as(&format!(
            "select e.id, l.full_name, l.company_name, e.subject, e.body from email_messages e join leads l on l.id = e.lead_id
             where {WAITING} and e.created_at <= $2 order by e.created_at"
        ))
        .bind(project_id)
        .bind(before)
        .fetch_all(db)
        .await?
    } else {
        vec![]
    };
    Ok(Some(Digest {
        project_name,
        total,
        campaigns: campaigns.into_iter().map(|(id, name, count)| DigestCampaign { id, name, count }).collect(),
        emails: emails.into_iter().map(|(id, lead_name, company, subject, body)| DigestEmail { id, lead_name, company, subject, body }).collect(),
        before,
        link: format!("{}/app/p/{project_id}/emails", public_url.trim_end_matches('/')),
    }))
}

/// Marks today's digest as posted for every project with a channel and emails waiting, and returns them. Claiming first
/// keeps a second check in the digest hour from posting it twice.
async fn claim_digests(db: &sqlx::PgPool) -> sqlx::Result<Vec<(Uuid, String, String)>> {
    sqlx::query_as(&format!(
        "update slack_channels c set digest_on = current_date from projects p join slack_installs i on i.org_id = p.org_id
         where p.id = c.project_id and c.digest_on is distinct from current_date
           and exists (select 1 from email_messages e where {})
         returning c.project_id, c.channel_id, i.bot_token_enc",
        WAITING.replace("$1", "c.project_id")
    ))
    .fetch_all(db)
    .await
}

/// Once a day, with the other digests: each project's emails waiting for approval, in its channel.
pub async fn post_digests(state: &AppState) -> anyhow::Result<()> {
    if state.cfg.slack.is_none() {
        return Ok(());
    }
    for (project_id, channel, token_enc) in claim_digests(&state.db).await? {
        let result = async {
            let Some(digest) = load_digest(&state.db, &state.cfg.public_url, project_id).await? else { return Ok(()) };
            Client::for_install(state, &token_enc)?.post_message(&channel, &digest.text(), &digest.blocks()).await.map(drop)
        }
        .await;
        record(&state.db, project_id, &channel, &result).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, ts: &str, body: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("v0:{ts}:{body}").as_bytes());
        format!("v0={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn only_a_fresh_request_signed_with_the_secret_passes() {
        // Slack's own example from "Verifying requests from Slack".
        let body = "token=xyzz0WbapA4vBCDEFasx0q6G&team_id=T1DC2JH3J&team_domain=testteamnow&channel_id=G8PSS9T3V&channel_name=foobar&user_id=U2CERLKJA&user_name=roadrunner&command=%2Fwebhook-collect&text=&response_url=https%3A%2F%2Fhooks.slack.com%2Fcommands%2FT1DC2JH3J%2F397700885554%2F96rGlfmibIGlgcZRskXaIFfN&trigger_id=398738663015.47445629121.803a0bc887a14d10d2c447fce8b6703c";
        let good = "v0=a2114d57b48eac39b9ad189dd8316235a7b4a8d21a10bd27519666489c69b503";
        let secret = "8f742231b10e8888abcd99yyyzzz85a5";
        assert!(verify_signature(secret, "1531420618", body.as_bytes(), good, 1531420618 + 60));
        assert!(!verify_signature(secret, "1531420618", format!("{body}x").as_bytes(), good, 1531420618), "a changed body");
        assert!(!verify_signature("another secret", "1531420618", body.as_bytes(), good, 1531420618));
        assert!(!verify_signature(secret, "1531420618", body.as_bytes(), &good.replace("v0=", "v1="), 1531420618));
        assert!(!verify_signature(secret, "1531420618", body.as_bytes(), "v0=not-hex", 1531420618));
        assert!(!verify_signature(secret, "1531420618", body.as_bytes(), "", 1531420618));

        // Five minutes either way, and not a second more.
        let ts = "1700000000";
        let sig = sign(secret, ts, "payload=x");
        assert!(verify_signature(secret, ts, b"payload=x", &sig, 1700000000 + 300));
        assert!(verify_signature(secret, ts, b"payload=x", &sig, 1700000000 - 300));
        assert!(!verify_signature(secret, ts, b"payload=x", &sig, 1700000000 + 301), "stale");
        assert!(!verify_signature(secret, ts, b"payload=x", &sig, 1700000000 - 301), "from the future");
        assert!(!verify_signature(secret, "soon", b"payload=x", &sig, 1700000000));
    }

    fn hot(draft: Option<Uuid>, answered: bool) -> HotReply {
        HotReply {
            lead_id: Uuid::nil(),
            lead_name: "Sarah Lee".into(),
            company: "Acme & Co".into(),
            project_name: "Fieldnote".into(),
            reply: "Sounds good.\nCan we talk <Tuesday>?".into(),
            answer: draft.map(|_| "Hi Sarah, Tuesday works.".into()),
            draft_id: draft,
            answered,
            note: None,
            link: "https://splee.test/app/p/1/inbox?lead=2".into(),
            owner: None,
        }
    }

    fn action_ids(blocks: &Value) -> Vec<String> {
        let actions = blocks.as_array().unwrap().iter().find(|b| b["type"] == "actions").unwrap();
        actions["elements"].as_array().unwrap().iter().map(|e| e["action_id"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn a_hot_reply_quotes_them_and_offers_the_draft() {
        let draft = Uuid::new_v4();
        let h = hot(Some(draft), false);
        let blocks = h.blocks();
        assert_eq!(h.text(), "Hot reply: Sarah Lee (Acme & Co) replied to Fieldnote");
        assert_eq!(blocks[0]["text"]["text"], "*Hot reply:* Sarah Lee (Acme &amp; Co) replied to Fieldnote");
        assert_eq!(blocks[1]["text"]["text"], ">Sounds good.\n>Can we talk &lt;Tuesday&gt;?");
        assert_eq!(blocks[2]["text"]["text"], "*Drafted answer*\n>Hi Sarah, Tuesday works.");
        assert_eq!(action_ids(&blocks), ["send_reply", "edit_reply", "snooze", "open"]);
        let actions = &blocks[3]["elements"];
        assert_eq!((actions[0]["value"].as_str(), actions[0]["style"].as_str()), (Some(draft.to_string().as_str()), Some("primary")));
        assert_eq!(actions[1]["value"], Uuid::nil().to_string());
        assert_eq!(actions[2]["text"]["text"], "Snooze 1 day");
        assert_eq!(actions[3]["url"], "https://splee.test/app/p/1/inbox?lead=2");

        let none = hot(None, false).blocks();
        assert_eq!(action_ids(&none), ["edit_reply", "snooze", "open"]);
        assert_eq!(none[2]["type"], "context");

        // Once someone answered, only the link is left, under what went out and who sent it.
        let mut sent = hot(Some(draft), true);
        sent.note = Some("Ana Silva sent this reply from Slack.".into());
        let blocks = sent.blocks();
        assert_eq!(blocks[2]["text"]["text"], "*Reply sent*\n>Hi Sarah, Tuesday works.");
        assert_eq!(blocks[3]["elements"][0]["text"], "Ana Silva sent this reply from Slack.");
        assert_eq!(action_ids(&blocks), ["open"]);
    }

    #[test]
    fn long_replies_are_cut_to_fit_a_section() {
        let mut h = hot(None, false);
        h.reply = "word ".repeat(2000);
        let text = h.blocks()[1]["text"]["text"].as_str().unwrap().to_string();
        assert!(text.chars().count() <= 3000 && text.ends_with('…'), "{}", text.len());
    }

    #[test]
    fn the_modal_carries_the_draft_and_where_it_came_from() {
        let meta = json!({ "lead": Uuid::nil(), "channel": "C1", "ts": "1.2" });
        let view = reply_modal("Sarah", "Tuesday?", Some("Hi Sarah"), &meta);
        assert_eq!((view["callback_id"].as_str(), view["title"]["text"].as_str()), (Some("edit_reply"), Some("Edit the reply")));
        assert_eq!(view["blocks"][1]["block_id"], "reply");
        assert_eq!(view["blocks"][1]["element"]["action_id"], "body");
        assert_eq!(view["blocks"][1]["element"]["initial_value"], "Hi Sarah");
        assert_eq!(serde_json::from_str::<Value>(view["private_metadata"].as_str().unwrap()).unwrap(), meta);
        let empty = reply_modal("Sarah", "Tuesday?", None, &meta);
        assert!(empty["blocks"][1]["element"].get("initial_value").is_none());
        assert_eq!(empty["title"]["text"], "Write a reply");
    }

    fn digest(total: i64, each: usize) -> Digest {
        let campaign = DigestCampaign { id: Uuid::new_v4(), name: "Founders in Lisbon".into(), count: total };
        let emails = (0..each)
            .map(|i| DigestEmail {
                id: Uuid::new_v4(),
                lead_name: format!("Lead {i}"),
                company: "Acme".into(),
                subject: "Quick idea".into(),
                body: "Hi there".into(),
            })
            .collect();
        Digest {
            project_name: "Fieldnote".into(),
            total,
            campaigns: vec![campaign],
            emails,
            before: "2026-11-11T08:00:00Z".parse().unwrap(),
            link: "https://splee.test/app/p/1/emails".into(),
        }
    }

    #[test]
    fn the_digest_offers_approve_all_and_one_by_one_for_a_few() {
        let d = digest(2, 2);
        let blocks = d.blocks();
        assert_eq!(d.text(), "2 emails waiting for approval in Fieldnote");
        assert_eq!(blocks[0]["text"]["text"], "*2 emails are waiting for approval* in Fieldnote");
        let campaign = &blocks[1];
        assert_eq!(campaign["block_id"], format!("campaign:{}", d.campaigns[0].id));
        assert_eq!(campaign["accessory"]["action_id"], "approve_campaign");
        assert_eq!(campaign["accessory"]["text"]["text"], "Approve all 2");
        assert_eq!(campaign["accessory"]["confirm"]["title"]["text"], "Approve 2 emails?");
        let value: Value = serde_json::from_str(campaign["accessory"]["value"].as_str().unwrap()).unwrap();
        assert_eq!(value, json!({ "segment": d.campaigns[0].id, "before": "2026-11-11T08:00:00Z" }));
        assert_eq!(blocks[2]["type"], "divider");
        assert_eq!(blocks[3]["text"]["text"], "*Lead 0 at Acme*\nQuick idea\n>Hi there");
        assert_eq!(blocks[4]["block_id"], format!("email:{}", d.emails[0].id));
        assert_eq!(action_ids(&json!([blocks[4]])), ["approve_email", "skip_email"]);
        assert_eq!(blocks.as_array().unwrap().last().unwrap()["elements"][0]["url"], "https://splee.test/app/p/1/emails");

        // Over five, only the campaigns and the link.
        let many = digest(40, 0).blocks();
        assert_eq!(many.as_array().unwrap().len(), 3);
        let one = digest(1, 1).blocks();
        assert_eq!(one[0]["text"]["text"], "*1 email is waiting for approval* in Fieldnote");
        assert_eq!(one[1]["accessory"]["text"]["text"], "Approve 1");
    }

    #[test]
    fn a_done_email_or_campaign_loses_its_buttons() {
        let d = digest(2, 2);
        let blocks = d.blocks();
        let email = format!("email:{}", d.emails[0].id);
        let marked = mark_done(&blocks, &email, "Approved by Ana <Silva>");
        assert_eq!(marked[4], json!({ "type": "context", "elements": [{ "type": "mrkdwn", "text": "Approved by Ana &lt;Silva&gt;" }] }));
        assert_eq!(marked[6]["block_id"], format!("email:{}", d.emails[1].id), "the other email keeps its buttons");

        let marked = mark_done(&blocks, &format!("campaign:{}", d.campaigns[0].id), "Ana approved 2 emails");
        assert!(marked[1].get("accessory").is_none());
        assert_eq!(marked[1]["text"], blocks[1]["text"]);
        assert_eq!(marked[2]["elements"][0]["text"], "Ana approved 2 emails");
        assert_eq!(marked.as_array().unwrap().len(), blocks.as_array().unwrap().len() + 1);
    }
}
