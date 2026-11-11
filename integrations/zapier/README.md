# Splee for Zapier

A [Zapier Platform CLI](https://docs.zapier.com/platform/build-cli/overview) app. Triggers use REST hooks, so a zap starts the
moment something happens in Splee, and their samples come from `GET /api/v1/hooks/samples/{event}`.

## Test

```sh
npm install
npm test
```

The tests fake `z.request`, so they run without network, and validate the app against Zapier's schema.

## Publish

1. `npm install -g zapier-platform-cli`, then `zapier login`.
2. `zapier register "Splee"` once, which writes `.zapierapprc` (it's ignored by git).
3. `zapier push` uploads this version as private. Invite yourself with `zapier users:add` and try each trigger and action in a zap.
4. To list it in Zapier's app directory, submit it from the Zapier developer platform and follow its review.

People connect with the address they open Splee at and an API key from the API keys page.
