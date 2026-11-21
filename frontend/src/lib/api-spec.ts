import { endpoints, HOOK_EVENTS, hookEndpoints, type Endpoint } from "./api-reference";

// The API docs for tools and agents: an OpenAPI spec for Postman and code generators, and llms.txt for AI agents.
// Both come from the docs page's endpoints, so a call documented there shows up here with nothing else to update.

/** The docs fill paths with example ids. Built with this as the project id, project paths come out as templates. */
const PROJECT_ID = "{project_id}";

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
type Schema = { [key: string]: unknown };

/** "/campaigns/5d0a…" names its id campaign_id. Ids are written like 5d0a…, and the segment before says what of. */
function idName(of: string) {
  const one = of.endsWith("ies") ? `${of.slice(0, -3)}y` : /(ch|sh|ss|x)es$/.test(of) ? of.slice(0, -2) : of.replace(/s$/, "");
  return `${one.replaceAll("-", "_")}_id`;
}

/** Path parameters the rule above can't name: /dedup/companies/6e1b… is a dedup list, and hot_lead is an example event. */
const PARAMS: Record<string, string> = { "/dedup/companies": "dedup_list_id", "/hooks/samples": "event" };

/** The path as an OpenAPI template, like /campaigns/{campaign_id}/start, without its example query. */
function template(path: string) {
  const segments = path.split("?")[0]!.split("/");
  return segments
    .map((s, i) => {
      const param = PARAMS[segments.slice(0, i).join("/")] ?? (s.endsWith("…") ? idName(segments[i - 1]!) : undefined);
      return param ? `{${param}}` : s;
    })
    .join("/");
}

const FIELD = /^[a-z_][a-z0-9_]*$/;

/** The docs list fields together, like "page, per_page" or "stage_id or stage". Notes on syntax, like {a|b}, aren't fields. */
function fields(params: Endpoint["params"] = []) {
  const named = new Map<string, string>();
  const notes: string[] = [];
  for (const [names, description] of params) {
    const split = names.split(/, | or /);
    if (split.every((n) => FIELD.test(n))) for (const n of split) named.set(n, description);
    else notes.push(`\`${names}\`: ${description}`);
  }
  return { named, notes };
}

/** An example from the docs, without the "…": "…" they write for the fields they leave out. */
function example(text: string): Json {
  return JSON.parse(text, (key, value: Json) => (key === "…" ? undefined : value)) as Json;
}

/** A schema read off an example. Every number is a "number", since an example 0 can't tell a count from a rate, and null allows anything. */
function schemaOf(value: Json): Schema {
  if (value === null) return {};
  if (Array.isArray(value)) return { type: "array", items: value.length > 0 ? value.map(schemaOf).reduce(merge) : {} };
  if (typeof value === "object") return { type: "object", properties: Object.fromEntries(Object.entries(value).map(([key, v]) => [key, schemaOf(v)])) };
  return { type: typeof value };
}

/** One schema for two items of a list: the fields of both, with no type where they disagree, like a string and a null. */
function merge(a: Schema, b: Schema): Schema {
  if (a.type !== b.type) return {};
  if (a.type === "array") return { type: "array", items: merge(a.items as Schema, b.items as Schema) };
  if (a.type !== "object") return a;
  const [pa, pb] = [a.properties, b.properties] as Record<string, Schema>[];
  const keys = new Set([...Object.keys(pa!), ...Object.keys(pb!)]);
  return { type: "object", properties: Object.fromEntries([...keys].map((k) => [k, pa![k] && pb![k] ? merge(pa![k], pb![k]) : (pa![k] ?? pb![k])])) };
}

/** The first sentence, as a title for the operation. */
const firstSentence = (text: string) => text.split(/(?<=\.)\s/)[0]!.replace(/\.$/, "");

function operation(e: Endpoint, path: string) {
  const { named, notes } = fields(e.params);
  const query = new URLSearchParams(e.path.split("?")[1] ?? "");
  // A GET or DELETE has no body, so every field it lists goes in the query. The others take theirs in the body.
  const inQuery = new Set([...query.keys(), ...(e.method === "GET" || e.method === "DELETE" ? named.keys() : [])]);
  const parameters: Schema[] = [...path.matchAll(/\{(\w+)\}/g)].map(([, name]) => ({ name, in: "path", required: true, schema: { type: "string" } }));
  for (const name of inQuery) {
    parameters.push({ name, in: "query", description: named.get(name), schema: { type: "string" }, example: query.get(name) ?? undefined });
  }
  if (e.method !== "GET") parameters.push({ $ref: "#/components/parameters/IdempotencyKey" });

  const op: Schema = {
    operationId: `${e.method.toLowerCase()}${path.replace(/[^a-zA-Z0-9]+/g, "_").replace(/_$/, "")}`,
    summary: firstSentence(e.summary),
    description: [e.summary, ...notes.map((n) => `- ${n}`)].join("\n\n"),
    parameters,
    responses: { "200": { description: "OK", content: { [e.responseType ?? "application/json"]: media(e) } }, "4XX": { $ref: "#/components/responses/Error" } },
  };
  if (e.body) {
    const body = example(e.body);
    const schema = schemaOf(body);
    const properties = (schema.properties ?? {}) as Record<string, Schema>;
    for (const [name, description] of named) if (!inQuery.has(name)) properties[name] = { ...properties[name], description };
    op.requestBody = { required: true, content: { "application/json": { schema, example: body } } };
  }
  if (e.form) {
    const name = e.form.split("=")[0]!;
    const schema = { type: "object", properties: { [name]: { type: "string", format: "binary" } }, required: [name] };
    op.requestBody = { required: true, content: { "multipart/form-data": { schema } } };
  }
  return op;
}

function media(e: Endpoint) {
  if (e.responseType === "application/octet-stream") return { schema: { type: "string", format: "binary" } };
  if (e.responseType) return { schema: { type: "string" }, example: e.response };
  const response = example(e.response);
  return { schema: schemaOf(response), example: response };
}

const INTRO = "Run campaigns from your own scripts or CRM: find companies and people, approve emails, and read replies and results.";

const HOOKS_INTRO =
  "Subscribe a URL to a lead event and Splee posts each one to it as it happens, with the same JSON and signature as the webhooks on the Integrations page. A post that gets 410 Gone turns the subscription off, and revoking the key stops all of its subscriptions.";

/** What the docs page's Authentication, Retries and Rate limits sections say. Change them together. */
function guide(base: string) {
  return `## Authentication

- Send an API key from the API keys page as a Bearer token: \`Authorization: Bearer splee_…\`. Keys belong to the organization and see all of its projects.
- Base URL: \`${base}\`
- Requests and responses are JSON. Errors return a status code and \`{ "error": "message" }\`. Paginated lists return \`{ "items": [...], "total": n }\`.

## Retries

- Send an \`Idempotency-Key\` with a POST, PATCH or DELETE and retrying it after a timeout is safe: Splee runs it once and answers every repeat with the first response for 24 hours.
- Use a new key, up to 255 characters, for each new request. A replayed response carries \`Idempotent-Replayed: true\`. The same key with a different method, path or body is 422, and a repeat while the first request still runs is 409. Server errors aren't kept, so a retry after a 5xx runs again.

## Rate limits

- Each organization gets 10,000 requests an hour and 150 at once, across the API and the MCP server.
- Every response carries \`RateLimit-Limit\`, \`RateLimit-Remaining\` and \`RateLimit-Reset\`, the seconds until the hour starts again. Past either limit the answer is 429 with \`Retry-After\` in seconds.`;
}

const all = () => [...endpoints(PROJECT_ID), ...hookEndpoints(PROJECT_ID)];

/** The OpenAPI 3.1 spec of the v1 API, served at /openapi.json. */
export function openapi(origin: string) {
  const base = `${origin}/api/v1`;
  const paths: Record<string, Record<string, Schema>> = {};
  for (const e of all()) {
    const path = template(e.path);
    (paths[path] ??= {})[e.method.toLowerCase()] = operation(e, path);
  }
  return {
    openapi: "3.1.0",
    info: { title: "Splee API", version: "1", description: `${INTRO}\n\n${guide(base)}\n\n## Subscriptions for Zapier, Make and n8n\n\n${HOOKS_INTRO}` },
    servers: [{ url: base }],
    security: [{ apiKey: [] }],
    components: {
      securitySchemes: { apiKey: { type: "http", scheme: "bearer", description: "An API key from the API keys page, like splee_…" } },
      parameters: {
        IdempotencyKey: {
          name: "Idempotency-Key",
          in: "header",
          description: "Makes a retry safe: Splee runs the request once and answers every repeat with the first response for 24 hours.",
          schema: { type: "string", maxLength: 255 },
        },
      },
      responses: {
        Error: {
          description: "An error, with a status code like 400, 401, 404, 409, 422 or 429",
          content: { "application/json": { schema: { type: "object", properties: { error: { type: "string" } }, required: ["error"] } } },
        },
      },
    },
    paths,
  };
}

/** One endpoint as a line of llms.txt, with its example query, since some are required, and its fields and body under it. */
function entry(e: Endpoint) {
  const query = e.path.split("?")[1];
  const lines = [`- \`${e.method} ${template(e.path)}${query ? `?${query}` : ""}\`: ${e.summary}`];
  for (const [names, description] of e.params ?? []) lines.push(`  - \`${names}\`: ${description}`);
  if (e.body) lines.push(`  - Body: \`${e.body.replace(/\s*\n\s*/g, " ")}\``);
  if (e.form) lines.push(`  - Multipart form: \`${e.form}\``);
  return lines.join("\n");
}

/** The API for AI agents, served at /llms.txt. */
export function llmsTxt(origin: string) {
  return `# Splee API

> ${INTRO}

${guide(`${origin}/api/v1`)}

## For agents

- [OpenAPI spec](${origin}/openapi.json): every endpoint below with its parameters and an example response.
- [MCP server](${origin}/api/mcp): Splee as tools for Claude Code, Cursor or any MCP client. Send an API key as a Bearer token, or sign in with OAuth from ChatGPT or Claude.ai.

## Endpoints

${endpoints(PROJECT_ID).map(entry).join("\n")}

## Subscriptions for Zapier, Make and n8n

${HOOKS_INTRO}

Events:

${HOOK_EVENTS.map(([event, description]) => `- \`${event}\`: ${description}`).join("\n")}

${hookEndpoints(PROJECT_ID).map(entry).join("\n")}
`;
}

/** The address a request came to, as the caller sees it when a proxy sits in front of Splee. */
export function originOf(request: Request) {
  const url = new URL(request.url);
  const first = (header: string) => request.headers.get(header)?.split(",")[0]?.trim();
  return `${first("x-forwarded-proto") || url.protocol.slice(0, -1)}://${first("x-forwarded-host") || url.host}`;
}
