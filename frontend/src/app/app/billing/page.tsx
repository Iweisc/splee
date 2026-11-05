"use client";

import { useRouter, useSearchParams } from "next/navigation";
import { Suspense, useEffect, useState } from "react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { CheckCircle2, OctagonX, Receipt, XCircle } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LoadError, MicroLabel, Section, SettingsColumn } from "@/components/app/settings/layout";
import { EMAIL_COST_MICROS } from "@/components/app/settings/pricing";
import { StopCampaignsModal } from "@/components/app/settings/stop-campaigns-modal";
import { toastError } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { Card, EmptyState, Skeleton } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api, ApiError } from "@/lib/api";
import type { BillingProfile, BillingSummary, LedgerKind } from "@/lib/types";
import { cn, dateTime, money, num } from "@/lib/utils";

const AMOUNTS = [30, 100, 300];
const MIN_TOPUP_USD = 10;

const LEDGER_LABELS: Record<LedgerKind, string> = {
  signup_bonus: "Signup credits",
  topup: "Top-up",
  email_sent: "Email sent",
  referral_bonus: "Referral reward",
  adjustment: "Adjustment",
};

export default function BillingPage() {
  const { data, error } = useSWR<BillingSummary>("/billing", fetcher);
  const { mutate: mutateMe } = useMe();
  const [stopOpen, setStopOpen] = useState(false);

  return (
    <>
      <PageHeader title="Billing" description={`Pay as you go: ${money(EMAIL_COST_MICROS)} per sent email.`} />
      <PageBody>
        <SettingsColumn>
          <Suspense>
            <TopupBanner />
          </Suspense>

          {error ? (
            <LoadError error={error} what="billing" />
          ) : (
            <>
              <Card className="grid gap-px overflow-hidden bg-border sm:grid-cols-4">
                <Stat
                  label="Balance"
                  value={data && (data.payments_enabled ? money(data.balance_micros) : "Not enforced")}
                  large
                  danger={!!data?.payments_enabled && data.balance_micros <= 0}
                />
                <Stat label="Spent today" value={data && money(data.spent_today_micros)} />
                <Stat label="Total spent" value={data && money(data.total_spent_micros)} />
                <Stat label="Emails sent" value={data && num(data.emails_sent)} />
              </Card>
              {data && !data.payments_enabled ? (
                <Section title="Payments are disabled" description="This server runs with PAYMENTS_ENABLED=false, so the balance never blocks sending. Usage is still recorded below." />
              ) : (
                <AddFunds stripeEnabled={data?.stripe_enabled} />
              )}
              <Ledger data={data} />
            </>
          )}

          <BillingProfileForm />

          <Section
            title="Stop all campaigns"
            description="Pauses sending in every project of this organization right away. You can start each project again later."
            actions={
              <Button variant="danger" onClick={() => setStopOpen(true)}>
                <OctagonX className="size-4" /> Stop all
              </Button>
            }
            className="ring-1 ring-red-200"
          />
        </SettingsColumn>
      </PageBody>
      <StopCampaignsModal open={stopOpen} onClose={() => setStopOpen(false)} onStopped={() => mutateMe()} />
    </>
  );
}

function TopupBanner() {
  const status = useSearchParams().get("topup");
  const router = useRouter();
  const { mutate } = useSWRConfig();

  useEffect(() => {
    if (status !== "success") return;
    const refresh = () => {
      mutate("/billing");
      mutate("/auth/me");
    };
    // Stripe's webhook can land a moment after the redirect back, so refetch once more shortly after.
    refresh();
    const t = setTimeout(refresh, 4000);
    return () => clearTimeout(t);
  }, [status, mutate]);

  if (status !== "success" && status !== "cancelled") return null;
  const success = status === "success";
  return (
    <div
      className={cn(
        "flex items-center gap-3 rounded-2xl px-5 py-3.5 text-sm",
        success ? "bg-brand-50 text-brand-800 ring-1 ring-brand-200" : "bg-black/5 text-foreground",
      )}
    >
      {success ? <CheckCircle2 className="size-5 shrink-0 text-brand-600" /> : <XCircle className="size-5 shrink-0 text-muted-foreground" />}
      <p className="flex-1">
        {success
          ? "Payment received. Your balance updates as soon as Stripe confirms it, usually within a few seconds."
          : "Payment cancelled. You were not charged."}
      </p>
      <Button variant="ghost" size="sm" onClick={() => router.replace("/app/billing")}>
        Dismiss
      </Button>
    </div>
  );
}

function Stat({ label, value, large, danger }: { label: string; value?: string; large?: boolean; danger?: boolean }) {
  return (
    <div className="bg-card px-5 py-4">
      <MicroLabel>{label}</MicroLabel>
      {value === undefined ? (
        <Skeleton className="mt-2 h-7 w-20" />
      ) : (
        <p className={cn("mt-1 font-semibold tabular-nums", large ? "text-2xl" : "text-xl", danger && "text-danger")}>{value}</p>
      )}
    </div>
  );
}

function AddFunds({ stripeEnabled }: { stripeEnabled?: boolean }) {
  const [amount, setAmount] = useState<number | "custom">(100);
  const [custom, setCustom] = useState("");
  const [loading, setLoading] = useState(false);
  const usd = amount === "custom" ? Number(custom) : amount;
  const valid = Number.isFinite(usd) && usd >= MIN_TOPUP_USD;

  const checkout = async () => {
    setLoading(true);
    try {
      const { checkout_url } = await api<{ checkout_url: string }>("/billing/topup", { body: { amount_cents: Math.round(usd * 100) } });
      window.location.assign(checkout_url);
    } catch (e) {
      setLoading(false);
      if (e instanceof ApiError && e.status === 501) toastError(new Error("Card payments aren't set up on this server. Set STRIPE_SECRET_KEY on the backend to enable top-ups."));
      else toastError(e);
    }
  };

  return (
    <Section title="Add funds" description="Your balance is shared by every project. Sending stops when it reaches $0.">
      <div className="flex flex-wrap items-center gap-2">
        {AMOUNTS.map((a) => (
          <AmountButton key={a} active={amount === a} onClick={() => setAmount(a)}>
            ${a}
          </AmountButton>
        ))}
        <AmountButton active={amount === "custom"} onClick={() => setAmount("custom")}>
          Custom
        </AmountButton>
        {amount === "custom" && (
          <div className="relative w-32">
            <span className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-sm text-muted-foreground">$</span>
            <Input
              type="number"
              min={MIN_TOPUP_USD}
              value={custom}
              onChange={(e) => setCustom(e.target.value)}
              placeholder={String(MIN_TOPUP_USD)}
              className="pl-6 tabular-nums"
              aria-label="Custom amount in dollars"
              autoFocus
            />
          </div>
        )}
      </div>
      <div className="mt-4 flex flex-wrap items-center justify-between gap-3">
        <p className="text-sm text-muted-foreground">
          {amount === "custom" && !valid
            ? `Minimum top-up is $${MIN_TOPUP_USD}.`
            : `≈ ${num(Math.floor((usd * 1_000_000) / EMAIL_COST_MICROS))} emails. You'll pay on Stripe's secure checkout page.`}
        </p>
        <Button onClick={checkout} disabled={!valid || stripeEnabled === false} loading={loading}>
          Add ${valid ? usd : "…"}
        </Button>
      </div>
      {stripeEnabled === false && (
        <p className="mt-3 rounded-lg bg-amber-50 px-3 py-2 text-sm text-amber-800">
          Card payments aren&apos;t set up on this server yet. Set STRIPE_SECRET_KEY on the backend to enable top-ups.
        </p>
      )}
    </Section>
  );
}

function AmountButton({ active, onClick, children }: { active: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={cn(
        "h-10 min-w-20 rounded-lg border px-4 text-sm font-medium tabular-nums transition-colors",
        active ? "border-foreground bg-primary text-primary-foreground" : "border-border-strong bg-card hover:bg-accent",
      )}
    >
      {children}
    </button>
  );
}

const LEDGER_PREVIEW = 10;

function Ledger({ data }: { data?: BillingSummary }) {
  // Every sent email is a row, so 100 rows pushed billing details and "Stop all" off the screen.
  const [all, setAll] = useState(false);
  const rows = data && (all ? data.ledger : data.ledger.slice(0, LEDGER_PREVIEW));
  return (
    <Card>
      <div className="px-6 pt-5 pb-3">
        <h2 className="font-semibold">History</h2>
        <p className="mt-0.5 text-sm text-muted-foreground">The last 100 credits and charges.</p>
      </div>
      {!data ? (
        <div className="space-y-3 px-6 pb-6">
          {Array.from({ length: 4 }, (_, i) => (
            <Skeleton key={i} className="h-5 w-full" />
          ))}
        </div>
      ) : data.ledger.length === 0 ? (
        <EmptyState icon={<Receipt className="size-6" />} title="No transactions yet" className="py-8" />
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-y border-border text-left">
                <th className="px-6 py-2 font-mono text-[11px] font-normal tracking-wide text-muted-foreground uppercase">Date</th>
                <th className="px-3 py-2 font-mono text-[11px] font-normal tracking-wide text-muted-foreground uppercase">Type</th>
                <th className="px-3 py-2 font-mono text-[11px] font-normal tracking-wide text-muted-foreground uppercase">Note</th>
                <th className="px-6 py-2 text-right font-mono text-[11px] font-normal tracking-wide text-muted-foreground uppercase">Amount</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border">
              {rows?.map((e) => (
                <tr key={e.id}>
                  <td className="px-6 py-2.5 whitespace-nowrap text-muted-foreground">{dateTime(e.created_at)}</td>
                  <td className="px-3 py-2.5 whitespace-nowrap">{LEDGER_LABELS[e.kind]}</td>
                  <td className="max-w-64 truncate px-3 py-2.5 text-muted-foreground">{e.note}</td>
                  <td className={cn("px-6 py-2.5 text-right font-medium whitespace-nowrap tabular-nums", e.amount_micros > 0 && "text-brand-700")}>
                    {e.amount_micros > 0 ? "+" : "−"}
                    {money(Math.abs(e.amount_micros))}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {data.ledger.length > LEDGER_PREVIEW && (
            <div className="border-t border-border px-3 py-2">
              <Button variant="ghost" size="sm" onClick={() => setAll(!all)}>
                {all ? "Show fewer" : `Show all ${num(data.ledger.length)}`}
              </Button>
            </div>
          )}
        </div>
      )}
    </Card>
  );
}

type ProfileFields = Omit<BillingProfile, "org_id" | "updated_at">;

const PROFILE_FIELDS: { key: keyof ProfileFields; label: string; wide?: boolean }[] = [
  { key: "company_name", label: "Company name", wide: true },
  { key: "address_line1", label: "Address", wide: true },
  { key: "address_line2", label: "Address line 2", wide: true },
  { key: "city", label: "City" },
  { key: "postal_code", label: "Postal code" },
  { key: "country", label: "Country" },
  { key: "vat_id", label: "VAT ID" },
];

function BillingProfileForm() {
  const { data, error, mutate } = useSWR<BillingProfile>("/billing/profile", fetcher);
  const [draft, setDraft] = useState<ProfileFields | null>(null);
  const [saving, setSaving] = useState(false);
  const values = draft ?? data;
  const dirty = !!draft && !!data && PROFILE_FIELDS.some(({ key }) => draft[key] !== data[key]);

  const save = async () => {
    if (!draft) return;
    setSaving(true);
    try {
      await mutate(await api<BillingProfile>("/billing/profile", { method: "PUT", body: { ...draft } }), { revalidate: false });
      setDraft(null);
      toast.success("Billing details saved");
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };

  return (
    <Section title="Billing details" description="Company and tax details for this organization.">
      {error ? (
        <LoadError error={error} what="billing details" />
      ) : !values ? (
        <Skeleton className="h-40 w-full" />
      ) : (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          <div className="grid gap-4 sm:grid-cols-2">
            {PROFILE_FIELDS.map(({ key, label, wide }) => (
              <Field key={key} label={label} className={cn(wide && "sm:col-span-2")}>
                <Input value={values[key]} onChange={(e) => setDraft({ ...values, [key]: e.target.value })} />
              </Field>
            ))}
          </div>
          <div className="mt-5 flex justify-end gap-2">
            {dirty && (
              <Button type="button" variant="outline" onClick={() => setDraft(null)} disabled={saving}>
                Discard
              </Button>
            )}
            <Button type="submit" disabled={!dirty} loading={saving}>
              Save
            </Button>
          </div>
        </form>
      )}
    </Section>
  );
}
