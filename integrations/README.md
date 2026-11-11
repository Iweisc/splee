# Splee for Zapier, Make and n8n

Each folder is an app for one tool, built on the same public API: `GET /api/v1/me` for the connection test, REST hook
subscriptions (`POST /api/v1/hooks`, `DELETE /api/v1/hooks/{id}`, `GET /api/v1/hooks/samples/{event}`) for the triggers, and the
lead, do-not-contact and owner endpoints for the actions. The API page in Splee documents all of them.

- [zapier](zapier) is a Zapier Platform CLI app.
- [make](make) is a Make custom app in the layout of Make's Apps Editor for VS Code.
- [n8n](n8n) is an n8n community node package.

Triggers: new hot reply, lead replied, meeting booked, lead status changed, deal won and new lead added. Actions: add or update a
lead, add to the do-not-contact list, set a lead's status, assign a lead's owner, and find a lead by email.

None of them is published. Their dependencies stay in their own folders, not in the frontend's.
