"use client";

import { useState } from "react";
import Link from "next/link";
import useSWR from "swr";
import { toast } from "sonner";
import { Check, Globe, Plus, RefreshCw, Search, Trash2, X } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { DnsBadge } from "@/components/app/settings/dns-badge";
import { LoadError, Section, SectionsSkeleton, SettingsColumn } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Field, Input, Select, Textarea } from "@/components/ui/form";
import { Badge, Card, Host, Message, Modal, type BadgeTone } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { DomainAvailability, DomainRecord, MailProvider, ProjectSummary, RegistrarAccount, SendingDomain } from "@/lib/types";
import { cn, money, normalizeDomain, plural } from "@/lib/utils";

/** What each provider still needs after Splee set the records it knows in advance. */
const PROVIDERS: Record<MailProvider, { label: string; next: string }> = {
  google: {
    label: "Google Workspace",
    next: "Add the domain in the Google Admin console and add the TXT record it verifies with here. Then generate a DKIM key under Apps, Gmail, Authenticate email and add its TXT record here.",
  },
  microsoft: {
    label: "Microsoft 365",
    next: "Add the domain in the Microsoft 365 admin center and check its MX value matches the one here. Then turn on DKIM in Microsoft Defender and add its two CNAME records here.",
  },
  zoho: {
    label: "Zoho Mail",
    next: "Add the domain in the Zoho Mail admin console and add the TXT record it verifies with here. Then add a DKIM selector under Email configuration and add its TXT record here.",
  },
  zoho_eu: {
    label: "Zoho Mail (EU)",
    next: "Add the domain in the Zoho Mail admin console and add the TXT record it verifies with here. Then add a DKIM selector under Email configuration and add its TXT record here.",
  },
  migadu: {
    label: "Migadu",
    next: "Splee set Migadu's DKIM records already. Add the domain in Migadu and add the TXT record it verifies with here.",
  },
  other: {
    label: "Another provider",
    next: "Add your provider's MX, SPF and DKIM records here. Splee set DMARC and the forward to your site.",
  },
};

const BUSY = (d: SendingDomain) => d.status === "queued" || d.status === "registering" || d.dns_status === "setting";

function status(d: SendingDomain): { tone: BadgeTone; label: string } {
  if (d.status === "queued") return { tone: "neutral", label: "Waiting" };
  if (d.status === "registering") return { tone: "blue", label: "Buying" };
  if (d.status === "uncertain") return { tone: "amber", label: "Unclear" };
  if (d.status === "failed") return { tone: "red", label: "Not bought" };
  if (d.dns_status === "setting") return { tone: "blue", label: "Setting DNS" };
  if (d.dns_status === "error") return { tone: "amber", label: "DNS incomplete" };
  if (d.warming > 0) return { tone: "green", label: "Warmup on" };
  if (d.mailboxes > 0) return { tone: "green", label: "Mailbox connected" };
  return { tone: "green", label: "DNS set" };
}

/** "Nov 10, 2027" from "2027-11-10", read as a calendar day so no time zone moves it. */
const day = (iso: string) => new Date(`${iso}T12:00:00`).toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });

export default function SendingDomainsPage() {
  const { data: account, error: accountError, mutate: mutateAccount } = useSWR<RegistrarAccount | null>("/registrar", fetcher);
  // Polled while a purchase or DNS setup is under way, which runs in the background.
  const { data: domains, mutate } = useSWR<SendingDomain[]>("/sending-domains", fetcher, {
    refreshInterval: (list) => (list?.some(BUSY) ? 2_000 : 0),
  });

  return (
    <>
      <PageHeader
        title="Domains"
        description="Send cold email from domains that look like yours, so your main domain's reputation stays safe. Splee finds them, buys them through your Porkbun account and sets up their DNS."
      />
      <PageBody>
        <SettingsColumn>
          {accountError ? (
            <LoadError error={accountError} what="your registrar" retry={() => mutateAccount()} />
          ) : account === undefined ? (
            <SectionsSkeleton />
          ) : (
            <>
              <Registrar account={account} onChange={(a) => mutateAccount(a, { revalidate: false })} />
              {account && <FindDomains owned={domains ?? []} onBought={(list) => mutate(list, { revalidate: false })} />}
              {domains && domains.length > 0 && <DomainList domains={domains} mutate={mutate} />}
            </>
          )}
        </SettingsColumn>
      </PageBody>
    </>
  );
}

function Registrar({ account, onChange }: { account: RegistrarAccount | null; onChange: (a: RegistrarAccount | null) => void }) {
  const [key, setKey] = useState("");
  const [secret, setSecret] = useState("");
  const { pending, run } = useAction();

  if (account) {
    const disconnect = () => {
      if (!confirm("Disconnect Porkbun? Domains you bought stay in your Porkbun account and keep their DNS.")) return;
      run("disconnect", async () => {
        await api("/registrar", { method: "DELETE" });
        onChange(null);
      });
    };
    return (
      <Section
        title="Porkbun"
        description={
          <>
            Connected as <span className="font-medium text-foreground">{account.account || "your account"}</span>. Buying a domain charges your Porkbun
            account credit.
          </>
        }
        actions={
          <Button variant="outline" size="sm" onClick={disconnect} loading={pending === "disconnect"}>
            Disconnect
          </Button>
        }
      />
    );
  }

  const connect = (e: React.FormEvent) => {
    e.preventDefault();
    run("connect", async () => onChange(await api<RegistrarAccount>("/registrar", { method: "PUT", body: { api_key: key, secret } })), "Porkbun connected");
  };
  return (
    <Section title="Connect Porkbun" description="Splee checks prices, buys domains and sets their DNS through your Porkbun account.">
      <form onSubmit={connect} className="grid gap-4">
        <ol className="grid list-decimal gap-1 pl-5 text-sm text-pretty text-muted-foreground">
          <li>
            Create an API key at{" "}
            <a href="https://porkbun.com/account/api" target="_blank" rel="noreferrer" className="font-medium text-foreground underline-offset-4 hover:underline">
              porkbun.com/account/api
            </a>
            .
          </li>
          <li>On the same page, opt all domains in to API access, so the domains Splee buys can get their DNS.</li>
          <li>Add account credit. Porkbun pays for domains from it, never from a saved card on its own.</li>
        </ol>
        <div className="grid gap-4 sm:grid-cols-2">
          <Field label="API key">
            <Input value={key} onChange={(e) => setKey(e.target.value)} placeholder="pk1_…" autoComplete="off" spellCheck={false} />
          </Field>
          <Field label="Secret API key">
            <Input type="password" value={secret} onChange={(e) => setSecret(e.target.value)} placeholder="sk1_…" autoComplete="off" />
          </Field>
        </div>
        <p className="rounded-lg bg-amber-50 px-3 py-2 text-xs text-pretty text-amber-800">
          Anyone on your team who is an admin can buy domains with this key, and each purchase charges your Porkbun account credit.
        </p>
        <div>
          <Button type="submit" loading={pending === "connect"} disabled={!key.trim() || !secret.trim()}>
            Connect Porkbun
          </Button>
        </div>
      </form>
    </Section>
  );
}

function FindDomains({ owned, onBought }: { owned: SendingDomain[]; onBought: (list: SendingDomain[]) => void }) {
  const { data: projects } = useSWR<ProjectSummary[]>("/projects", fetcher);
  // The project the sidebar was last on, like the shell picks it, so the search starts from that company's domain.
  const [typed, setTyped] = useState<string | null>(null);
  const last = typeof window === "undefined" ? null : localStorage.getItem("splee:last-project");
  const projectDomain = (projects?.find((p) => p.id === last) ?? projects?.find((p) => p.domain))?.domain ?? "";
  const query = typed ?? projectDomain;
  const [results, setResults] = useState<{ domain: string; items: DomainAvailability[] } | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [buying, setBuying] = useState(false);
  const { pending, run } = useAction();

  const search = (e: React.FormEvent) => {
    e.preventDefault();
    const domain = normalizeDomain(query);
    run("search", async () => {
      const items = await api<DomainAvailability[]>(`/sending-domains/suggestions?domain=${encodeURIComponent(domain)}`);
      setResults({ domain, items });
      setPicked(new Set());
    });
  };

  const ours = new Set(owned.filter((d) => d.status !== "failed").map((d) => d.domain));
  const chosen = results?.items.filter((a) => picked.has(a.domain)) ?? [];
  const total = chosen.reduce((sum, a) => sum + a.price_micros, 0);
  const toggle = (domain: string) =>
    setPicked((p) => {
      const next = new Set(p);
      if (!next.delete(domain)) next.add(domain);
      return next;
    });

  return (
    <Section title="Find sending domains" description="Lookalikes of your company's domain, with what Porkbun charges for the first year.">
      <form onSubmit={search} className="flex gap-2">
        <Input value={query} onChange={(e) => setTyped(e.target.value)} placeholder="acme.com" aria-label="Your company's domain" className="min-w-0 flex-1" />
        <Button type="submit" variant="outline" loading={pending === "search"} disabled={!normalizeDomain(query)}>
          <Search className="size-4" /> Search
        </Button>
      </form>

      {results && (
        <>
          <div className="mt-4 divide-y divide-border rounded-xl border border-border">
            {results.items.map((a) => {
              const mine = ours.has(a.domain);
              const buyable = a.available === true && !a.premium && !mine && a.price_micros > 0;
              return (
                <label
                  key={a.domain}
                  className={cn("flex items-center gap-3 px-4 py-2.5 text-sm", buyable ? "cursor-pointer hover:bg-accent/60" : "text-muted-foreground")}
                >
                  <input
                    type="checkbox"
                    checked={picked.has(a.domain)}
                    onChange={() => toggle(a.domain)}
                    disabled={!buyable}
                    className="block shrink-0 accent-black"
                  />
                  <span className={cn("min-w-0 flex-1", buyable && "font-medium")}>
                    <Host host={a.domain} />
                  </span>
                  <span className="shrink-0 text-right whitespace-nowrap">
                    {mine ? (
                      <Badge>Yours</Badge>
                    ) : a.available === null ? (
                      "No answer, search again"
                    ) : !a.available ? (
                      "Taken"
                    ) : a.premium ? (
                      <span title="Porkbun doesn't sell premium names through its API">Premium</span>
                    ) : (
                      <>
                        <span className="font-medium tabular-nums">{money(a.price_micros)}</span>
                        {a.renewal_micros > 0 && a.renewal_micros !== a.price_micros && (
                          <span className="text-xs text-muted-foreground max-sm:hidden"> then {money(a.renewal_micros)}/yr</span>
                        )}
                      </>
                    )}
                  </span>
                </label>
              );
            })}
          </div>
          <div className="mt-4 flex flex-wrap items-center justify-between gap-3">
            <p className="text-sm text-muted-foreground">
              {chosen.length ? (
                <>
                  {plural(chosen.length, "domain")} picked · <span className="font-medium text-foreground tabular-nums">{money(total)}</span>
                </>
              ) : (
                "Two or three domains with a couple of mailboxes each is plenty to start."
              )}
            </p>
            <Button onClick={() => setBuying(true)} disabled={!chosen.length}>
              Buy {chosen.length > 1 ? plural(chosen.length, "domain") : "domain"}
            </Button>
          </div>
        </>
      )}

      <Modal open={buying} onClose={() => setBuying(false)} title={`Buy ${plural(chosen.length, "domain")}`} className="max-w-xl">
        {buying && (
          <BuyForm
            chosen={chosen}
            total={total}
            site={results?.domain ?? ""}
            onBought={(list, skipped) => {
              onBought(list);
              setBuying(false);
              setPicked(new Set());
              if (skipped.length) toast.warning(`Splee already has ${skipped.join(", ")}, so it wasn't bought again`);
              else toast.success("Buying now. Each domain gets its DNS once Porkbun confirms it.");
            }}
          />
        )}
      </Modal>
    </Section>
  );
}

function BuyForm({
  chosen,
  total,
  site,
  onBought,
}: {
  chosen: DomainAvailability[];
  total: number;
  site: string;
  onBought: (list: SendingDomain[], skipped: string[]) => void;
}) {
  const [provider, setProvider] = useState<MailProvider>("google");
  const [forward, setForward] = useState(site ? `https://${site}` : "");
  const [dmarc, setDmarc] = useState("");
  const { pending, run } = useAction();

  const buy = (e: React.FormEvent) => {
    e.preventDefault();
    run("buy", async () => {
      const body = { domains: chosen.map((a) => ({ domain: a.domain, price_micros: a.price_micros })), provider, forward_to: forward, dmarc_email: dmarc };
      const res = await api<{ domains: SendingDomain[]; skipped: string[] }>("/sending-domains", { body });
      onBought(res.domains, res.skipped);
    });
  };

  return (
    <form onSubmit={buy} className="grid gap-4">
      <div className="divide-y divide-border rounded-xl border border-border text-sm">
        {chosen.map((a) => (
          <div key={a.domain} className="flex items-center justify-between gap-3 px-4 py-2">
            <span className="min-w-0">
              <Host host={a.domain} />
            </span>
            <span className="tabular-nums">{money(a.price_micros)}</span>
          </div>
        ))}
        <div className="flex items-center justify-between gap-3 px-4 py-2 font-medium">
          <span>Total</span>
          <span className="tabular-nums">{money(total)}</span>
        </div>
      </div>
      <Field label="Mail provider" hint="Splee sets its MX and SPF records. You create the mailboxes there.">
        <Select value={provider} onChange={(e) => setProvider(e.target.value as MailProvider)}>
          {Object.entries(PROVIDERS).map(([value, p]) => (
            <option key={value} value={value}>
              {p.label}
            </option>
          ))}
        </Select>
      </Field>
      <Field label="Forward visitors to" hint="Someone who opens one of these domains lands on your main site.">
        <Input value={forward} onChange={(e) => setForward(e.target.value)} placeholder="https://acme.com" />
      </Field>
      <Field label="DMARC reports to" hint="Optional. Mail providers send daily reports on mail from these domains here.">
        <Input type="email" value={dmarc} onChange={(e) => setDmarc(e.target.value)} placeholder="dmarc@acme.com" />
      </Field>
      <p className="rounded-lg bg-amber-50 px-3 py-2 text-sm text-pretty text-amber-800">
        Porkbun takes {money(total)} from your account credit now, and a bought domain can&apos;t be returned. It renews at its yearly price in your
        Porkbun account.
      </p>
      <div className="flex justify-end">
        <Button type="submit" loading={pending === "buy"}>
          Buy for {money(total)}
        </Button>
      </div>
    </form>
  );
}

function DomainList({ domains, mutate }: { domains: SendingDomain[]; mutate: () => Promise<unknown> }) {
  const { pending, run } = useAction();
  const [adding, setAdding] = useState<SendingDomain | null>(null);

  const call = (key: string, d: SendingDomain, path: string, success?: string) =>
    run(`${key}:${d.id}`, async () => {
      try {
        await api(`/sending-domains/${d.id}${path}`, { method: "POST" });
      } finally {
        await mutate();
      }
    }, success);

  const remove = (d: SendingDomain) => {
    const note = d.status === "registered" ? " It stays in your Porkbun account with its DNS." : "";
    if (!confirm(`Remove ${d.domain} from Splee?${note}`)) return;
    run(`delete:${d.id}`, async () => {
      await api(`/sending-domains/${d.id}`, { method: "DELETE" });
      await mutate();
    });
  };

  return (
    <div className="grid gap-3">
      <h2 className="mt-2 font-semibold">Your sending domains</h2>
      {domains.map((d) => {
        const s = status(d);
        const bought = d.status === "registered";
        return (
          <Card key={d.id} className="min-w-0 p-5">
            <div className="flex flex-wrap items-start justify-between gap-x-4 gap-y-3">
              <div className="min-w-0">
                <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                  <p className="min-w-0 font-medium">
                    <Host host={d.domain} />
                  </p>
                  <Badge tone={s.tone}>{s.label}</Badge>
                </div>
                <p className="mt-0.5 flex flex-wrap gap-x-1.5 text-sm text-muted-foreground [&>span]:whitespace-nowrap">
                  <span>{PROVIDERS[d.provider].label} ·</span>
                  <span>{money(d.price_micros)}</span>
                  {d.expires_on && <span>· renews {day(d.expires_on)}</span>}
                </p>
              </div>
              {/* On phones the outline buttons share the row and the remove icon keeps its size at the end. */}
              <div className="flex flex-wrap items-center gap-1.5 max-sm:w-full">
                {d.status === "uncertain" && (
                  <Button variant="outline" size="sm" className="max-sm:flex-1" onClick={() => call("confirm", d, "/confirm")} loading={pending === `confirm:${d.id}`}>
                    <RefreshCw className="size-4" /> Check Porkbun
                  </Button>
                )}
                {bought && d.dns_status === "error" && (
                  <Button variant="outline" size="sm" className="max-sm:flex-1" onClick={() => call("dns", d, "/dns", "DNS set")} loading={pending === `dns:${d.id}`}>
                    <RefreshCw className="size-4" /> Set DNS again
                  </Button>
                )}
                {bought && (
                  <>
                    <Button variant="outline" size="sm" className="max-sm:flex-1" onClick={() => setAdding(d)}>
                      <Plus className="size-4" /> Add record
                    </Button>
                    <Button variant="outline" size="sm" className="max-sm:flex-1" onClick={() => call("check", d, "/check-dns")} loading={pending === `check:${d.id}`}>
                      <RefreshCw className="size-4" /> Check DNS
                    </Button>
                  </>
                )}
                {!BUSY(d) && (
                  <Button variant="ghost" size="icon" onClick={() => remove(d)} loading={pending === `delete:${d.id}`} aria-label="Remove" title="Remove">
                    <Trash2 className="size-4" />
                  </Button>
                )}
              </div>
            </div>

            {d.error && (
              <p className={cn("mt-3 rounded-lg px-3 py-2 text-sm text-pretty", d.status === "failed" ? "bg-red-50 text-red-700" : "bg-amber-50 text-amber-800")}>
                <Message text={d.error} />
              </p>
            )}

            {bought && d.records.length > 0 && <Records domain={d.domain} records={d.records} />}

            {bought && (
              <>
                <div className="mt-4 flex flex-wrap items-center gap-2">
                  <DnsBadge name="MX" check={d.dns?.mx} />
                  <DnsBadge name="SPF" check={d.dns?.spf} />
                  <DnsBadge name="DMARC" check={d.dns?.dmarc} />
                  <DnsBadge name="DKIM" check={d.dns?.dkim} />
                  <span className="text-xs text-muted-foreground">{d.dns?.checked_at ? "New records can take an hour to show" : "DNS not checked yet"}</span>
                </div>
                <p className="mt-3 text-sm text-pretty text-muted-foreground">{PROVIDERS[d.provider].next}</p>
                <p className="mt-2 text-sm text-pretty text-muted-foreground">
                  {d.mailboxes === 0 ? (
                    <>
                      Then create a mailbox on {d.domain} and connect it under{" "}
                      <Link href="/app/domains" className="font-medium text-foreground underline-offset-4 hover:underline">
                        Mailboxes
                      </Link>
                      , where warmup starts on its own.
                    </>
                  ) : (
                    <>
                      {plural(d.mailboxes, "mailbox", "mailboxes")} connected, {d.warming === d.mailboxes ? "all" : d.warming} warming up.{" "}
                      <Link href="/app/domains" className="font-medium text-foreground underline-offset-4 hover:underline">
                        See them under Mailboxes
                      </Link>
                    </>
                  )}
                </p>
              </>
            )}
          </Card>
        );
      })}

      <Modal open={adding !== null} onClose={() => setAdding(null)} title="Add a record from your provider" className="max-w-xl">
        {adding && (
          <AddRecord domain={adding} refresh={mutate} onDone={() => setAdding(null)} />
        )}
      </Modal>
    </div>
  );
}

function Records({ domain, records }: { domain: string; records: DomainRecord[] }) {
  return (
    <div className="mt-4 divide-y divide-border rounded-xl border border-border text-xs">
      {records.map((r, i) => (
        <div key={i} className="grid grid-cols-[3.25rem_minmax(0,1fr)_auto] items-start gap-x-3 gap-y-1 px-3 py-2">
          <span className="pt-px font-mono font-medium">{r.type === "URL" ? "Forward" : r.type}</span>
          <div className="min-w-0">
            <p className="font-mono text-muted-foreground">
              <Host host={r.name ? `${r.name}.${domain}` : domain} />
            </p>
            <p className="mt-0.5 line-clamp-3 font-mono wrap-anywhere" title={r.content}>
              {r.prio !== undefined && `${r.prio} `}
              {r.content}
            </p>
            {r.error && (
              <p className="mt-1 text-pretty text-red-700">
                <Message text={r.error} />
              </p>
            )}
          </div>
          {r.ok ? (
            <Check className="size-4 text-brand-600" aria-label="Set" />
          ) : (
            <X className="size-4 text-red-600" aria-label="Not set" />
          )}
        </div>
      ))}
    </div>
  );
}

function AddRecord({ domain, refresh, onDone }: { domain: SendingDomain; refresh: () => Promise<unknown>; onDone: () => void }) {
  const [type, setType] = useState<"TXT" | "CNAME">("TXT");
  const [name, setName] = useState("");
  const [content, setContent] = useState("");
  const { pending, run } = useAction();

  const add = (e: React.FormEvent) => {
    e.preventDefault();
    run("add", async () => {
      // A record Porkbun refused is still listed, with its reason, so the card refreshes either way.
      try {
        await api(`/sending-domains/${domain.id}/records`, { body: { type, name, content } });
      } finally {
        await refresh();
      }
      onDone();
    }, "Record added");
  };

  return (
    <form onSubmit={add} className="grid gap-4">
      <p className="text-sm text-pretty text-muted-foreground">{PROVIDERS[domain.provider].next}</p>
      <div className="grid gap-4 sm:grid-cols-[8rem_minmax(0,1fr)]">
        <Field label="Type">
          <Select value={type} onChange={(e) => setType(e.target.value as typeof type)}>
            <option value="TXT">TXT</option>
            <option value="CNAME">CNAME</option>
          </Select>
        </Field>
        <Field label="Host" hint={`Like google._domainkey, with or without .${domain.domain}`}>
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="google._domainkey" spellCheck={false} />
        </Field>
      </div>
      <Field label="Value">
        <Textarea value={content} onChange={(e) => setContent(e.target.value)} placeholder="v=DKIM1; k=rsa; p=…" spellCheck={false} className="font-mono text-xs" />
      </Field>
      <div className="flex justify-end">
        <Button type="submit" loading={pending === "add"} disabled={!content.trim()}>
          <Globe className="size-4" /> Add to {domain.domain}
        </Button>
      </div>
    </form>
  );
}
