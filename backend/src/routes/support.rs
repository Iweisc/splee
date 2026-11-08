use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::demo::client_ip;
use crate::agent::sdk::{self, Call};
use crate::auth::MaybeAuth;
use crate::error::{AppError, AppResult};
use crate::models::SupportMessage;
use crate::state::AppState;

const MAX_CONTENT_CHARS: usize = 2000;
const MAX_CONVERSATION_MESSAGES: i64 = 30;
const ANSWERS_PER_HOUR_PER_IP: i64 = 40;

const SUPPORT_PROMPT: &str = "You are the support assistant on the Splee website. Answer visitors' questions about Splee in 1 to 4 short \
sentences, plain text, friendly and direct. Only use the facts below. If something isn't covered, say you don't know and that the \
team can help by email. Never make up features, prices, customers, statistics or results. Splee is new and has no customer numbers to quote.

Facts about Splee:
- Splee is an AI outbound agent. You paste your website and the agent researches your company, finds your competitors, defines buyer \
segments (campaigns), finds matching companies and decision makers on the public web, writes each one a personal cold email, sends them \
from your own mailboxes within a daily email limit, reads replies, sorts them (hot, question, not interested, out of office, unsubscribe) and \
can answer hot leads with your booking link, automatically or after your approval.
- The free demo on the homepage shows the research, competitors, campaigns, a sample of companies, people and written emails for any website, \
without signing up.
- Leads come from live web research by the agent (company websites, directories, public profiles, press). Splee has no proprietary \
contact database. Emails are taken from the web when published, otherwise guessed from the company's email pattern and marked as guessed. \
Domains without a mail server are dropped.
- Emails are sent from your own mailboxes that you connect with SMTP and IMAP (Google Workspace, Microsoft 365 or any provider with app \
passwords). Replies are read over IMAP. Splee checks your domain's MX, SPF and DMARC records. Sending from a separate domain protects your main domain's reputation.
- Price: Splee is free and open source (AGPL-3.0). People run it themselves, on their own machine or server, with their own Claude \
account for research and writing and their own mailboxes for sending. There are no credits, plans or per-email fees.
- You can set targeting (countries, company sizes, keywords to exclude), a send schedule with timezone, days and hours, follow-ups \
(by default two, after 3 and 5 days), tone, language, length, call to action, signature, booking link and materials such as case \
studies or FAQs the agent can use.
- Every email has an unsubscribe link. Unsubscribes and bounces are added to your do-not-contact list automatically. You can upload \
your own suppression list and import leads from CSV.
- The agent can also draft LinkedIn connection notes and messages, which you send yourself.
- Hot-lead alerts go by email, webhook or Slack. Teams can invite members. There is a public API with API keys. The referral program \
gives the referrer $30 in credits once the referred account has spent $10 on emails.
- The Segments Explorer is a free tool that finds companies matching a description, on demand from the web.";

/// Only true where the operator set BIRD_BIN, so the assistant never promises X research a deployment doesn't have.
const X_FACT: &str = "\n- The agent also searches X (Twitter): it finds people posting about the problem you solve, uses a \
person's recent posts to open their email, and links each lead's X profile next to LinkedIn.";

pub fn router() -> Router<AppState> {
    Router::new().route("/support/messages", post(send))
}

#[derive(Deserialize)]
struct SendBody {
    conversation_id: Option<Uuid>,
    visitor_token: String,
    content: String,
}

#[derive(Deserialize)]
struct Answer {
    reply: String,
}

async fn send(
    State(state): State<AppState>,
    MaybeAuth(auth): MaybeAuth,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<SendBody>,
) -> AppResult<Json<Value>> {
    let visitor = body.visitor_token.trim();
    if visitor.is_empty() || visitor.len() > 200 {
        return Err(AppError::BadRequest("visitor_token is required".into()));
    }
    let content: String = body.content.trim().chars().take(MAX_CONTENT_CHARS).collect();
    if content.is_empty() {
        return Err(AppError::BadRequest("Type a message first".into()));
    }
    let ip = client_ip(&headers, peer);
    let recent: i64 = sqlx::query_scalar(
        "select count(*) from support_messages m join support_conversations c on c.id = m.conversation_id
         where c.ip = $1 and m.role = 'assistant' and m.created_at > now() - interval '1 hour'",
    )
    .bind(&ip)
    .fetch_one(&state.db)
    .await?;
    if recent >= ANSWERS_PER_HOUR_PER_IP {
        return Err(AppError::TooManyRequests("You've sent a lot of messages. Try again in an hour.".into()));
    }

    let conversation_id: Uuid = match body.conversation_id {
        Some(id) => sqlx::query_scalar("select id from support_conversations where id = $1 and visitor_token = $2")
            .bind(id)
            .bind(visitor)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound)?,
        None => sqlx::query_scalar("insert into support_conversations (visitor_token, user_id, ip) values ($1, $2, $3) returning id")
            .bind(visitor)
            .bind(auth.as_ref().map(|a| a.user_id))
            .bind(&ip)
            .fetch_one(&state.db)
            .await?,
    };

    let count: i64 = sqlx::query_scalar("select count(*) from support_messages where conversation_id = $1")
        .bind(conversation_id)
        .fetch_one(&state.db)
        .await?;
    if count + 2 > MAX_CONVERSATION_MESSAGES {
        return Err(AppError::BadRequest("This conversation is full. Start a new one to keep chatting.".into()));
    }
    sqlx::query("insert into support_messages (conversation_id, role, content) values ($1, 'user', $2)")
        .bind(conversation_id)
        .bind(&content)
        .execute(&state.db)
        .await?;

    let history = messages(&state, conversation_id).await?;
    let transcript = history
        .iter()
        .map(|m| format!("{}: {}", if m.role == "user" { "Visitor" } else { "You" }, m.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut system = SUPPORT_PROMPT.to_string();
    if state.cfg.bird_bin.is_some() {
        system.push_str(X_FACT);
    }
    let call = Call::writing(
        system,
        format!("The conversation so far:\n\n{transcript}\n\nWrite your reply to the visitor's last message."),
        json!({"type": "object", "properties": {"reply": {"type": "string"}}, "required": ["reply"]}),
    );
    let answer: Answer = sdk::ask(&state, None, &call).await?;
    sqlx::query("insert into support_messages (conversation_id, role, content) values ($1, 'assistant', $2)")
        .bind(conversation_id)
        .bind(answer.reply.trim())
        .execute(&state.db)
        .await?;

    Ok(Json(json!({"conversation_id": conversation_id, "messages": messages(&state, conversation_id).await?})))
}

async fn messages(state: &AppState, conversation_id: Uuid) -> AppResult<Vec<SupportMessage>> {
    Ok(sqlx::query_as::<_, SupportMessage>("select * from support_messages where conversation_id = $1 order by created_at")
        .bind(conversation_id)
        .fetch_all(&state.db)
        .await?)
}
