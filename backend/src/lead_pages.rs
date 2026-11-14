//! Lead pages: a campaign with `lead_pages` on gets a short personal page for each lead, written in the same call as the
//! lead's first email, which links to it at PUBLIC_URL/for/{token}. The page's script reports each view, scanners and
//! clouds are filtered out like email clicks (tracking.rs), and the views that count raise the lead's score and ping the
//! team's chats the first time.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::hooks::{self, ChatAlert};
use crate::models::{Lead, Ts};
use crate::tracking::{self, Seen};
use crate::visitors::Kind;

/// A second view from the same address this soon after the last is the same visit, a reload or a back button.
pub const REPEAT_MINUTES: i32 = 30;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Point {
    pub title: String,
    pub body: String,
}

/// What the writer returns for a lead's page along with the email.
#[derive(Debug, Clone, Deserialize)]
pub struct PageDraft {
    pub headline: String,
    #[serde(default)]
    pub intro: String,
    #[serde(default)]
    pub points: Vec<Point>,
    #[serde(default)]
    pub proof: String,
    /// The email's line before the link, like "I put together a one-page overview for Acme:".
    pub link_line: String,
}

/// The `page` property of the writer's schema.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "headline": { "type": "string" },
            "intro": { "type": "string" },
            "points": { "type": "array", "items": { "type": "object", "properties": { "title": { "type": "string" }, "body": { "type": "string" } }, "required": ["title", "body"] } },
            "proof": { "type": "string" },
            "link_line": { "type": "string" }
        },
        "required": ["headline", "intro", "points", "proof", "link_line"]
    })
}

/// Appended to the first-email prompt of a campaign with lead pages.
pub const RULE: &str = "\n\nPersonal page: this campaign also gives each lead a short web page written for them, linked from the email. \
For each email also return \"page\":\n\
- headline: at most 10 words, about their company's situation and what changes with us, naming their company. Not a slogan.\n\
- intro: 2 or 3 sentences to the lead by first name, picking up the email's observation and saying what the page covers.\n\
- points: exactly 3, each a title of at most 6 words and a body of 1 or 2 sentences on how we'd help their company in particular, \
from the lead's facts and what we offer. No invented numbers, customers or features.\n\
- proof: one or two sentences of real proof from our materials, like a customer result, or empty when the materials have none. Never invent it.\n\
- link_line: the line the email puts before the link, in the email's language, like \"I put together a one-page overview for Acme:\".\n\
The email itself must not mention or link the page: Splee adds the link_line and the link after the email's last paragraph.";

/// The page's public address.
pub fn url(public_url: &str, token: &str) -> String {
    format!("{}/for/{token}", public_url.trim_end_matches('/'))
}

/// The email with the page's line and link after its last paragraph, before the signature `sign` adds.
pub fn with_link(body: &str, link_line: &str, url: &str) -> String {
    let line = link_line.trim();
    let line = if line.is_empty() { "I put together a short page for you:" } else { line };
    format!("{}\n\n{line}\n{url}", body.trim_end())
}

/// Saves the lead's page, or rewrites it in place, and returns its token. The writer's text is trimmed and cut to sizes the
/// page lays out well.
pub async fn save(db: &PgPool, lead: &Lead, draft: &PageDraft) -> sqlx::Result<String> {
    let cut = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    let points: Vec<Point> = draft
        .points
        .iter()
        .filter(|p| !p.title.trim().is_empty())
        .take(3)
        .map(|p| Point { title: cut(&p.title, 80), body: cut(&p.body, 400) })
        .collect();
    let token: String = crate::crypto::random_token().chars().take(20).collect();
    sqlx::query_scalar(
        "insert into lead_pages (project_id, lead_id, token, headline, intro, points, proof) values ($1, $2, $3, $4, $5, $6, $7)
         on conflict (lead_id) do update set headline = excluded.headline, intro = excluded.intro, points = excluded.points,
                                             proof = excluded.proof, updated_at = now()
         returning token",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(&token)
    .bind(cut(&draft.headline, 120))
    .bind(cut(&draft.intro, 800))
    .bind(sqlx::types::Json(&points))
    .bind(cut(&draft.proof, 500))
    .fetch_one(db)
    .await
}

/// What the public page shows. Only what the lead's own email already said, plus who sent it.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Public {
    pub headline: String,
    pub intro: String,
    pub points: sqlx::types::Json<Vec<Point>>,
    pub proof: String,
    pub first_name: String,
    pub company: String,
    pub company_domain: Option<String>,
    /// The project's company name and domain, from its research.
    pub from_name: String,
    pub from_domain: Option<String>,
    pub sender_name: String,
    /// The sending mailbox's booking link, else its Splee booking page, else the project's booking link. Null without any.
    pub booking_url: Option<String>,
    /// Who to write back to when there's no booking link: the sending mailbox.
    pub reply_to: Option<String>,
}

pub async fn public(db: &PgPool, public_url: &str, token: &str) -> sqlx::Result<Option<Public>> {
    sqlx::query_as::<_, Public>(
        "select p.headline, p.intro, p.points, p.proof, l.first_name, l.company_name as company, l.company_domain::text as company_domain,
                coalesce(nullif(pr.company->>'name', ''), pr.name) as from_name, pr.domain::text as from_domain,
                coalesce(nullif(m.from_name, ''), s.sender_name, '') as sender_name,
                coalesce(nullif(m.booking_link, ''), $2 || '/book/' || b.slug, nullif(s.booking_link, '')) as booking_url,
                m.email as reply_to
         from lead_pages p join leads l on l.id = p.lead_id join projects pr on pr.id = p.project_id
         left join project_settings s on s.project_id = p.project_id
         left join lateral (select e.mailbox_id from email_messages e where e.lead_id = l.id and e.kind = 'initial' and e.mailbox_id is not null
                            order by e.created_at desc limit 1) e on true
         left join mailboxes m on m.id = e.mailbox_id
         left join booking_pages b on b.mailbox_id = m.id
         where p.token = $1",
    )
    .bind(token)
    .bind(public_url.trim_end_matches('/'))
    .fetch_optional(db)
    .await
}

/// A view as the page's script reported it.
pub struct View {
    pub user_agent: String,
    pub ip: Option<IpAddr>,
    pub ip_hash: String,
    pub at: Ts,
}

/// Stores the view with why it doesn't count, if it doesn't, and returns whether it was the page's first counted view.
pub async fn record(db: &PgPool, token: &str, view: &View, network: Kind) -> sqlx::Result<Option<FirstView>> {
    let seen = Seen { head: false, user_agent: &view.user_agent, ip: view.ip, network, at: view.at };
    let mut tx = db.begin().await?;
    let page: Option<(Uuid, Uuid, Uuid)> = sqlx::query_as("select id, project_id, lead_id from lead_pages where token = $1 for update").bind(token).fetch_optional(&mut *tx).await?;
    let Some((page_id, project_id, lead_id)) = page else { return Ok(None) };
    // The page needs its script to run, so most link checkers never get here. The sent time isn't a filter here: the page
    // only loads after a click, which tracking already held to the same rules.
    let mut reason = tracking::filter_reason(&seen, None).map(str::to_string);
    if reason.is_none() && !view.ip_hash.is_empty() {
        let repeat: bool = sqlx::query_scalar(
            "select exists (select 1 from lead_page_views where page_id = $1 and ip_hash = $2 and filter_reason is null
                            and created_at > $3 - make_interval(mins => $4))",
        )
        .bind(page_id)
        .bind(&view.ip_hash)
        .bind(view.at)
        .bind(REPEAT_MINUTES)
        .fetch_one(&mut *tx)
        .await?;
        if repeat {
            reason = Some("repeat".into());
        }
    }
    sqlx::query("insert into lead_page_views (page_id, project_id, lead_id, ip_hash, user_agent, filter_reason, created_at) values ($1, $2, $3, $4, $5, $6, $7)")
        .bind(page_id)
        .bind(project_id)
        .bind(lead_id)
        .bind(&view.ip_hash)
        .bind(view.user_agent.chars().take(300).collect::<String>())
        .bind(&reason)
        .bind(view.at)
        .execute(&mut *tx)
        .await?;
    if reason.is_some() {
        tx.commit().await?;
        return Ok(None);
    }
    let first: bool = sqlx::query_scalar(
        "update lead_pages set views = views + 1, first_viewed_at = coalesce(first_viewed_at, $2), last_viewed_at = $2 where id = $1
         returning views = 1",
    )
    .bind(page_id)
    .bind(view.at)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(first.then_some(FirstView { project_id, lead_id }))
}

pub struct FirstView {
    pub project_id: Uuid,
    pub lead_id: Uuid,
}

/// Tells the project's chats that a lead opened their page for the first time.
pub async fn announce(db: &PgPool, public_url: &str, first: &FirstView) -> anyhow::Result<()> {
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(first.lead_id).fetch_one(db).await?;
    let who = if lead.full_name.trim().is_empty() { lead.email.clone().unwrap_or_default() } else { lead.full_name.trim().to_string() };
    let summary = if lead.company_name.trim().is_empty() { format!("{who} opened their page") } else { format!("{who} ({}) opened their page", lead.company_name.trim()) };
    let link = format!("{}/app/p/{}/leads?lead={}", public_url.trim_end_matches('/'), first.project_id, lead.id);
    let alert = ChatAlert { label: "Page opened", summary: &summary, quote: None, link: &link, link_text: "Open the lead" };
    hooks::post_chat(db, first.project_id, &alert).await;
    Ok(())
}

/// The lead's page for the app, with its address and views.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ForLead {
    pub token: String,
    pub headline: String,
    pub views: i32,
    pub first_viewed_at: Option<Ts>,
    pub last_viewed_at: Option<Ts>,
    #[sqlx(skip)]
    pub url: String,
}

pub async fn for_lead(db: &PgPool, public_url: &str, lead_id: Uuid) -> sqlx::Result<Option<ForLead>> {
    let page = sqlx::query_as::<_, ForLead>("select token, headline, views, first_viewed_at, last_viewed_at from lead_pages where lead_id = $1")
        .bind(lead_id)
        .fetch_optional(db)
        .await?;
    Ok(page.map(|p| ForLead { url: url(public_url, &p.token), ..p }))
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    #[test]
    fn the_link_goes_after_the_last_paragraph_with_a_line_before_it() {
        assert_eq!(with_link("Hi Ana,\n\nWorth a call?\n", " Here's a page for Acme: ", "https://x.example/for/abc"), "Hi Ana,\n\nWorth a call?\n\nHere's a page for Acme:\nhttps://x.example/for/abc");
        assert!(with_link("Hi", "", "u").ends_with("I put together a short page for you:\nu"));
        assert_eq!(url("https://app.example/", "abc"), "https://app.example/for/abc");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn views_count_once_per_visit_and_never_from_scanners(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into project_settings (project_id, booking_link, sender_name) values ($1, 'https://cal.example/dana', 'Dana')").bind(project).execute(&db).await.unwrap();
        let lead = sqlx::query_as::<_, Lead>("insert into leads (project_id, full_name, first_name, company_name) values ($1, 'Ana Ruiz', 'Ana', 'Smile Dental') returning *")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        let draft = PageDraft { headline: "Fewer no-shows".into(), intro: "Hi".into(), points: vec![], proof: String::new(), link_line: String::new() };
        let token = save(&db, &lead, &draft).await.unwrap();
        assert_eq!(token.len(), 20);

        let page = public(&db, "https://app.example", &token).await.unwrap().unwrap();
        assert_eq!((page.from_name.as_str(), page.first_name.as_str(), page.sender_name.as_str()), ("Fieldnote", "Ana", "Dana"));
        assert_eq!(page.booking_url.as_deref(), Some("https://cal.example/dana"), "the project's link without a sending mailbox");
        assert!(public(&db, "https://app.example", "nope").await.unwrap().is_none());

        let chrome = "Mozilla/5.0 (Macintosh) AppleWebKit/537.36 Chrome/131.0 Safari/537.36";
        let at = Utc::now();
        let view = |agent: &str, ip: &str, minutes: i64| View { user_agent: agent.into(), ip: ip.parse().ok(), ip_hash: ip.into(), at: at + Duration::minutes(minutes) };
        assert!(record(&db, &token, &view("Mozilla/5.0 (compatible; bingbot/2.0)", "198.51.100.1", 0), Kind::Unknown).await.unwrap().is_none());
        assert!(record(&db, &token, &view(chrome, "198.51.100.2", 0), Kind::Hosting).await.unwrap().is_none(), "a cloud");
        let first = record(&db, &token, &view(chrome, "198.51.100.3", 0), Kind::Unknown).await.unwrap();
        assert!(first.is_some_and(|f| f.lead_id == lead.id), "the first person to look");
        assert!(record(&db, &token, &view(chrome, "198.51.100.3", 5), Kind::Unknown).await.unwrap().is_none(), "a reload");
        assert!(record(&db, &token, &view(chrome, "198.51.100.3", 45), Kind::Unknown).await.unwrap().is_none(), "counted, but not the first");
        assert!(record(&db, "nope", &view(chrome, "198.51.100.4", 0), Kind::Unknown).await.unwrap().is_none());

        let mine = for_lead(&db, "https://app.example", lead.id).await.unwrap().unwrap();
        assert_eq!((mine.views, mine.url), (2, format!("https://app.example/for/{token}")));
        let reasons: Vec<Option<String>> = sqlx::query_scalar("select filter_reason from lead_page_views order by created_at, filter_reason nulls first").fetch_all(&db).await.unwrap();
        assert_eq!(reasons.iter().filter(|r| r.is_none()).count(), 2);
        assert!(reasons.contains(&Some("scanner_agent".into())) && reasons.contains(&Some("cloud".into())) && reasons.contains(&Some("repeat".into())));

        crate::score::recompute(&db, project).await.unwrap();
        let reasons: serde_json::Value = sqlx::query_scalar("select score_reasons from leads where id = $1").bind(lead.id).fetch_one(&db).await.unwrap();
        assert!(reasons.to_string().contains("Opened their page 2 times"), "{reasons}");
    }
}
