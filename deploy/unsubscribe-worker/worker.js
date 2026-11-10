// Takes unsubscribes for a Splee that isn't reachable from the internet. Each one waits in KV until pull.sh, running
// next to Splee, hands it to the API. Splee can't send while it's down either, and pull.sh runs every minute while it's up.
const TOKEN = "([A-Za-z0-9]+)";
const PAGE = new RegExp(`^/unsubscribe/${TOKEN}/?$`);
const ONE_CLICK = new RegExp(`^/api/unsubscribe/${TOKEN}$`);

export default {
  async fetch(request, env) {
    const { pathname } = new URL(request.url);
    const page = pathname.match(PAGE);
    // GET only shows the button, because mail security scanners open links and would unsubscribe people who never clicked.
    if (page && request.method === "GET") return html(CONFIRM);
    if (page && request.method === "POST") {
      await env.UNSUBSCRIBES.put(page[1], new Date().toISOString());
      return html(DONE);
    }
    const oneClick = pathname.match(ONE_CLICK);
    if (oneClick && request.method === "POST") {
      await env.UNSUBSCRIBES.put(oneClick[1], new Date().toISOString());
      return Response.json({ ok: true });
    }

    if (!env.PULL_SECRET || request.headers.get("authorization") !== `Bearer ${env.PULL_SECRET}`) return new Response("Not found", { status: 404 });
    if (pathname === "/api/unsubscribe" && request.method === "GET") {
      const { keys } = await env.UNSUBSCRIBES.list();
      return Response.json({ tokens: keys.map((k) => k.name) });
    }
    if (oneClick && request.method === "DELETE") {
      await env.UNSUBSCRIBES.delete(oneClick[1]);
      return new Response(null, { status: 204 });
    }
    return new Response("Not found", { status: 404 });
  },
};

function html(body) {
  return new Response(
    `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex"><title>Unsubscribe</title>
<style>body{font:16px/1.5 system-ui,sans-serif;display:grid;place-items:center;min-height:100vh;margin:0;padding:20px;box-sizing:border-box;color:#1a1a1a}
main{max-width:380px;text-align:center}h1{font-size:22px;font-weight:600;margin:0 0 8px}p{color:#555;margin:0 0 20px}
button{font:inherit;padding:10px 22px;border:0;border-radius:8px;background:#1a1a1a;color:#fff;cursor:pointer}</style></head>
<body><main>${body}</main></body></html>`,
    { headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" } },
  );
}

const CONFIRM = `<h1>Unsubscribe from these emails?</h1><p>You won't get any more emails from this sender.</p>
<form method="post"><button>Unsubscribe</button></form>`;
const DONE = `<h1>You're unsubscribed</h1><p>You won't get any more emails from this sender.</p>`;
