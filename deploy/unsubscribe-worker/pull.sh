#!/bin/sh
# Hands the unsubscribes the Worker queued to the local Splee API. Run it every minute with splee-unsubscribe-pull.timer.
set -eu
: "${UNSUBSCRIBE_URL:?}" "${PULL_SECRET:?}"
api=${SPLEE_API:-http://127.0.0.1:8787}
auth="Authorization: Bearer $PULL_SECRET"
for token in $(curl -fsS -H "$auth" "$UNSUBSCRIBE_URL/api/unsubscribe" | jq -r '.tokens[]'); do
  status=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$api/api/unsubscribe/$token")
  # A 404 is a link Splee doesn't know, like a test email's, so retrying won't help. Anything else waits for the next run.
  case $status in
    2?? | 404) curl -fsS -o /dev/null -X DELETE -H "$auth" "$UNSUBSCRIBE_URL/api/unsubscribe/$token" ;;
  esac
done
