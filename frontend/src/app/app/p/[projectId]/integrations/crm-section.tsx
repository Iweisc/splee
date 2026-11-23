"use client";

import { useEffect, useState } from "react";
import { Contact, MailPlus } from "lucide-react";
import { toast } from "sonner";
import { useSWRConfig } from "swr";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Field, Input, Select, Switch } from "@/components/ui/form";
import { useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import { CRM_NAMES, type CrmName, type ProjectSettings, type SignInCrm, signsIn } from "@/lib/types";
import { LastFailure, useConnection, submit } from "./shared";

const CRMS = {
  hubspot: {
    name: "HubSpot",
    record: "contacts",
    field: "Service key",
    hint: "In HubSpot: Settings → Integrations → Service Keys → Create, with the crm.objects.contacts.read and crm.objects.contacts.write scopes. An older private app's token works too.",
  },
  salesforce: {
    name: "Salesforce",
    record: "leads",
    signIn: true,
    hint: "A lead whose email is already in Salesforce updates that contact or lead instead. Their reply goes in a task.",
  },
  zoho: {
    name: "Zoho CRM",
    record: "leads",
    signIn: true,
    hint: "Hot and booked leads go to the Leads module, matched by email, in whichever Zoho data center your account is in.",
  },
  dynamics: {
    name: "Dynamics 365 Sales",
    record: "leads",
    signIn: true,
    hint: "A lead whose email is already in Dynamics 365 gets updated instead. Their reply goes in a note on the lead.",
  },
  pipedrive: {
    name: "Pipedrive",
    record: "people",
    field: "API token",
    hint: "In Pipedrive: your profile → Personal preferences → API, then copy your personal API token.",
  },
  attio: {
    name: "Attio",
    record: "people",
    field: "Access token",
    hint: "In Attio: Workspace settings → Developers → New access token, with read-write access to records and notes and read access to object configuration.",
  },
  close: {
    name: "Close",
    record: "contacts",
    field: "API key",
    hint: "In Close: Settings → Developer → API Keys → New API Key.",
  },
  folk: {
    name: "folk",
    record: "people",
    field: "API key",
    hint: "In folk: Settings → API → Create a new key.",
  },
  capsule: {
    name: "Capsule",
    record: "people",
    field: "Access token",
    hint: "In Capsule: My Preferences → API Authentication Tokens, then generate a new token.",
  },
  brevo: {
    name: "Brevo",
    record: "contacts",
    field: "API key",
    hint: "In Brevo: Settings → SMTP & API → API keys → Generate a new API key.",
  },
  zendesk_sell: {
    name: "Zendesk Sell",
    record: "contacts",
    field: "Access token",
    hint: "In Zendesk Sell: Settings → Integrations → OAuth → Access Tokens, then add a user token.",
  },
  lacrm: {
    name: "Less Annoying CRM",
    record: "contacts",
    field: "API key",
    hint: "In Less Annoying CRM: Settings → API, then generate a new API key.",
  },
  salesflare: {
    name: "Salesflare",
    record: "contacts",
    field: "API key",
    hint: "In Salesflare: Settings → API keys, then the + button. Notes go on the contact's account.",
  },
  affinity: {
    name: "Affinity",
    record: "people",
    field: "API key",
    hint: "In Affinity: Settings → API, then Generate an API key.",
  },
  freshsales: {
    name: "Freshsales",
    record: "contacts",
    field: "API key",
    account: { label: "Bundle alias", placeholder: "acme.myfreshworks.com" },
    hint: "In Freshsales: your profile → Profile Settings → API Settings, which shows both.",
  },
  copper: {
    name: "Copper",
    record: "people",
    field: "API key",
    account: { label: "Your Copper email", placeholder: "you@company.com" },
    hint: "In Copper: Settings → Integrations → API Keys → Generate API Key.",
  },
  insightly: {
    name: "Insightly",
    record: "contacts",
    field: "API key",
    account: { label: "API URL", placeholder: "https://api.na1.insightly.com/v3.1/" },
    hint: "In Insightly: your profile → User Settings → API, which shows both.",
  },
  twenty: {
    name: "Twenty",
    record: "people",
    field: "API key",
    hint: "In Twenty: Settings → APIs & Webhooks → Create API key.",
  },
  twenty_self: {
    name: "Twenty (self-hosted)",
    record: "people",
    field: "API key",
    account: { label: "Site URL", placeholder: "https://crm.acme.com" },
    hint: "In Twenty: Settings → APIs & Webhooks → Create API key.",
  },
  espocrm: {
    name: "EspoCRM",
    record: "contacts",
    field: "API key",
    account: { label: "Site URL", placeholder: "https://crm.acme.com" },
    hint: "In EspoCRM: Administration → API Users → Create, with API Key auth and a role that can create Contacts.",
  },
  odoo: {
    name: "Odoo",
    record: "contacts",
    field: "API key",
    account: { label: "Site URL", placeholder: "https://acme.odoo.com" },
    hint: "Odoo 19+. In Odoo: your avatar → My Preferences → Security → Add API Key.",
  },
  notion: {
    name: "Notion",
    record: "database rows",
    field: "Integration secret",
    account: { label: "Database link", placeholder: "https://www.notion.so/…" },
    hint: "From an internal integration, added to the database under ••• → Connections. It needs an Email column.",
  },
  airtable: {
    name: "Airtable",
    record: "table records",
    field: "Personal access token",
    account: { label: "Table link", placeholder: "https://airtable.com/app…/tbl…" },
    hint: "With data.records:read, data.records:write, data.recordComments:write and schema.bases:read. It needs an Email field.",
  },
  baserow: {
    name: "Baserow",
    record: "table rows",
    field: "Database token",
    account: { label: "Table link", placeholder: "https://baserow.io/database/…/table/…" },
    hint: "On baserow.io or your own server, with create, read and update. The table needs an Email field and a long text field called Notes.",
  },
  nocodb: {
    name: "NocoDB",
    record: "table rows",
    field: "API token",
    account: { label: "Table link", placeholder: "https://app.nocodb.com/#/…" },
    hint: "On app.nocodb.com or your own server, from Account settings → Tokens. The table needs an Email column and a long text column called Notes.",
  },
  grist: {
    name: "Grist",
    record: "table rows",
    field: "API key",
    account: { label: "Page link", placeholder: "https://docs.getgrist.com/…/p/…" },
    hint: "On getgrist.com or your own server, from Profile settings → API. The page's table needs text columns called Email and Notes.",
  },
  teable: {
    name: "Teable",
    record: "table records",
    field: "Personal access token",
    account: { label: "Table link", placeholder: "https://app.teable.ai/base/…/table/…" },
    hint: "On app.teable.ai or your own server, with record read, create and update. The table needs a text field called Email and a long text field called Notes.",
  },
} as const satisfies Record<CrmName, Crm>;

/** `account` is what a CRM needs besides the token to find the account, sent alongside it. A CRM with `signIn` connects by
 *  signing in instead. */
type Crm = TokenCrm | { name: string; record: string; hint: string; signIn: true };

type TokenCrm = { name: string; record: string; hint: string; field: string; account?: { label: string; placeholder: string } };

/** Sends the browser to the CRM's sign-in page. It comes back to this page with how it went, which CrmSection shows. */
async function signInToCrm(projectId: string, crm: SignInCrm, body: { sandbox?: boolean; org_url?: string }) {
  const { url } = await api<{ url: string }>(`/projects/${projectId}/integrations/${crm}/oauth`, { method: "POST", body });
  window.location.assign(url);
}

/** Every CRM the project syncs to, each tested and disconnected on its own, and a form to connect another. Tokens are
 *  write-only, and connecting checks the token with the CRM first. A CRM that signs in is only offered when the server has
 *  an app for it. */
export function CrmSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const { me } = useMe();
  const { mutate } = useSWRConfig();
  const connected = CRM_NAMES.filter((crm) => settings[`${crm}_connected`]);
  const open = CRM_NAMES.filter((crm) => !settings[`${crm}_connected`] && (!signsIn(crm) || me?.crm_sign_in.includes(crm)));
  const [picked, setPicked] = useState<CrmName>(open[0] ?? "hubspot");
  // Once the picked CRM connects it leaves the list, so the form moves on to the next one.
  const crm = open.includes(picked) ? picked : open[0];

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const done = params.get("crm");
    const failed = params.get("crm_error");
    if (!done && !failed) return;
    // Cleared first, so a reload doesn't show it again.
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    if (done && done in CRMS) {
      toast.success(`Connected to ${CRMS[done as CrmName].name}`);
      mutate(`/projects/${projectId}/settings`);
    }
  }, [mutate, projectId]);

  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Contact className="size-4" /> CRM
        </span>
      }
      description="Adds leads to each CRM you connect once they reply hot, book or talk with you, with a note holding their reply or the call or meeting notes. Splee matches by email, so people already there get updated, not duplicated."
    >
      {!!connected.length && (
        <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
          {connected.map((c) => (
            <ConnectedCrm key={c} projectId={projectId} settings={settings} crm={c} />
          ))}
        </ul>
      )}
      {crm && <CrmForm key={crm} projectId={projectId} crm={crm} label={connected.length ? "Connect another CRM" : "Connect a CRM"} pick={setPicked} open={open} />}
    </Section>
  );
}

function ConnectedCrm({ projectId, settings, crm }: { projectId: string; settings: ProjectSettings; crm: CrmName }) {
  const { name, record } = CRMS[crm];
  const { busy, call } = useConnection(projectId, crm);
  const status = useConnection(projectId, `${crm}/status`);
  const { pending, run } = useAction();
  const { mutate } = useSWRConfig();
  const key = `/projects/${projectId}/settings`;
  const site = signsIn(crm) ? settings[`${crm}_site`] : "";
  const reconnect = signsIn(crm) && settings[`${crm}_needs_reconnect`];
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? New hot leads and meetings won't be added to it.`)) call(undefined, `${name} disconnected`);
  };
  // A failed test is saved like a failed sync, so the settings are read again either way.
  const test = () =>
    run("test", async () => {
      try {
        await mutate(key, await api<ProjectSettings>(`/projects/${projectId}/integrations/${crm}/test`, { method: "POST" }), { revalidate: false });
      } catch (e) {
        await mutate(key);
        throw e;
      }
    }, `${name} is working`);
  const signInAgain = () =>
    run("reconnect", () => signInToCrm(projectId, crm as SignInCrm, { sandbox: settings.salesforce_sandbox, org_url: `https://${settings.dynamics_site}` }));
  return (
    <li className="px-3 py-2.5">
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-40 flex-1">
          <p className="text-sm font-medium">{name}</p>
          <p className="text-xs break-all text-muted-foreground">{site ? `Signed in to ${site}` : `Adds leads as ${record}.`}</p>
        </div>
        <div className="flex gap-2">
          {reconnect ? (
            <Button size="sm" onClick={signInAgain} loading={pending === "reconnect"}>
              Reconnect
            </Button>
          ) : (
            <Button size="sm" variant="outline" onClick={test} loading={pending === "test"}>
              Test
            </Button>
          )}
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        </div>
      </div>
      {crm === "salesforce" && (
        <Field label="Status for booked leads" hint="From your org's Lead Status picklist." className="mt-3 sm:w-80">
          <Select
            value={settings.salesforce_booked_status}
            disabled={status.busy}
            onChange={(e) => status.call({ status: e.target.value }, e.target.value ? `Booked leads become ${e.target.value}` : "Booked leads keep their status")}
          >
            <option value="">Leave it as it is</option>
            {settings.salesforce_statuses.map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </Select>
        </Field>
      )}
      <LastFailure error={settings[`${crm}_error`]} at={settings[`${crm}_failed_at`]} what="sync" />
    </li>
  );
}

/** Keyed by the CRM, so picking another one starts with empty fields. */
function CrmForm({ projectId, crm, label, pick, open }: { projectId: string; crm: CrmName; label: string; pick: (crm: CrmName) => void; open: CrmName[] }) {
  const c: Crm = CRMS[crm];
  const picker = (
    <Field label={label} className="sm:w-72">
      <Select value={crm} onChange={(e) => pick(e.target.value as CrmName)}>
        {open.map((c) => (
          <option key={c} value={c}>
            {CRMS[c].name}
          </option>
        ))}
      </Select>
    </Field>
  );
  if ("signIn" in c) return <SignInCrmForm projectId={projectId} crm={crm as SignInCrm} picker={picker} />;
  return <TokenCrmForm projectId={projectId} crm={crm} crmInfo={c} picker={picker} />;
}

function TokenCrmForm({ projectId, crm, crmInfo, picker }: { projectId: string; crm: CrmName; crmInfo: TokenCrm; picker: React.ReactNode }) {
  const { name, field, hint, account } = crmInfo;
  const [token, setToken] = useState("");
  const [accountId, setAccountId] = useState("");
  const { busy, call } = useConnection(projectId, crm);
  const connect = () => call({ token, account: accountId }, `Connected to ${name}`);
  return (
    <form onSubmit={submit(connect)} className="flex flex-col gap-4">
      {picker}
      <div className="flex flex-col gap-4 sm:flex-row">
        {account && (
          <Field label={account.label} className="sm:w-72">
            <Input placeholder={account.placeholder} autoComplete="off" value={accountId} onChange={(e) => setAccountId(e.target.value.trim())} />
          </Field>
        )}
        <Field label={field} hint={hint} className="flex-1">
          <div className="flex gap-2">
            {/* The label also wraps Connect, so name the input itself. */}
            <Input aria-label={field} type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
            <Button type="submit" loading={busy} disabled={!token || (!!account && !accountId)}>
              Connect
            </Button>
          </div>
        </Field>
      </div>
    </form>
  );
}

function SignInCrmForm({ projectId, crm, picker }: { projectId: string; crm: SignInCrm; picker: React.ReactNode }) {
  const { name, hint } = CRMS[crm];
  const [orgUrl, setOrgUrl] = useState("");
  const [sandbox, setSandbox] = useState(false);
  const { pending, run } = useAction();
  const signIn = () => run("sign-in", () => signInToCrm(projectId, crm, { sandbox, org_url: orgUrl }));
  return (
    <form onSubmit={submit(signIn)} className="flex flex-col gap-4">
      {picker}
      {crm === "dynamics" && (
        <Field label="Org URL" hint="The address Dynamics 365 opens at." className="sm:w-96">
          <Input placeholder="https://acme.crm.dynamics.com" autoComplete="off" value={orgUrl} onChange={(e) => setOrgUrl(e.target.value.trim())} />
        </Field>
      )}
      {crm === "salesforce" && (
        <div className="flex items-center gap-3">
          <Switch checked={sandbox} onChange={setSandbox} label="Sandbox org" />
          <span>
            <span className="block text-sm">Sandbox org</span>
            <span className="block text-xs text-muted-foreground">Signs in at test.salesforce.com</span>
          </span>
        </div>
      )}
      <div className="flex flex-col gap-2">
        <Button type="submit" loading={pending === "sign-in"} disabled={crm === "dynamics" && !orgUrl} className="self-start">
          Sign in to {name}
        </Button>
        <p className="text-xs text-pretty text-muted-foreground">{hint}</p>
      </div>
    </form>
  );
}

export function BccSection({ settings: s, update }: { settings: ProjectSettings; update: (patch: Partial<ProjectSettings>) => void }) {
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <MailPlus className="size-4" /> BCC copies
        </span>
      }
      description="Copies every email Splee sends to one address, so your CRM logs the conversation."
    >
      <Field label="BCC address" hint="HubSpot, Pipedrive, Close, Capsule and most other CRMs give you one. Leave it empty for no copies.">
        <Input type="email" autoComplete="off" value={s.bcc_email} onChange={(e) => update({ bcc_email: e.target.value.trim() })} placeholder="1234567@bcc.hubspot.com" />
      </Field>
      <LastFailure error={s.bcc_error} at={s.bcc_failed_at} what="copy" />
    </Section>
  );
}
