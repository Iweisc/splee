# Contributing to Splee

Thanks for helping. Bug reports, fixes, features and docs are all welcome. Before you put a lot of work into a new feature, please open an issue, so we can agree on the shape first.

## Set up

Follow [Quick start](README.md#quick-start). Keep `PAYMENTS_ENABLED=false` in `backend/.env` for local work.

Splee sends real email. Before you connect a real mailbox, use test addresses or a sink like [Mailpit](https://mailpit.axllent.org). Don't point a development backend at a database with live campaigns. Every backend process runs the sender, the IMAP poller and the agent queue. `SENDING_ENABLED=false` stops only the sender.

## Before you open a pull request

Run what CI runs:

```sh
(cd backend && cargo clippy --all-targets -- -D warnings && cargo test)
(cd frontend && pnpm lint && pnpm typecheck)
```

Then try the change in the running app. For a UI change, add a screenshot or short clip to the pull request.

## Conventions

- **Commit and PR titles** follow [Conventional Commits](https://www.conventionalcommits.org) and describe what a user gets, not what you edited. Write `fix: stop cancelled research from leaving a project stuck researching`, not `fix: add settle_research call to cancel route`. Pull requests are squash-merged, so the title becomes the commit on `main`.
- **One change per pull request.** Keep refactors apart from behavior changes.
- **The data contract** is `backend/migrations/`, `backend/src/models.rs` and `frontend/src/lib/types.ts`. Change all three together.
- **Migrations** are append-only. Add the next numbered file and never edit one that's already on `main`.
- **SQL** uses sqlx runtime queries (`sqlx::query_as::<_, T>(...)`), so you don't need a live database to build.
- **Rust formatting:** the backend isn't `rustfmt`-formatted and uses long lines on purpose. Don't run `cargo fmt` over the tree. Match the surrounding code.
- **Agent work** goes through `agent::enqueue(...)` from routes. Don't call Claude inside a request handler. The support chat is the one exception.
- **Next.js 16** has breaking changes from earlier versions. Check `frontend/node_modules/next/dist/docs/` before you rely on an API you remember.
- **Comments** explain why something is the way it is, for the next developer. Don't narrate what the code does.

`SPEC.md` describes the endpoints, the agent pipeline and the background workers. Update it when you change one of them.

## Reporting bugs

Open an issue with what you did, what you expected and what happened instead. Include your OS, how you run Splee, and any backend log lines around the failure. Remove email addresses, mailbox credentials and API keys from logs before you paste them.

Report security problems privately as described in [SECURITY.md](SECURITY.md), not in a public issue.

## License

By contributing, you agree that your contribution is licensed under the [AGPL-3.0](LICENSE), the same license as the rest of Splee.
