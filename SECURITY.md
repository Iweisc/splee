# Security policy

Splee stores mailbox credentials, sends email for its users and makes outbound HTTP requests. We take reports about any of that seriously.

## Reporting a vulnerability

Please don't open a public issue. Report the problem privately through [GitHub's private vulnerability reporting](https://github.com/Iweisc/splee/security/advisories/new).

Include what an attacker can do, the steps or a proof of concept, and the commit you tested. You should hear back within a week. We'll agree on a disclosure date with you once a fix is ready, and we'll credit you in the advisory unless you'd rather not be named.

## Supported versions

Splee has no releases yet. Fixes land on `main`, so run the latest `main`.

## Running Splee safely

- Set a long random `APP_SECRET`. It encrypts saved mailbox passwords.
- Keep the backend on a private address (`BIND_ADDR` defaults to `127.0.0.1:8787`) and expose only the Next.js app. It proxies `/api/*` to the backend.
- `docker-compose.yml` is for development. Its Postgres password is public, so it binds to loopback only. In production, use your own database credentials and keep Postgres off the public network.
- Serve the app over HTTPS. The session cookie is `HttpOnly` and `SameSite=Lax`.
- The agent runs the `claude` CLI under the backend's user account, so give that account only the access it needs.
