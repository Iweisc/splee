"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import useSWR from "swr";
import { toast } from "sonner";
import { ExternalLink, RefreshCw, ShieldCheck } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LoadError, SectionsSkeleton } from "@/components/app/settings/layout";
import { ProviderLogo } from "@/components/app/settings/provider-logo";
import { useAction } from "@/components/app/settings/use-action";
import { formatDay } from "@/components/app/work/daily-chart";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Address, Badge, Card, EmptyState, type BadgeTone } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { BlocklistCheck, Deliverability, DomainHealth, HealthPoint, HealthRates, HealthReason, MailboxHealth, PostmasterDay, Verdict } from "@/lib/types";
import { cn, num, timeAgo } from "@/lib/utils";

const VERDICT: Record<Verdict, { tone: BadgeTone; label: string; dot: string }> = {
  good: { tone: "green", label: "Good", dot: "bg-brand-500" },
  watch: { tone: "amber", label: "Watch", dot: "bg-amber-500" },
  at_risk: { tone: "red", label: "At risk", dot: "bg-red-500" },
};

/** The bounce line the verdict holds a mailbox past. Keep in sync with BOUNCE_AT_RISK in deliverability/thresholds.rs. */
const BOUNCE_AT_RISK = 0.03;

/** Like the backend's thresholds::percent: "4.1%", "0.25%", "3%". */
function pct(rate: number) {
  const p = rate * 100;
  return `${Number(p.toFixed(p >= 1 ? 1 : 2))}%`;
}

export default function DeliverabilityPage() {
  const { data, error, mutate } = useSWR<Deliverability>("/deliverability", fetcher);
  const { pending, run } = useAction();

  // The Postmaster sign-in comes back here with how it went.
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const connected = params.get("postmaster");
    const failed = params.get("postmaster_error");
    if (!connected && !failed) return;
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    else toast.success("Google Postmaster connected");
  }, []);

  const check = () => run("check", async () => mutate(await api<Deliverability>("/deliverability/check", { method: "POST" }), { revalidate: false }), "Checked");
  const connect = () =>
    run("connect", async () => {
      const { url } = await api<{ url: string }>("/deliverability/postmaster/oauth", { method: "POST" });
      window.location.assign(url);
    });
  const disconnect = () => {
    if (!confirm("Disconnect Google Postmaster? Its numbers are removed from this page.")) return;
    run("disconnect", async () => {
      await api("/deliverability/postmaster", { method: "DELETE" });
      await mutate();
    }, "Google Postmaster disconnected");
  };

  return (
    <>
      <PageHeader
        title="Deliverability"
        description={
          <>
            Whether your email is landing in trouble, for each domain and mailbox you send from. Splee checks the blocklists every 6 hours and
            holds a mailbox at risk from sending until you resume it on the{" "}
            <Link href="/app/domains" className="font-medium text-foreground underline-offset-4 hover:underline">
              Mailboxes page
            </Link>
            .
          </>
        }
        actions={
          data &&
          data.domains.length > 0 && (
            <Button variant="outline" className="max-sm:w-full" onClick={check} loading={pending === "check"}>
              <RefreshCw className="size-4" /> Check now
            </Button>
          )
        }
      />
      <PageBody className="mx-auto max-w-5xl">
        {error ? (
          <LoadError error={error} what="deliverability" retry={() => mutate()} />
        ) : !data ? (
          <SectionsSkeleton />
        ) : data.domains.length === 0 ? (
          <Card>
            <EmptyState icon={<ShieldCheck className="size-6" />} title="Nothing to check yet">
              Connect a mailbox and Splee starts checking its domain&apos;s blocklists and bounce rates.
            </EmptyState>
          </Card>
        ) : (
          <div className="grid gap-6">
            <Summary data={data} />
            <ChartCard data={data} />
            {data.domains.map((d) => (
              <DomainCard key={d.domain} d={d} mailboxes={data.mailboxes.filter((m) => m.domain === d.domain)} postmaster={data.postmaster.connection} />
            ))}
            <PostmasterCard data={data} pending={pending} connect={connect} disconnect={disconnect} />
          </div>
        )}
      </PageBody>
    </>
  );
}

function Summary({ data }: { data: Deliverability }) {
  const count = (v: Verdict) => data.mailboxes.filter((m) => m.verdict === v).length;
  const held = data.mailboxes.filter((m) => m.held).length;
  return (
    <div className="grid grid-cols-3 gap-3">
      {(["good", "watch", "at_risk"] as const).map((v) => (
        <Card key={v} className="p-4">
          <p className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
            <span className={cn("size-2 rounded-full", VERDICT[v].dot)} />
            {VERDICT[v].label}
          </p>
          <p className="mt-1 text-2xl font-semibold tabular-nums">{count(v)}</p>
          <p className="text-xs text-muted-foreground">
            {v === "at_risk" && held > 0 ? `${held} held` : count(v) === 1 ? "mailbox" : "mailboxes"}
          </p>
        </Card>
      ))}
      {data.checked_at && <p className="col-span-3 -mt-1 text-xs text-muted-foreground">Blocklists checked {timeAgo(data.checked_at)}</p>}
    </div>
  );
}

const SERIES = [
  { key: "bounce_rate", label: "Bounced", stroke: "stroke-amber-500", swatch: "bg-amber-500" },
  { key: "opt_out_rate", label: "Opted out", stroke: "stroke-sky-500", swatch: "bg-sky-500" },
  { key: "spam_rate", label: "Gmail spam", stroke: "stroke-red-500", swatch: "bg-red-500" },
] as const;

const W = 20;
const H = 150;

function ChartCard({ data }: { data: Deliverability }) {
  const [domain, setDomain] = useState("");
  const days = (data.series.find((s) => s.domain === domain) ?? data.series[0]).days;
  return (
    <Card className="min-w-0 p-5">
      <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <h2 className="font-medium">Bounce and spam rates, last 30 days</h2>
          <p className="text-xs text-muted-foreground">Each day shows the week up to it. Gmail spam comes from Google Postmaster.</p>
        </div>
        {data.domains.length > 1 && (
          <Select value={domain} onChange={(e) => setDomain(e.target.value)} aria-label="Domain" className="sm:w-auto">
            <option value="">All domains</option>
            {data.domains.map((d) => (
              <option key={d.domain} value={d.domain}>
                {d.domain}
              </option>
            ))}
          </Select>
        )}
      </div>
      <RateChart days={days} />
    </Card>
  );
}

/** A line per rate on one scale, with the bounce line a mailbox is held past, so how close each day came reads at a glance. */
function RateChart({ days }: { days: HealthPoint[] }) {
  const [hover, setHover] = useState<number | null>(null);
  const series = SERIES.filter((s) => s.key !== "spam_rate" || days.some((d) => d.spam_rate !== null));
  const values = days.flatMap((d) => series.map((s) => d[s.key] ?? 0));
  // Never below the 3% line, so it always shows with room for its label, and a quiet month doesn't fill the chart with noise.
  const max = Math.max(BOUNCE_AT_RISK, ...values) * 1.25;
  const y = (rate: number) => H - (rate / max) * H;
  const shown = days[hover ?? days.length - 1];

  const path = (key: (typeof SERIES)[number]["key"]) => {
    let d = "";
    let open = false;
    days.forEach((p, i) => {
      const v = p[key];
      if (v === null) {
        open = false;
        return;
      }
      d += `${open ? "L" : "M"}${i * W + W / 2},${y(v)}`;
      open = true;
    });
    return d;
  };

  return (
    <div>
      <div className="mb-3 flex flex-wrap items-center justify-between gap-3 text-xs">
        <div className="flex flex-wrap gap-x-4 gap-y-1">
          {series.map((s) => (
            <span key={s.key} className="inline-flex items-center gap-1.5 whitespace-nowrap text-muted-foreground">
              <span className={cn("h-0.5 w-3 rounded-full", s.swatch)} />
              {s.label}
              {shown && <span className="font-medium text-foreground tabular-nums">{shown[s.key] === null ? "–" : pct(shown[s.key]!)}</span>}
            </span>
          ))}
        </div>
        {shown && <span className="font-mono text-muted-foreground">Week to {formatDay(shown.date)}</span>}
      </div>
      <div className="relative">
        <svg
          viewBox={`0 0 ${Math.max(1, days.length) * W} ${H}`}
          preserveAspectRatio="none"
          className="h-40 w-full overflow-visible"
          onMouseLeave={() => setHover(null)}
          role="img"
          aria-label="Bounce, opt-out and Gmail spam rates per day"
        >
          <line x1={0} x2={days.length * W} y1={y(BOUNCE_AT_RISK)} y2={y(BOUNCE_AT_RISK)} className="stroke-red-500/40" strokeDasharray="4 4" vectorEffect="non-scaling-stroke" />
          {days.map((d, i) => (
            <rect key={d.date} x={i * W} y={0} width={W} height={H} className={hover === i ? "fill-black/[0.03]" : "fill-transparent"} onMouseEnter={() => setHover(i)} />
          ))}
          {series.map((s) => (
            <path key={s.key} d={path(s.key)} fill="none" className={s.stroke} strokeWidth={2} strokeLinejoin="round" vectorEffect="non-scaling-stroke" />
          ))}
        </svg>
        <span className="pointer-events-none absolute right-0 text-[11px] text-red-600" style={{ top: `calc(${(y(BOUNCE_AT_RISK) / H) * 100}% - 1.1rem)` }}>
          {pct(BOUNCE_AT_RISK)} bounced holds a mailbox
        </span>
      </div>
      <div className="mt-1.5 flex justify-between font-mono text-[11px] text-muted-foreground">
        <span>{days[0] && formatDay(days[0].date)}</span>
        <span>{days.at(-1) && formatDay(days.at(-1)!.date)}</span>
      </div>
    </div>
  );
}

function Reasons({ reasons }: { reasons: HealthReason[] }) {
  if (reasons.length === 0) return null;
  return (
    <ul className="mt-3 grid gap-2">
      {reasons.map((r) => (
        <li key={r.code} className={cn("rounded-lg px-3 py-2 text-sm", r.level === "at_risk" ? "bg-red-50" : "bg-amber-50")}>
          <p className={cn("font-medium", r.level === "at_risk" ? "text-red-800" : "text-amber-900")}>{r.text}</p>
          <p className="mt-0.5 text-xs text-pretty text-muted-foreground">{r.fix}</p>
        </li>
      ))}
    </ul>
  );
}

function DomainCard({ d, mailboxes, postmaster }: { d: DomainHealth; mailboxes: MailboxHealth[]; postmaster: Deliverability["postmaster"]["connection"] }) {
  return (
    <Card className="min-w-0 p-5">
      <div className="flex flex-wrap items-baseline gap-2">
        <h2 className="min-w-0 font-medium wrap-anywhere">{d.domain}</h2>
        <Badge tone={VERDICT[d.verdict].tone}>{VERDICT[d.verdict].label}</Badge>
        <span className="text-xs text-muted-foreground">{d.mailboxes === 1 ? "1 mailbox" : `${d.mailboxes} mailboxes`}</span>
      </div>
      <Reasons reasons={d.reasons} />
      <Blocklists listings={d.listings} />
      <Postmaster domain={d.domain} day={d.postmaster} connection={postmaster} />
      {mailboxes.length > 0 && (
        <div className="mt-4 grid gap-3 border-t border-border pt-4">
          {mailboxes.map((m) => (
            <MailboxRow key={m.id} m={m} shown={d.reasons.map((r) => r.code)} />
          ))}
        </div>
      )}
    </Card>
  );
}

const LIST_STATUS: Record<BlocklistCheck["status"], { tone: BadgeTone; label: string }> = {
  clear: { tone: "green", label: "Clear" },
  listed: { tone: "red", label: "Listed" },
  unknown: { tone: "neutral", label: "Couldn't check" },
};

/** Listings and lists that couldn't be asked get a row each. The clear ones share a line, since there's nothing to do about them. */
function Blocklists({ listings }: { listings: BlocklistCheck[] }) {
  if (listings.length === 0) return <p className="mt-3 text-xs text-muted-foreground">Blocklists not checked yet.</p>;
  const notable = listings.filter((l) => l.status !== "clear");
  const clear = listings.filter((l) => l.status === "clear");
  return (
    <div className="mt-4">
      <h3 className="text-xs font-medium text-muted-foreground">Blocklists</h3>
      {notable.length > 0 && (
        <ul className="mt-2 divide-y divide-border rounded-lg border border-border">
          {notable.map((l) => (
            <li key={`${l.list}:${l.target}`} className="flex flex-wrap items-start justify-between gap-x-4 gap-y-1 px-3 py-2">
              <div className="min-w-0 flex-1 basis-56">
                <p className="text-sm">
                  <span className="font-medium">{l.label}</span> <span className="font-mono text-xs wrap-anywhere text-muted-foreground">{l.target}</span>
                </p>
                {/* "Listed" is all some lists say, which the badge already does. */}
                {l.detail && l.detail !== "Listed" && <p className="mt-0.5 text-xs text-pretty text-muted-foreground">{l.detail}</p>}
              </div>
              <div className="flex items-center gap-3">
                {l.status === "listed" && (
                  <a href={l.delist_url} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 text-xs font-medium whitespace-nowrap underline-offset-4 hover:underline">
                    Removal page <ExternalLink className="size-3" />
                  </a>
                )}
                <Badge tone={LIST_STATUS[l.status].tone}>{LIST_STATUS[l.status].label}</Badge>
              </div>
            </li>
          ))}
        </ul>
      )}
      {clear.length > 0 && (
        <p className="mt-2 flex flex-wrap gap-1.5 text-xs text-muted-foreground">
          <span className="whitespace-nowrap">Clear on</span>
          {clear.map((l) => (
            <span key={`${l.list}:${l.target}`} className="rounded-full bg-black/5 px-2 py-0.5 whitespace-nowrap" title={l.target}>
              {l.label}
              {l.kind === "ip" && <span className="font-mono"> {l.target}</span>}
            </span>
          ))}
        </p>
      )}
    </div>
  );
}

const REPUTATION: Record<string, { tone: BadgeTone; label: string }> = {
  HIGH: { tone: "green", label: "High" },
  MEDIUM: { tone: "amber", label: "Medium" },
  LOW: { tone: "red", label: "Low" },
  BAD: { tone: "red", label: "Bad" },
};

function Postmaster({ domain, day, connection }: { domain: string; day: PostmasterDay | null; connection: Deliverability["postmaster"]["connection"] }) {
  if (!connection || connection.signed_out) return null;
  const verified = connection.domains.includes(domain);
  return (
    <div className="mt-4">
      <h3 className="text-xs font-medium text-muted-foreground">Google Postmaster{day && `, ${formatDay(day.day)}`}</h3>
      {day ? (
        <div className="mt-2 flex flex-wrap gap-x-5 gap-y-2 text-sm">
          <Stat label="Reputation">
            {REPUTATION[day.reputation] ? <Badge tone={REPUTATION[day.reputation].tone}>{REPUTATION[day.reputation].label}</Badge> : "–"}
          </Stat>
          <Stat label="Spam rate">{day.spam_rate === null ? "–" : pct(day.spam_rate)}</Stat>
          <Stat label="SPF">{day.spf_rate === null ? "–" : pct(day.spf_rate)}</Stat>
          <Stat label="DKIM">{day.dkim_rate === null ? "–" : pct(day.dkim_rate)}</Stat>
          <Stat label="DMARC">{day.dmarc_rate === null ? "–" : pct(day.dmarc_rate)}</Stat>
        </div>
      ) : (
        <p className="mt-1 text-xs text-pretty text-muted-foreground">
          {verified
            ? "Google has no numbers for this domain yet. It shows them once enough of its mail reaches Gmail each day."
            : "Not verified in Postmaster Tools. Add it at postmaster.google.com to see how Gmail rates it."}
        </p>
      )}
    </div>
  );
}

function Stat({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
      <span className="text-muted-foreground">{label}</span>
      <span className="font-medium tabular-nums">{children}</span>
    </span>
  );
}

/** A row per window, so the numbers line up and a phone never wraps one apart from its label. */
function Rates({ week, month }: { week: HealthRates; month: HealthRates }) {
  const cells = (when: string, r: HealthRates) => {
    const of = (n: number) => (r.emailed ? pct(n / r.emailed) : "–");
    return [when, num(r.emailed), of(r.bounced), of(r.opted_out), of(r.replied)];
  };
  const rows = [["", "Emailed", "Bounced", "Opted out", "Replied"], cells("7 days", week), cells("30 days", month)];
  return (
    <div className="mt-2 grid max-w-md grid-cols-[auto_repeat(4,minmax(0,1fr))] gap-x-3 gap-y-0.5 text-xs tabular-nums">
      {rows.flatMap((row, i) =>
        row.map((text, j) => (
          <span key={`${i}:${j}`} className={i === 0 || j === 0 ? "whitespace-nowrap text-muted-foreground" : undefined}>
            {text}
          </span>
        )),
      )}
    </div>
  );
}

/** `shown` is the codes of the reasons its domain's card already lists, which a mailbox shares. */
function MailboxRow({ m, shown }: { m: MailboxHealth; shown: string[] }) {
  return (
    <div className="min-w-0">
      <div className="flex flex-wrap items-baseline gap-2">
        <p className="min-w-0 text-sm font-medium">
          <Address email={m.email} />
        </p>
        <Badge tone={VERDICT[m.verdict].tone}>{VERDICT[m.verdict].label}</Badge>
        {m.held ? <Badge tone="red">Held</Badge> : m.status !== "active" && <Badge>{m.status === "paused" ? "Paused" : "Error"}</Badge>}
      </div>
      <Rates week={m.week} month={m.month} />
      {m.ips.length > 0 && (
        <p className="mt-1 text-xs text-muted-foreground">
          Sends from <span className="font-mono">{m.ips.join(", ")}</span>
          {m.ip_source === "received" ? ", as its warmup emails arrived" : ", its SMTP server's address"}
        </p>
      )}
      {m.held && (
        <p className="mt-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-pretty text-red-800">
          Splee stopped sending from this mailbox. Once you&apos;ve dealt with what&apos;s below, resume it on the{" "}
          <Link href="/app/domains" className="font-medium underline underline-offset-2">
            Mailboxes page
          </Link>
          .
        </p>
      )}
      <Reasons reasons={m.reasons.filter((r) => !shown.includes(r.code))} />
    </div>
  );
}

function PostmasterCard({
  data,
  pending,
  connect,
  disconnect,
}: {
  data: Deliverability;
  pending: string | null;
  connect: () => void;
  disconnect: () => void;
}) {
  const { available, connection } = data.postmaster;
  if (!available && !connection) return null;
  return (
    <Card className="min-w-0 p-5">
      <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-3">
        <div className="min-w-0 flex-1 basis-72">
          <h2 className="font-medium">Google Postmaster</h2>
          {connection && !connection.signed_out ? (
            <p className="mt-0.5 text-sm text-pretty text-muted-foreground">
              Connected as {connection.email}.{" "}
              {connection.domains.length > 0 ? `Verified there: ${connection.domains.join(", ")}.` : "No domains are verified there yet."}{" "}
              {connection.synced_at && `Synced ${timeAgo(connection.synced_at)}.`}
            </p>
          ) : (
            <p className="mt-0.5 text-sm text-pretty text-muted-foreground">
              {connection?.signed_out
                ? "Google signed Splee out. Connect again to keep Gmail's numbers coming."
                : "See how Gmail rates your domains: reputation, spam reports and SPF, DKIM and DMARC pass rates, for the domains you verified in Postmaster Tools."}
            </p>
          )}
          {connection?.error && !connection.signed_out && <p className="mt-2 rounded-lg bg-red-50 px-3 py-2 text-xs wrap-anywhere text-red-700">{connection.error}</p>}
        </div>
        {connection && !connection.signed_out ? (
          <Button variant="outline" className="max-sm:w-full" onClick={disconnect} loading={pending === "disconnect"}>
            Disconnect
          </Button>
        ) : (
          available && (
            <Button variant="outline" className="max-sm:w-full" onClick={connect} loading={pending === "connect"}>
              <ProviderLogo provider="google" /> Connect Google Postmaster
            </Button>
          )
        )}
      </div>
    </Card>
  );
}
