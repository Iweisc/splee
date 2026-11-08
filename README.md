# Splee

Splee is a self-hosted AI outbound agent, modelled on [explee.com](https://explee.com). You paste your website. A Claude agent then researches your company, competitors and buyer segments, and finds matching companies and their decision makers on the web. It writes a personal cold email to each person and sends them from your own mailboxes within a daily email limit. It also reads the replies over IMAP and answers interested leads with your booking link.

It runs on your machine or server, with your own Claude subscription and mailboxes. Nothing goes through a third-party outbound platform.

## Features

- **Research.** It profiles your company, maps competitors, proposes buyer segments as campaigns, finds companies and decision makers, and writes personal emails and LinkedIn sequences. Every run streams its progress live and can be cancelled.
- **Sending.** It sends from any SMTP mailbox, rotating mailboxes under per-mailbox daily limits. Sending respects each project's time zone, send days and hours. Follow-ups thread under the first email. You can choose to review every draft or approve whole campaigns.
- **Inbox.** It polls IMAP for replies and sorts them into hot, question, not interested, out of office, unsubscribe or bounce. It drafts replies with your booking link and either sends them or holds them for approval.
- **Safety rails.** It keeps a suppression list of emails and domains, adds an unsubscribe link and `List-Unsubscribe` header to every outreach email, and handles bounces. A daily email limit per project and per mailbox caps sending, and the DNS checker covers MX, SPF and DMARC.
- **Costs.** Spend and cost per hot lead add up what Claude Code reports for each agent run and the cost per email you set on each mailbox. On a Claude subscription, the AI part is the equivalent API price.
- **Integrations.** Hot-lead alerts go out by email, Slack or webhook. Leads import from CSV and export to CSV. A public REST API takes API keys.
- **Teams.** It has organizations, invites and admin roles.

## Quick start

You need:

- Docker, for Postgres.
- [Rust](https://rustup.rs) 1.93 or newer.
- Node.js 20 or newer and [pnpm](https://pnpm.io).
- The [Claude Code CLI](https://docs.claude.com/en/docs/claude-code), signed in. Running `claude` in your shell must work.

From the repo root, run each of these in its own terminal:

```sh
docker compose up -d                                  # Postgres on 127.0.0.1:55432 (dev password, loopback only)
(cd backend && cp -n .env.example .env && cargo run)  # API on 127.0.0.1:8787, runs migrations on start
(cd frontend && pnpm install && pnpm dev)             # app on http://localhost:3333
```

Open http://localhost:3333, sign up and paste a website. The first account in an organization is its owner.

## Configuration

The backend reads `backend/.env`, and the frontend reads its own environment.

| Variable | Default | What it does |
|---|---|---|
| `DATABASE_URL` | required | Postgres connection string. |
| `APP_SECRET` | required | Encrypts mailbox passwords. Set a long random value before you connect a real mailbox, and keep it, because changing it makes the saved passwords unreadable. |
| `PUBLIC_URL` | `http://localhost:3333` | Where the app is served. Used for unsubscribe, invite and inbox links in emails. An `https://` URL also makes the sign-in cookie HTTPS-only. |
| `BIND_ADDR` | `127.0.0.1:8787` | Address the API listens on. |
| `AGENT_MODEL` | `claude-sonnet-5-5` | Claude model for every agent run. |
| `AGENT_CONCURRENCY` | `2` | How many agent runs execute at once. |
| `SENDING_ENABLED` | `true` | `false` stops the sender, so approved emails, follow-ups and auto-replies wait. IMAP polling, agent runs and notification emails to your team still run. |
| `BIRD_BIN` | unset | Path to the [bird](https://www.npmjs.com/package/@steipete/bird) CLI (`npm i -g @steipete/bird`). When set, research runs can search and read X. |
| `RUST_LOG` | unset | Log filter, e.g. `splee=debug,tower_http=info`. |
| `API_URL` (frontend) | `http://127.0.0.1:8787` | Where the Next.js server proxies `/api/*`. |
| `NEXT_PUBLIC_SOURCE_URL` (frontend, build time) | `https://github.com/Iweisc/splee` | Where the "Source code" links in the footer and sidebar point. If you run a modified version for other people, the AGPL requires you to point this at your source. |

bird reads X with the cookies of a browser that is logged into x.com, so the agent sees X as that account. Pick the browser in `~/.config/bird/config.json5`.

## How it works

```
frontend/  Next.js 16, React 19, Tailwind 4. Proxies /api/* to the backend, so the session cookie is same-origin.
backend/   Rust: Axum 0.8, sqlx, Postgres. Runs the API and the background workers in one process.
SPEC.md    Endpoints, data contract, agent pipeline and workers.
```

The agent uses the local `claude` CLI through [`claude-agent-sdk-rs`](https://github.com/tyrchen/claude-agent-sdk-rs). Each call runs in a temp directory without your Claude settings. Research calls can only use web search and fetch, plus the X tools when `BIRD_BIN` is set. Writing and classification calls get no tools.

Every backend process runs the workers: the agent queue, the sender every 30 seconds, the IMAP poller every 60 seconds, the hourly campaign optimizer and the notification digests. Run one backend per database. `SENDING_ENABLED=false` only stops the sender, so a second backend would still poll mailboxes and process replies.

`backend/vendor/claude-agent-sdk-rs` is the crates.io 0.6.4 release with two patches. `MessageParser::parse_known` in `src/internal/message_parser.rs` lets every reader skip message types the crate doesn't know yet, because newer CLIs emit `tool_progress` and `rate_limit_event`. `src/internal/transport/subprocess.rs` spawns the CLI with `kill_on_drop`, so a timed-out call can't leave it running. Drop the `[patch.crates-io]` entry once upstream handles both.

## Development

These are the checks CI runs. Run them before you open a pull request:

```sh
(cd backend && cargo clippy --all-targets -- -D warnings && cargo test && cargo test -p claude-agent-sdk-rs --lib)
(cd frontend && pnpm lint && pnpm typecheck)
```

CI also runs the database tests that `cargo test` skips. [CONTRIBUTING.md](CONTRIBUTING.md) shows how to run them. See it for conventions and [SECURITY.md](SECURITY.md) to report a vulnerability.

## Responsible use

Splee sends real email to real people. You're responsible for following the law where you and your recipients are, including CAN-SPAM, GDPR, PECR and CASL. Splee gives you the tools for that: an unsubscribe link and header on every outreach email, a suppression list, bounce handling and daily limits. Send only to people with a legitimate business reason to hear from you.

## License

[GNU Affero General Public License v3.0](LICENSE). You can self-host, modify and share Splee. If you run a modified version as a service for other people, you must offer them its source code.
