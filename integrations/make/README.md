# Splee for Make

A [Make custom app](https://developers.make.com/custom-apps-documentation) in the layout of Make's Apps Editor extension for VS
Code: `makecomapp.json` lists each component and the file behind each of its tabs.

- `general/base.iml.json`: the base URL and API key every call uses.
- `connections/splee`: the API key connection, tested with `GET /me`.
- `webhooks/splee`: the dedicated webhook. Attach subscribes it to the event picked (`POST /hooks`), detach unsubscribes it.
- `modules/watchEvents`: the instant trigger. The other modules are the actions and the search.
- `rpcs`: the Project and Campaign dropdowns.

## Publish

1. In Make, open Custom apps and create an app named Splee.
2. Install the Make Apps Editor extension in VS Code, add your Make API key and open this folder.
3. Put the key in `.secrets/apikey` (it's ignored by git), set `appId` under `origins` in `makecomapp.json` to your app's id, and
   deploy. Without the extension, paste each file into the tab of the same name in Make's web editor.
4. Share the app with an invite link, or ask Make to review it for its public list.

The connection asks for the base URL from the Integrations page in Splee, ending in `/api/v1`, and an API key.
