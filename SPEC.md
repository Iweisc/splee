# Splee spec

Splee is a functional clone of explee.com: an AI outbound agent. You paste your website, a Claude agent researches your company, competitors and buyer segments, finds companies and decision makers on the web, writes personal cold emails, sends them from your own mailboxes within a daily email limit, reads replies over IMAP, classifies them, and auto-replies with your booking link.

- Backend: Rust, Axum 0.8, sqlx + Postgres, `claude-agent-sdk-rs` (drives the local `claude` CLI). `backend/`
- Frontend: Next.js 16 App Router, React 19, Tailwind 4. `frontend/`
- Next rewrites `/api/*` to the backend (`http://127.0.0.1:8787/api/*`), so cookies are same-origin.
- Postgres: `docker compose up -d` (port 55432). Backend: `cd backend && cargo run`. Frontend: `cd frontend && pnpm dev` (port 3333).

Schema: `backend/migrations/`. Rust rows: `backend/src/models.rs`. TS types: `frontend/src/lib/types.ts`. Those three are the contract. Change all three together.

## Conventions

- JSON in and out. Errors are `{ "error": "message" }` with a meaningful status (see `backend/src/error.rs`).
- Money is micro-dollars (`*_micros`, 1 USD = 1_000_000).
- Timestamps are RFC 3339 strings. IDs are UUIDs.
- Lists return `{ "items": [...], "total": n }` when paginated (`?page=1&per_page=50`, page is 1-based). Small fixed lists return a bare array.
- Auth: cookie `splee_session` (extractor `Auth` in `backend/src/auth.rs`). `Auth::project(db, id)` checks org ownership. Admin-only routes call `auth.require_admin()`.
- Public API v1 uses `Authorization: Bearer splee_...` (extractor `ApiKeyAuth`).
- Agent work is always `agent::enqueue(db, kind, project_id, org_id, input)` from routes. Never call Claude inside a request handler, except support chat.
- Splee is free. Sending stops for the day once the project has sent `daily_email_limit` emails or a mailbox reaches its `daily_limit`. Spend in stats is real cost, not credits: `agent_runs.cost_usd` (what Claude Code reports) plus `email_messages.cost_micros`, which is the sending mailbox's `cost_per_email_micros`.

## Agent pipeline (explore run)

One `explore` run executes 6 steps in order and emits `agent_events` as it goes. `agent_runs.step` is the current step (1-6). Each step writes its results to the normal tables as soon as it finishes, so the UI can render partial results.

| step | title (UI) | writes |
|------|-----------|--------|
| 1 | Research your company | `projects.company`, `projects.name` |
| 2 | Explore competitors | `competitors` (up to 12) |
| 3 | Define campaigns | `segments` (4-6, with icon, est_size, fit_score, targeting) |
| 4 | Find potential customers | `companies` for the top segment (demo: 10, app: 25) |
| 5 | Find decision makers | `leads` for the best companies (demo: 6, app: 20) |
| 6 | Write personal emails | `email_messages` drafts, kind `initial`, status `draft` (demo: 3, app: all leads) |

Event kinds: `phase` (data `{step, title, status: "started"|"finished"}`), `log` (short line like `searching "alternatives to larksilk"…`), `tool` (data `{name, input}`), `done`, `error`.

`segments.icon` is one of: `building, store, hotel, heart, palette, home, scissors, briefcase, graduation-cap, stethoscope, factory, truck, utensils, code, shopping-cart, users, landmark, plane, dumbbell, camera`.

`companies.employees` is a display string like `11-50`.

### Run inputs (`agent_runs.input`), enqueued by routes, consumed by the agent worker

| kind | input |
|---|---|
| explore | `{mode: "demo"|"app", from_step?: number (default 1), note?: string}` (demo sizes vs app sizes in the table above) |
| find_companies | `{segment_id, count}` |
| find_people | `{segment_id, company_ids?: uuid[], max_per_company?: number (default 2)}` |
| write_emails | `{segment_id?, lead_ids?: uuid[], instructions?: string, follow_up?: bool, rewrite?: bool}`. Writes `initial` drafts for leads that have none. `follow_up` writes each lead's next follow-up. `rewrite` rewrites each lead's unsent first email or pending follow-up in place, keeping its status. |
| classify_reply | `{message_id}` (inbound email_messages row) |
| draft_reply | `{message_id}` |
| linkedin_sequence | `{lead_id}` (connect note + 2 follow-up messages → `linkedin_messages`) |
| explorer | `{query, country?, ip}` → `output.results: ExplorerCompany[]` |
| segment_from_prompt | `{segment_id, prompt}` fills the draft segment's fields |

`research_company` is reserved and unused.

## Endpoints

All paths below are under `/api`.

### Public (no auth)

| method | path | body / query | response |
|---|---|---|---|
| GET | /health | | `{ok:true}` when Postgres answers, else 503 with no body. For uptime monitors. |
| GET | /public/stats | | `{hot_leads, companies}` (counts of leads with status hot/meeting_booked and orgs; add 0 offsets, no fake numbers) |
| POST | /demo | `{domain?: string, description?: string}` exactly one | `{project_id, token}`. Reuses an anonymous project for the same domain from the last 24h. Rate limit 10/hour per IP (the last `X-Forwarded-For` entry, which the HTTPS proxy sets, or the peer). With `PUBLIC_AGENT=false`, every request that would start a run gets 403 (a reused project still returns 200), as do `/demo/:id/correct`, `/explorer` and signed-out `/support/messages`. Anonymous demo projects and explorer runs are deleted 7 days after they're created. |
| GET | /demo/:project_id?token= | | `DemoState` |
| GET | /demo/:project_id/events?token=&after=<event id> | | SSE. Each message `data: AgentEvent` JSON. Sends backlog after `after`, then live. Ends after `done`/`error`. |
| POST | /demo/:project_id/correct?token= | `{note}` | re-runs from step 1 with the user's correction ("Details look off?"). `{ok:true}` |
| POST | /support/messages | `{conversation_id?, visitor_token, content}` | `{conversation_id, messages: SupportMessage[]}` with the assistant reply appended. Claude answers from the FAQ + pricing facts in SPEC. Conversations are deleted 7 days after their last message, and the widget starts a new one when its stored conversation 404s. |
| POST | /explorer | `{query, country?}` | `{run_id}` (Segments Explorer, public, rate limited) |
| GET | /explorer/:run_id | | `{run: AgentRun, results: ExplorerCompany[]}` |
| GET | /unsubscribe/:token | | `{email, unsubscribed}`. Read-only, because mail security scanners open links in a browser. |
| POST | /unsubscribe/:token | | marks lead unsubscribed + suppression, returns `{ok:true, email}`. The `/unsubscribe/[token]` page's button and the RFC 8058 `List-Unsubscribe-Post` one-click both use it. |

### Auth

| method | path | body | response |
|---|---|---|---|
| POST | /auth/signup | `{email, password, name, claim?: {project_id, token}, invite_token?}` | `Me`, sets cookie. Creates org (unless invite), owner membership, default notification prefs. `claim` moves the demo project into the org, creates `project_settings`. 403 without `invite_token` when `OPEN_SIGNUP=false` and an account already exists. |
| POST | /auth/login | `{email, password}` | `Me`, sets cookie. 429 after 10 failed attempts for an email (known or not) within 15 minutes. A successful sign-in resets the count. |
| POST | /auth/logout | | `{ok:true}` |
| GET | /auth/me | | `Me` or 401. `Me` is `{user, org, role, orgs, public_url}`. `public_url` is `PUBLIC_URL`, which the mailboxes page checks against the address the app is open at. |
| POST | /auth/password | `{current_password, new_password}` | `{ok}`. 400 when the current password is wrong or the new one is under 8 characters. Deletes the user's other sessions. |
| POST | /auth/switch-org | `{org_id}` | `Me` |
| GET | /invites/:token | | `{org_name, email, role}` (public). 404 once the invite is used, revoked or 7 days old. |
| POST | /invites/:token/accept | | `Me` (signed-in user joins org). 400 for a used, revoked or 7-day-old invite, as does signup with that `invite_token`. |

### Organization

| method | path | body | response |
|---|---|---|---|
| GET/PATCH | /organization | `{name}` | `Organization` |
| GET | /organization/users | | `Member[]` |
| PATCH | /organization/users/:user_id | `{role}` | `Member` (admin) |
| DELETE | /organization/users/:user_id | | `{ok}` (admin, can't remove last owner) |
| GET/POST | /organization/invites | `{email, role}` | `Invite[]` / `{invite, link}` (link = PUBLIC_URL/invite/token; also emailed if the org has an active mailbox). `Invite.expires_at` = created_at + 7 days. Expired invites stay listed until revoked. |
| DELETE | /organization/invites/:id | | `{ok}` |
| GET | /organization/deletion-eligibility | | `{eligible, reason}` (not eligible with emails scheduled in the next hour) |
| DELETE | /organization | | `{ok}` (owner) |
| GET/POST | /organization/api-keys | `{name}` | `ApiKey[]` / `{api_key: ApiKey, secret}` (secret shown once) |
| DELETE | /organization/api-keys/:id | | `{ok}` (revoke) |
| GET/PUT | /notification-preferences | `NotificationPrefs` | `NotificationPrefs` |

### Projects and campaigns

| method | path | body | response |
|---|---|---|---|
| GET | /projects | | `ProjectSummary[]` |
| POST | /projects | `{domain?, description?}` | `Project` + enqueues `explore` with `{mode:"app"}` |
| POST | /projects/claim | `{project_id, token}` | `Project`. Signed-in version of signup `claim`. |
| GET/PATCH/DELETE | /projects/:id | `{name?, company?}` | `ProjectDetail` |
| POST | /projects/:id/research | `{note?}` | re-enqueue `explore` from step 1 `{run_id}` |
| GET | /projects/:id/stats?days=30 | | `ProjectStats` |
| GET | /projects/:id/stats/steps | | `StepStats[]` `{step, sent, replied}` per sequence step that sent mail (0 is the first email, n is follow-up n). A lead's first non-bounce reply counts for the last first email or follow-up sent before it. Shown on the Follow-ups page. |
| GET | /projects/:id/stats/weekdays | | `WeekdayStats[]` `{weekday, sent, replied}` per day of the week (1 Monday to 7 Sunday, in the project's time zone) that sent a first email or follow-up, with replies counted the same way as steps. Shown on the Schedule page. |
| GET | /projects/:id/stats/hours | | `HourStats[]` `{hour, sent, replied}` per hour of the day (0 to 23, in the project's time zone), counted like weekdays. Shown on the Schedule page. |
| GET | /projects/:id/runs | | `AgentRun[]` (last 50) |
| POST | /projects/:id/rewrite-emails | | `{run_id}`. One write_emails run in rewrite mode for every lead with an unsent (draft/approved/scheduled) first email or follow-up. 409 when there are none or a write run is already going. The sender holds those leads until the run ends. |
| GET/POST | /projects/:id/competitors | `{domain, name?, note?}` | `Competitor[]` / `Competitor`. 409 when the domain is already listed. The explore run's competitor step replaces the whole list. |
| DELETE | /competitors/:id | | `{ok}` |
| GET | /projects/:id/segments | | `SegmentWithStats[]` |
| POST | /projects/:id/segments | `{prompt}` or full segment fields | `Segment`. With `prompt`, enqueues `segment_from_prompt` and returns the draft segment immediately |
| GET/PATCH/DELETE | /segments/:id | Segment fields | `SegmentWithStats` |
| POST | /segments/:id/find-companies | `{count?}` | `{run_id}` |
| POST | /segments/:id/find-people | `{company_ids?}` | `{run_id}` |
| POST | /segments/:id/write-emails | `{lead_ids?}` | `{run_id}` |
| POST | /segments/:id/start | | sets status `working`, approves its drafts, `{ok}` |
| POST | /segments/:id/pause | | `{ok}` |
| GET | /projects/:id/companies?segment_id&status&q&page | | paginated `Company` |
| PATCH | /companies/:id | `{status}` | `Company` |
| GET | /projects/:id/leads?segment_id&status&q&page | | paginated `LeadRow` |
| GET | /projects/:id/leads/export?segment_id&status&q | | `text/csv` download of every matching lead. Cells that start with `= + - @` get a leading `'`. |
| GET/PATCH/DELETE | /leads/:id | `{status?, email?, title?...}` | `LeadDetail` |
| GET/PUT | /projects/:id/settings | `ProjectSettings` fields | `ProjectSettings`. `webhook_error`/`webhook_failed_at` and `slack_error`/`slack_failed_at` are read-only: the last failed alert, cleared when a delivery or a passing test to the saved URL goes through, or when PUT changes that URL. |
| POST | /projects/:id/integrations/test | `{target: "slack"\|"webhook", url}` | `{ok}`. Posts a sample alert to the URL as typed. The webhook gets the `hot_lead` shape with `"event": "test"` and a sample lead, signed like real alerts. Slack gets `{text}` naming the project. 400 with the reason when it fails. A pass on the saved URL clears its last failure. |
| POST | /projects/:id/integrations/webhook-secret | | `ProjectSettings` with a new random `webhook_secret`. The old one stops verifying immediately. |
| POST | /projects/:id/start | | unpauses the project (requires at least one active mailbox, else 400 with a clear message) `{ok}` |
| POST | /projects/:id/pause | | `{ok}` |
| GET/POST | /projects/:id/materials | `{kind,title,content,url}` | `Material[]` / `Material` |
| PATCH/DELETE | /materials/:id | | `Material` / `{ok}` |
| POST | /projects/:id/import | multipart: `file` (CSV), `segment_id?`, `mapping` (JSON `{email:"Email", full_name:"Name", ...}`) | `Import` (skips suppressed, duplicate and invalid emails) |
| GET | /projects/:id/imports | | `Import[]` |
| POST | /projects/:id/import/preview | multipart `file` | `{headers: string[], rows: string[][] (first 5), suggested_mapping}` |

### Emails and inbox

| method | path | body | response |
|---|---|---|---|
| GET | /emails?project_id&segment_id&status&kind&q&page | | paginated `EmailRow` (outbound only). `status` and `kind` take comma lists. |
| GET/PATCH | /emails/:id | `{subject?, body?, status?: "approved"|"cancelled"}` | `EmailRow` |
| POST | /emails/:id/regenerate | `{instructions?}` | `{run_id}` (write_emails in rewrite mode for that one lead) |
| POST | /emails/:id/test | `{subject?, body?}` (unsaved edits) | `{sent_to}`. Sends the email with its unsubscribe footer to the signed-in user from the org's first active mailbox. The footer link is `PUBLIC_URL/unsubscribe/test`, so it can't unsubscribe the lead. 400 without an active mailbox or when SMTP fails. |
| GET | /inbox?project_id&filter=all|hot|needs_reply|auto_replied|other&page | | paginated `InboxThread`. `needs_reply` is threads whose last message is a reply, including ones with an auto-reply draft waiting for approval, except declines, out-of-office notes, unsubscribes and bounces, and leads marked meeting booked, not interested, bounced, unsubscribed or do-not-contact, or whose latest reply was marked done. Those are how you clear a reply handled outside Splee. |
| GET | /inbox/threads/:lead_id | | `{lead: LeadDetail, messages: EmailMessage[] (both directions, oldest first)}` |
| POST | /inbox/threads/:lead_id/reply | `{body}` | queues a `manual_reply` (status scheduled, now) `EmailMessage` |
| POST | /inbox/messages/:id/approve | `{body?}` | approves a pending `auto_reply` draft |
| POST | /inbox/threads/:lead_id/status | `{status}` | updates lead status (hot, meeting_booked, not_interested...) |
| POST | /inbox/threads/:lead_id/done | | `{ok}`. Sets `handled_at` on the lead's replies, for one that needs no answer. The thread leaves `needs_reply` until the lead writes again. |
| GET | /projects/:id/linkedin?segment_id | | `[{lead: LeadRow, messages: LinkedinMessage[]}]` |
| POST | /leads/:id/linkedin | | `{run_id}` (linkedin_sequence) |
| PATCH | /linkedin/:id | `{status, body?}` | `LinkedinMessage` |

### Mailboxes and suppressions

| method | path | body | response |
|---|---|---|---|
| GET/POST | /mailboxes | `MailboxInput` | `Mailbox[]` / `Mailbox` (passwords encrypted with `crypto::encrypt`) |
| GET | /mailboxes/stats | | `MailboxStats[]`: per mailbox that sent in the last 30 days, distinct leads `emailed` from it and, of the mail it received, leads that `bounced` and `replied`. The page warns past 5% bounced with at least 20 leads (`MailboxStats::bouncing`). |
| PATCH/DELETE | /mailboxes/:id | partial `MailboxInput` (empty password = keep) | `Mailbox` |
| POST | /mailboxes/:id/test | | `{ok, error?}`; sets status/last_error |
| POST | /mailboxes/:id/check-dns | | `Mailbox` with `dns` |
| GET | /suppressions?q&page | | paginated `Suppression` |
| GET | /suppressions/export?q | | `text/csv` download of every matching entry, newest first: `value, kind, reason, created_at`. Cells that start with `= + - @` get a leading `'`. |
| POST | /suppressions | `{values: string}` newline/comma separated emails or domains | `{added}` |
| DELETE | /suppressions/:id | | `{ok}` |

### Runs

| method | path | response |
|---|---|---|
| GET | /runs/:id | `{run: AgentRun, events: AgentEvent[]}` (auth, org scoped) |
| GET | /runs/:id/events?after= | SSE like the demo stream (auth) |
| POST | /runs/:id/cancel | `{ok}` |
| POST | /runs/:id/retry | `{run_id}`. Queues a failed or cancelled project run again with the same input (an explore run marks the project researching). 409 for any other status. |

### Public API v1 (ApiKeyAuth)

`GET /v1/projects`, `GET /v1/projects/:id/leads?status&page`, `POST /v1/projects/:id/leads` (`{leads: [{email, full_name?, title?, company_name?, company_domain?, linkedin_url?, segment_id?}]}` → `{imported, skipped}`), `GET /v1/leads?email=` (the org's leads with that address across projects, newest first, up to 100), `GET /v1/leads/:id`, `PATCH /v1/leads/:id` (`{status}` → `LeadDetail`), `GET /v1/projects/:id/replies?since=` (inbound messages), `GET /v1/projects/:id/sent-emails?since=` (sent outbound messages, oldest by `sent_at` first, up to 500), `GET /v1/projects/:id/stats` (`{totals: Totals, campaigns: SegmentWithStats[], steps: StepStats[]}`, the same numbers as `/projects/:id/stats` and `/projects/:id/stats/steps` without the daily series), `GET /v1/suppressions?since&page&per_page` (paginated `Suppression`, oldest first, `since` filtering on `created_at`), `POST /v1/suppressions` (`{values: string[]}`, up to 1,000 emails or domains → `{added}`, 400 naming the first invalid value), `POST /v1/segments/:id/write-emails` (no body → `{run_id}`, a write_emails run for every lead in the campaign without a first email, 409 when there are none), `GET /v1/runs/:id` (`AgentRun` of the org, for polling a run until it's succeeded, failed or cancelled).

## Background workers

- Agent worker: claims `agent_runs` with `for update skip locked`, runs up to `AGENT_CONCURRENCY` at once. Each Claude call uses `claude-agent-sdk-rs` `ClaudeClient` with `setting_sources: []` (never load the host user's Claude settings), `cwd` = a temp dir, `tools`/`allowed_tools` = `WebSearch`, `WebFetch` only (none for classification/writing), JSON-schema `output_format`, model `AGENT_MODEL`. With `BIRD_BIN` set, research calls also get `mcp__x__search` and `mcp__x__read`, an in-process MCP server that runs the `bird` CLI (npm `@steipete/bird`) to search and read X with a logged-in browser's cookies. Tool calls and short text become `tool`/`log` events. Cost is added to `agent_runs.cost_usd`.
- Sender (every 30s): for each unpaused project inside its send window (timezone, days, hours), with fewer outbound emails sent today than its `daily_email_limit` (replies count but are never held back): take due `approved`/`scheduled` emails, pick an active mailbox under today's limit (round robin; `daily_limit`, or with `ramp_up` 10 plus 4 a day since `first_sent_on`, up to it), send via SMTP with `Message-ID`, `List-Unsubscribe` header and an unsubscribe footer link (`PUBLIC_URL/unsubscribe/<lead.unsubscribe_token>`), skip suppressed recipients (cancelled), mark emails to leads without an address or to a domain with no mail server `failed` with the reason, record the mailbox's `cost_per_email_micros`, mark lead `contacted`, send the `email_sent` webhook event. Follow-ups: when a lead has no inbound reply, create the next follow-up draft per `project_settings.follow_ups` (written by the agent, threaded with `In-Reply-To`) and approve it automatically when the segment is working.
- IMAP poller (every 60s): fetch new messages per mailbox (UID > imap_last_uid; -1 means not polled yet, and the first poll starts just before the mail that arrived since the mailbox's first send, a day early, or after the newest message when it hasn't sent; changing the IMAP host resets it to -1, and so does a changed INBOX UIDVALIDITY, which `imap_uid_validity` records), match to lead by `In-Reply-To`/`References` then by from address, store `inbound` rows, enqueue `classify_reply`. Bounces (mailer-daemon) mark lead `bounced` + suppression. Classification `hot` → lead `hot`, webhook + Slack notify (Slack also gets a post when a lead moves to `meeting_booked` from the app or `PATCH /v1/leads/:id`, which doesn't send the `meeting_booked` webhook; the webhook gets the events listed in `project_settings.webhook_events`, `hot_lead` by default, out of `hot_lead`, `lead_replied`, `meeting_booked`, `lead_unsubscribed`, `lead_bounced` and `email_sent`; requests carry `Splee-Timestamp` and `Splee-Signature: v1=<hex HMAC-SHA256 of "{timestamp}.{raw body}">` keyed with `project_settings.webhook_secret`, Slack is unsigned. A timeout, refused connection, 429 or 5xx is retried once after 10 seconds, signed afresh, so receivers should dedupe by `event` with `reply.id` or `email.id`, since a hot reply sends both `lead_replied` and `hot_lead` with the same reply); `unsubscribe` → suppression; `ooo` → reschedule. For hot/question replies when `auto_reply_mode != off`, enqueue `draft_reply` (includes booking link) → `auto` sends it, `approve` leaves it as draft for the inbox.
- Digests (every 15 minutes, acting at 08:00 UTC, weekly reports on Mondays): per org member with the notification pref on, once a day via `notification_log`: each project's sends, replies, hot leads and spend, stopped mailboxes, mailboxes past the bounce warning, and replies waiting. A stopped or bouncing mailbox sends one even on a quiet day.
- Optimizer (hourly): per working segment with >= 40 sent emails, sent emails per hot lead < 250 → `scaling`, > 625 → `paused` (once: `auto_paused_at` records it, and a campaign you resume afterwards stays `working`), else `working`. Scaling segments get first pick in the sender.

## Frontend routes

Marketing: `/`, `/about`, `/b2b-database`, `/b2b-database/locations/[region]`, `/gm-dataset`, `/tools/explorer`, `/compliance-faq`, `/terms-of-use`, `/privacy-policy`, `/autogtm`, `/unsubscribe/[token]`, `/signin`, `/signup`, `/invite/[token]`, demo `/auto-gtm/company/[slug]/explore` (slug = domain, or `p-<project_id>` for "I don't have a website"), and the support chat bubble on every page.

App under `/app`: see `frontend/src/components/app/nav.ts`.
