# Splee

Splee is a self-hosted AI outbound agent, modelled on [explee.com](https://explee.com). You paste your website. A Claude agent then researches your company, competitors and buyer segments, and finds matching companies and their decision makers on the web. It writes a personal cold email to each person and sends them from your own mailboxes within a daily email limit. It also reads the replies over IMAP and answers interested leads with your booking link.

It runs on your machine or server, with your own Claude subscription and mailboxes. Nothing goes through a third-party outbound platform.

## Features

- **Research.** It profiles your company, maps competitors, proposes buyer segments as campaigns, finds companies and decision makers, and writes personal emails and LinkedIn sequences. Every run streams its progress live and can be cancelled.
- **Sending.** It sends from any SMTP mailbox, rotating mailboxes under per-mailbox daily limits. Sending respects each project's time zone, send days and hours. Follow-ups thread under the first email. You can choose to review every draft or approve whole campaigns.
- **Inbox.** It polls IMAP for replies and sorts them into hot, question, not interested, out of office, unsubscribe or bounce. It drafts replies with your booking link and either sends them or holds them for approval. Saved replies drop your usual answers into a reply, with the lead's name and company filled in.
- **Safety rails.** It keeps a suppression list of emails and domains, adds an unsubscribe link and `List-Unsubscribe` header to every outreach email, and handles bounces. A daily email limit per project and per mailbox caps sending, and the DNS checker covers MX, SPF and DMARC. With a NeverBounce, MillionVerifier or Hunter key, each address is checked before its first email, and ones that don't exist fail instead of bouncing. Hunter also looks up the real address for a guessed one.
- **Costs.** Spend and cost per hot lead add up what Claude Code reports for each agent run and the cost per email you set on each mailbox. On a Claude subscription, the AI part is the equivalent API price.
- **Integrations.** Hot-lead alerts go out by email or to a Slack, Discord, Microsoft Teams, Google Chat or Telegram channel, or as a phone notification through ntfy, which also hear about booked meetings and reminders that come due. Calendly and Cal.com bookings mark the lead Meeting booked on their own, and inbox reminders show up in Google Calendar, Outlook or Apple Calendar through a private feed. Hot and booked leads go into HubSpot, Pipedrive, Attio or Close, with a note holding their reply. A webhook can also get every reply, booked meeting, unsubscribe, bounce, sent email and due follow-up. The Integrations page lists recent deliveries and can resend one. Leads import from CSV or from Hunter's list of people at a company, and export to CSV. A public REST API takes API keys.
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
| `APP_SECRET` | required | Encrypts mailbox passwords. Set a long random value (`openssl rand -hex 32`) before you connect a real mailbox, and keep it, because changing it makes the saved passwords unreadable. With an `https://` `PUBLIC_URL` the API won't start on the example value or anything shorter than 32 characters. |
| `PUBLIC_URL` | `http://localhost:3333` | Where the app is served. Used for unsubscribe, invite and inbox links in emails. An `https://` URL also makes the sign-in cookie HTTPS-only. |
| `BIND_ADDR` | `127.0.0.1:8787` | Address the API listens on. |
| `AGENT_MODEL` | `claude-sonnet-5-5` | Claude model for every agent run. |
| `AGENT_CONCURRENCY` | `2` | How many agent runs execute at once. |
| `SENDING_ENABLED` | `true` | `false` stops the sender, so approved emails, follow-ups and auto-replies wait. IMAP polling, agent runs and notification emails to your team still run. |
| `OPEN_SIGNUP` | `true` | `false` makes sign-up invite-only once the first account exists. Every account can run agents on your Claude subscription, so turn this off on a public server. |
| `PUBLIC_AGENT` | `true` | `false` turns off the home page demo, the company explorer and the support chat for signed-out visitors, which otherwise run the agent on your Claude subscription within hourly limits per visitor. |
| `BIRD_BIN` | unset | Path to the [bird](https://www.npmjs.com/package/@steipete/bird) CLI (`npm i -g @steipete/bird`). When set, research runs can search and read X. |
| `RUST_LOG` | unset | Log filter, e.g. `splee=debug,tower_http=info`. |
| `API_URL` (frontend, build time) | `http://127.0.0.1:8787` | Where the Next.js server proxies `/api/*`. `next build` bakes it in, so set it when you build, not when you start. |
| `NEXT_PUBLIC_SOURCE_URL` (frontend, build time) | `https://github.com/Iweisc/splee` | Where the "Source code" links in the footer and sidebar point. If you run a modified version for other people, the AGPL requires you to point this at your source. |

bird reads X with the cookies of a browser that is logged into x.com, so the agent sees X as that account. Pick the browser in `~/.config/bird/config.json5`.

## Running it for real

The quick start is for your own machine. To run Splee for a team:

1. **Postgres.** Use a database with its own strong password. The compose file's password is public and only meant for local development.
2. **Backend.** Build it with `(cd backend && cargo build --release)` and start it with `(cd backend && ./target/release/splee)` as a user whose `claude` CLI is signed in. It reads `.env` from the directory it starts in and applies migrations on start. Set `DATABASE_URL` to that database, `PUBLIC_URL` to the `https://` address people will use and `APP_SECRET` to the output of `openssl rand -hex 32`. With an `https://` `PUBLIC_URL` the API refuses the example secret and makes the sign-in cookie HTTPS-only. Set `OPEN_SIGNUP=false` too, so the first account can still sign up and everyone after it needs an invite, and `PUBLIC_AGENT=false` unless you want strangers to try the demo. Keep `BIND_ADDR` on loopback or a private network, because browsers only need to reach the frontend.
3. **Frontend.** Build it with `API_URL` pointing at the backend, `(cd frontend && pnpm install && API_URL=http://127.0.0.1:8787 pnpm build)`, then start it with `(cd frontend && pnpm start)` on port 3333.
4. **HTTPS.** Put a reverse proxy such as Caddy or nginx in front of port 3333 for `PUBLIC_URL`'s host. Only the frontend needs to be public, because it proxies `/api/*` to the backend. The proxy must set `X-Forwarded-For`, because the hourly limits on the public demo, explorer and support chat count each visitor by its last entry. Caddy does this by default, and nginx needs `proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;`. Without it, visitors can pick their own address and run up your Claude usage.
5. **Monitoring.** Point an uptime monitor at `https://<your host>/api/health`. It returns 200 when the frontend, the backend and Postgres all answer.
6. **Backups.** Back up the database and keep `APP_SECRET` with it. Mailbox passwords in a backup can't be decrypted without the secret they were saved with.
7. **Forgotten passwords.** There's no reset email. Run `(cd backend && ./target/release/splee reset-password person@example.com)`, which prints a new random password, signs that person out everywhere and clears their failed sign-ins. It's safe to run while the server is up. They can change the password under Account.

To start both on boot and restart them after a crash, adapt the example systemd units in [`deploy/systemd/`](deploy/systemd). Stopping the backend's unit also stops any `claude` processes it started. Run a single backend per database (see below), and if you change the code, point `NEXT_PUBLIC_SOURCE_URL` at your source as the AGPL requires.

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
