# Splee

A self-hosted clone of [explee.com](https://explee.com): paste your website, a Claude agent researches your company, competitors and buyer segments, finds companies and decision makers on the web, writes personal cold emails, sends them from your own mailboxes within a daily budget, reads replies over IMAP and answers interested leads with your booking link.

- `backend/`: Rust (Axum, sqlx, Postgres). The agent runs on the local `claude` CLI through `claude-agent-sdk-rs`.
- `frontend/`: Next.js 16. It proxies `/api/*` to the backend.
- `SPEC.md`: endpoints, data contract, agent pipeline and background workers.

## Run it

Requirements: Docker, Rust, pnpm, and the `claude` CLI signed in (`claude` must work in your shell).

From the repo root, each in its own terminal:

```sh
docker compose up -d                                  # Postgres on 127.0.0.1:55432
(cd backend && cp -n .env.example .env && cargo run)  # API on 127.0.0.1:8787, runs migrations
(cd frontend && pnpm install && pnpm dev)             # http://localhost:3333
```

Set a real `APP_SECRET` in `backend/.env` before connecting mailboxes. It encrypts their passwords. `AGENT_MODEL` picks the Claude model (default `claude-sonnet-5-5`). Card top-ups need `STRIPE_SECRET_KEY` and `STRIPE_WEBHOOK_SECRET`. `PAYMENTS_ENABLED=false` (the `.env.example` default) turns off balance checks and top-ups for local testing. Usage is still recorded in the ledger. `BIRD_BIN` points at the [bird](https://www.npmjs.com/package/@steipete/bird) CLI (`npm i -g @steipete/bird`) and lets research runs search and read X. bird uses the cookies of a browser that is logged into x.com, so the agent reads X as that account. Pick the browser in `~/.config/bird/config.json5`.

## Notes

- `backend/vendor/claude-agent-sdk-rs` is crates.io 0.6.4 with two patches. `src/query.rs` skips message types the crate doesn't know (newer CLIs emit `tool_progress`), which used to end the whole run. `src/internal/transport/subprocess.rs` spawns the CLI with `kill_on_drop`, so a timed-out call can't leave it running. Drop the `[patch.crates-io]` entry once upstream handles both.
- Every server process runs the background workers (agent runs, sender, IMAP poller). Run one backend per database.
