# n8n-nodes-splee

An [n8n community node](https://docs.n8n.io/integrations/community-nodes/) package with a Splee credential, a Splee Trigger node
and a Splee node.

- The trigger subscribes its webhook URL to one event when the workflow is activated (`POST /hooks`) and unsubscribes it when the
  workflow is deactivated (`DELETE /hooks/{id}`).
- The Splee node adds or updates leads, sets their status or owner, finds them by email and adds to the do-not-contact list.

## Build

```sh
npm install
npm run build
```

## Publish

1. `npm publish --dry-run` shows what goes into the package. `npm publish` then puts it on npm.
2. Self-hosted n8n can then install it under Settings, Community nodes.
3. To show it in n8n Cloud, submit it for verification from the n8n creator portal.

The credential asks for the base URL from the Integrations page in Splee and an API key.
