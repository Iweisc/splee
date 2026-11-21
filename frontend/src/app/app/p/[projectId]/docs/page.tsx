"use client";

import { useState } from "react";
import { useParams } from "next/navigation";
import { KeyRound, SearchX } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CodeBlock } from "@/components/app/settings/code-block";
import { Section, SettingsColumn } from "@/components/app/settings/layout";
import { SearchField } from "@/components/app/work/search-field";
import { MicroLabel } from "@/components/app/work/states";
import { LinkButton } from "@/components/ui/button";
import { Badge, type BadgeTone, Card, EmptyState } from "@/components/ui/misc";
import { useOrigin } from "@/hooks/use-origin";
import { type Endpoint, endpoints, HOOK_EVENTS, hookEndpoints } from "@/lib/api-reference";

const METHOD_TONES: Record<Endpoint["method"], BadgeTone> = { GET: "blue", POST: "green", PUT: "neutral", PATCH: "amber", DELETE: "red" };

function EndpointSection({ e, base }: { e: Endpoint; base: string }) {
  const curl = [
    `curl${e.method === "GET" ? "" : ` -X ${e.method}`} "${base}${e.path}"`,
    `  -H "Authorization: Bearer $SPLEE_API_KEY"`,
    ...(e.body ? [`  -H "Content-Type: application/json"`, `  -d '${e.body}'`] : []),
    ...(e.form ? [`  -F "${e.form}"`] : []),
  ].join(" \\\n");
  return (
    <Section
      title={
        <span className="flex items-center gap-2 font-mono text-sm">
          <Badge tone={METHOD_TONES[e.method]}>{e.method}</Badge>
          {/* Wraps on a phone, filling each line like the code below it. Firefox doesn't break it at the slashes, so
              without this it widened the page. */}
          <span className="min-w-0 break-all">{e.path.split("?")[0]}</span>
        </span>
      }
      description={e.summary}
    >
      {e.params && (
        <ul className="mb-4 space-y-1 text-sm">
          {e.params.map(([name, description]) => (
            <li key={name}>
              <code className="font-mono">{name}</code> <span className="text-muted-foreground">{description}</span>
            </li>
          ))}
        </ul>
      )}
      <div className="grid gap-3">
        <CodeBlock label="Request" code={curl} />
        <CodeBlock label="Response" code={e.response} />
      </div>
    </Section>
  );
}

/** How to call the API, above the endpoints. */
/** lib/api-spec.ts repeats these for the OpenAPI spec and llms.txt. Change them together. */
function Guides({ origin, base }: { origin: string; base: string }) {
  return (
    <>
      <Section title="Authentication" description="Send an API key from the API keys page as a Bearer token. Keys belong to the organization and see all of its projects.">
        <div className="space-y-3 text-sm">
          <div>
            <MicroLabel>Base URL</MicroLabel>
            <code className="mt-1 block font-mono">{base}</code>
          </div>
          <CodeBlock label="Header" code={"Authorization: Bearer splee_…"} />
          <p className="text-muted-foreground">
            Requests and responses are JSON. Errors return a status code and <code className="font-mono">{`{ "error": "message" }`}</code>. Paginated lists
            return <code className="font-mono">{`{ "items": [...], "total": n }`}</code>.
          </p>
        </div>
      </Section>

      <Section
        title="Retries"
        description="Send an Idempotency-Key with a POST, PATCH or DELETE and retrying it after a timeout is safe: Splee runs it once and answers every repeat with the first response for 24 hours."
      >
        <div className="space-y-3 text-sm">
          <CodeBlock label="Header" code={"Idempotency-Key: 4f9c2a7e-new-campaign"} />
          <p className="text-muted-foreground">
            Use a new key, up to 255 characters, for each new request. A replayed response carries{" "}
            <code className="font-mono">Idempotent-Replayed: true</code>. The same key with a different method, path or body is 422, and a repeat
            while the first request still runs is 409. Server errors aren&apos;t kept, so a retry after a 5xx runs again.
          </p>
        </div>
      </Section>

      <Section
        title="Rate limits"
        description="Each organization gets 10,000 requests an hour and 150 at once, across the API and the MCP server."
      >
        <p className="text-sm text-muted-foreground">
          Every response carries <code className="font-mono">RateLimit-Limit</code>, <code className="font-mono">RateLimit-Remaining</code> and{" "}
          <code className="font-mono">RateLimit-Reset</code>, the seconds until the hour starts again. Past either limit the answer is 429 with{" "}
          <code className="font-mono">Retry-After</code> in seconds.
        </p>
      </Section>

      <Section
        title="For tools and agents"
        description="Import every endpoint below into Postman, Insomnia or a code generator from the OpenAPI spec, or point an AI agent at llms.txt."
      >
        <div className="grid gap-3">
          <CodeBlock label="OpenAPI" code={`${origin}/openapi.json`} />
          <CodeBlock label="llms.txt" code={`${origin}/llms.txt`} />
        </div>
      </Section>
    </>
  );
}

function HooksIntro() {
  return (
    <Section
      title="Subscriptions for Zapier, Make and n8n"
      description="Subscribe a URL to a lead event and Splee posts each one to it as it happens, with the same JSON and signature as the webhooks on the Integrations page. A post that gets 410 Gone turns the subscription off, and revoking the key stops all of its subscriptions."
    >
      <MicroLabel>Events</MicroLabel>
      <ul className="mt-2 space-y-1 text-sm">
        {HOOK_EVENTS.map(([event, description]) => (
          <li key={event}>
            <code className="font-mono">{event}</code> <span className="text-muted-foreground">{description}</span>
          </li>
        ))}
      </ul>
    </Section>
  );
}

/** Whether the endpoint's method, path and summary hold every word of the search, in any order. */
function matches(e: Endpoint, words: string[]) {
  const text = `${e.method} ${e.path} ${e.summary}`.toLowerCase();
  return words.every((w) => text.includes(w));
}

export default function DocsPage() {
  const { projectId } = useParams<{ projectId: string }>();
  const origin = useOrigin();
  const base = `${origin}/api/v1`;
  const [query, setQuery] = useState("");
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const finding = words.length > 0;
  const found = endpoints(projectId).filter((e) => matches(e, words));
  const foundHooks = hookEndpoints(projectId).filter((e) => matches(e, words));

  return (
    <>
      <PageHeader
        title="API"
        description="Run campaigns from your own scripts or CRM: find companies and people, approve emails, and read replies and results."
        actions={
          <LinkButton href="/app/api-keys" variant="outline">
            <KeyRound className="size-4" /> API keys
          </LinkButton>
        }
      />
      <PageBody>
        <SettingsColumn>
          <SearchField value={query} onChange={setQuery} placeholder="Find an endpoint, like deals or reply" label="Find an endpoint" />
          {/* While finding, only the matching endpoints show, so the guides don't push the results down. */}
          {finding && found.length + foundHooks.length === 0 && (
            <Card>
              <EmptyState icon={<SearchX className="size-5" />} title="No endpoint matches">
                Try one word from a path, like leads or campaigns.
              </EmptyState>
            </Card>
          )}
          {!finding && <Guides origin={origin} base={base} />}
          {found.map((e) => (
            <EndpointSection key={e.method + e.path} e={e} base={base} />
          ))}
          {!finding && <HooksIntro />}
          {foundHooks.map((e) => (
            <EndpointSection key={e.method + e.path} e={e} base={base} />
          ))}
        </SettingsColumn>
      </PageBody>
    </>
  );
}
