"use client";

import { useRef, useState } from "react";
import { api } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Field, Input, NumberInput, Select, Switch, Textarea } from "@/components/ui/form";
import { Tabs } from "@/components/ui/misc";
import { isUrl } from "@/components/app/settings/is-url";
import type { Mailbox, MailboxInput } from "@/lib/types";
import { SIGN_IN_LABEL } from "@/lib/mailbox";
import { MicroLabel } from "@/components/app/work/states";

type Preset = "google" | "microsoft" | "zoho" | "custom";

/** GET /mailboxes/settings: the provider an address's domain sends its mail to, with its SMTP and IMAP settings. */
type Found = Pick<MailboxInput, "smtp_host" | "smtp_port" | "smtp_security" | "imap_host" | "imap_port"> & { provider: string };

const PRESETS: Record<Exclude<Preset, "custom">, Pick<MailboxInput, "smtp_host" | "smtp_port" | "smtp_security" | "imap_host" | "imap_port">> = {
  google: { smtp_host: "smtp.gmail.com", smtp_port: 587, smtp_security: "starttls", imap_host: "imap.gmail.com", imap_port: 993 },
  microsoft: { smtp_host: "smtp.office365.com", smtp_port: 587, smtp_security: "starttls", imap_host: "outlook.office365.com", imap_port: 993 },
  zoho: { smtp_host: "smtp.zoho.com", smtp_port: 465, smtp_security: "tls", imap_host: "imap.zoho.com", imap_port: 993 },
};

const PRESET_NOTES: Record<Preset, string> = {
  google:
    "Google Workspace needs an app password: turn on 2-Step Verification, then create one at myaccount.google.com/apppasswords and use it for both SMTP and IMAP.",
  microsoft: "Microsoft 365 needs SMTP AUTH enabled for this mailbox in the admin center. With MFA on, use an app password.",
  zoho: "Zoho needs IMAP access enabled in Mail settings. With 2FA on, create an app-specific password.",
  custom: "Use the SMTP and IMAP settings from your email provider.",
};

const EMPTY: MailboxInput = {
  email: "",
  from_name: "",
  signature: "",
  booking_link: "",
  ...PRESETS.google,
  smtp_username: "",
  smtp_password: "",
  imap_username: "",
  imap_password: "",
  daily_limit: 30,
  cost_per_email_micros: 0,
  ramp_up: true,
  dkim_selector: "",
  warmup: true,
  warmup_daily_cap: 40,
  total_daily_limit: 80,
  hold_until_warm: true,
};

function toInput(m: Mailbox): MailboxInput {
  const { email, from_name, signature, booking_link, smtp_host, smtp_port, smtp_security, smtp_username, imap_host, imap_port, imap_username } = m;
  const { daily_limit, cost_per_email_micros, ramp_up, dkim_selector, warmup, warmup_daily_cap, total_daily_limit, hold_until_warm } = m;
  return {
    email,
    from_name,
    signature,
    booking_link,
    smtp_host,
    smtp_port,
    smtp_security,
    smtp_username,
    smtp_password: "",
    imap_host,
    imap_port,
    imap_username,
    imap_password: "",
    daily_limit,
    cost_per_email_micros,
    ramp_up,
    dkim_selector,
    warmup,
    warmup_daily_cap,
    total_daily_limit,
    hold_until_warm,
  };
}

function presetOf(m: Mailbox): Preset {
  const match = Object.entries(PRESETS).find(([, p]) => p.smtp_host === m.smtp_host && p.imap_host === m.imap_host);
  return (match?.[0] as Preset | undefined) ?? "custom";
}

/** Create (no `mailbox`) or edit. On edit, empty passwords keep the stored ones. */
export function MailboxForm({
  mailbox,
  saving,
  onSubmit,
}: {
  mailbox?: Mailbox;
  saving: boolean;
  onSubmit: (input: MailboxInput) => void;
}) {
  const [preset, setPreset] = useState<Preset>(mailbox ? presetOf(mailbox) : "google");
  // Settings are filled in from the address's domain until the user picks a provider, and never on edit.
  const [pickedByHand, setPickedByHand] = useState(!!mailbox);
  const [found, setFound] = useState<string | null>(null);
  // Bumped by each lookup, each change to the address and each tab click, so only the latest lookup's answer applies.
  const lookup = useRef(0);
  const [m, setM] = useState<MailboxInput>(mailbox ? toInput(mailbox) : EMPTY);
  // Most providers use the same login for SMTP and IMAP, so IMAP mirrors SMTP until the user edits it.
  const [separateImap, setSeparateImap] = useState(
    !!mailbox && (mailbox.imap_username !== mailbox.smtp_username || presetOf(mailbox) === "custom"),
  );

  const set = (patch: Partial<MailboxInput>) => setM((prev) => ({ ...prev, ...patch }));
  const pickPreset = (p: Preset) => {
    setPreset(p);
    if (p !== "custom") set(PRESETS[p]);
  };
  const lookUp = async (email: string) => {
    if (pickedByHand || !/^\S+@\S+\.\S+$/.test(email)) return;
    const id = ++lookup.current;
    const result = await api<Found | null>(`/mailboxes/settings?email=${encodeURIComponent(email)}`).catch(() => null);
    if (id !== lookup.current) return;
    if (!result) return setFound(null);
    const { provider, ...hosts } = result;
    const known = (Object.keys(PRESETS) as (keyof typeof PRESETS)[]).find(
      (p) => PRESETS[p].smtp_host === hosts.smtp_host && PRESETS[p].imap_host === hosts.imap_host,
    );
    pickPreset(known ?? "custom");
    if (!known) set(hosts);
    setFound(provider);
  };

  // The address and servers of a mailbox connected by signing in come from its provider, and the backend ignores them.
  const signedIn = mailbox && mailbox.provider !== "password" ? SIGN_IN_LABEL[mailbox.provider] : null;
  const imapUser = separateImap ? m.imap_username : m.smtp_username;
  const imapPassword = separateImap ? m.imap_password : m.smtp_password;
  const servers =
    signedIn || (m.smtp_host && m.smtp_port > 0 && m.smtp_username && (mailbox || m.smtp_password) && m.imap_host && m.imap_port > 0);
  const valid =
    /^\S+@\S+\.\S+$/.test(m.email) &&
    (!m.booking_link || isUrl(m.booking_link)) &&
    servers &&
    m.daily_limit > 0 &&
    m.warmup_daily_cap >= 1 &&
    m.warmup_daily_cap <= 200 &&
    m.total_daily_limit >= 1 &&
    m.cost_per_email_micros >= 0 &&
    m.cost_per_email_micros <= 1_000_000;

  return (
    <form
      // minmax(0,1fr) keeps the column at the modal's width. An auto column grows to its widest child, like the provider tabs, and pushes every field past the modal's edge on phones.
      className="grid grid-cols-[minmax(0,1fr)] gap-5"
      onSubmit={(e) => {
        e.preventDefault();
        if (valid) onSubmit({ ...m, imap_username: imapUser, imap_password: imapPassword });
      }}
    >
      {signedIn ? (
        <p className="text-sm text-pretty text-muted-foreground">
          Connected by signing in to {signedIn}. Splee sends and reads replies with that sign-in, so there are no server settings or passwords
          to change.
        </p>
      ) : (
        <div>
          {/* The four tabs don't fit one row on phones, and scrolling hid Zoho and Custom past the edge.
              So phones get a 2x2 grid with shorter labels, and the note below still names the full product. */}
          <Tabs
            className="max-sm:grid max-sm:grid-cols-2"
            value={preset}
            onChange={(p) => {
              lookup.current++;
              setPickedByHand(true);
              setFound(null);
              pickPreset(p);
            }}
            options={[
              { value: "google", label: <>Google<span className="max-sm:hidden"> Workspace</span></> },
              { value: "microsoft", label: <>Microsoft<span className="max-sm:hidden"> 365</span></> },
              { value: "zoho", label: "Zoho" },
              { value: "custom", label: "Custom" },
            ]}
          />
          <p className="mt-2 text-xs text-pretty text-muted-foreground">
            {preset === "custom" && found
              ? `Filled in with ${found}'s settings. With two-step login on, use an app password.`
              : PRESET_NOTES[preset]}
          </p>
        </div>
      )}

      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Email address">
          <Input
            type="email"
            value={m.email}
            onChange={(e) => {
              const email = e.target.value.trim();
              if (email !== m.email) lookup.current++;
              // Keep the username in step with the address while the user hasn't typed a different one.
              set({ email, ...(m.smtp_username === m.email ? { smtp_username: email } : {}) });
            }}
            onBlur={(e) => lookUp(e.target.value.trim())}
            placeholder="alex@try-acme.com"
            disabled={!!signedIn}
            autoFocus={!signedIn}
          />
        </Field>
        <Field label="From name">
          <Input value={m.from_name} onChange={(e) => set({ from_name: e.target.value })} placeholder="Alex Morgan" />
        </Field>
      </div>

      <Field label="Signature" hint="Signs the emails Splee writes when this mailbox sends them. Leave empty to use the project's.">
        <Textarea value={m.signature} onChange={(e) => set({ signature: e.target.value })} placeholder={"Alex Morgan\nFounder, Acme"} rows={2} />
      </Field>
      <Field
        label="Booking link"
        hint="Replaces the project's booking link in those emails, so calls land on this person's calendar."
        error={m.booking_link && !isUrl(m.booking_link) ? "Enter a full URL, e.g. https://cal.com/alex/intro" : null}
      >
        <Input type="url" value={m.booking_link} onChange={(e) => set({ booking_link: e.target.value.trim() })} placeholder="https://cal.com/alex/intro" />
      </Field>

      {!signedIn && (
        <>
          <fieldset className="grid gap-4">
            <MicroLabel>Sending (SMTP)</MicroLabel>
            {preset === "custom" && (
              <div className="grid gap-4 sm:grid-cols-[1fr_6rem_8rem]">
                <Field label="Host">
                  <Input value={m.smtp_host} onChange={(e) => set({ smtp_host: e.target.value.trim() })} placeholder="smtp.example.com" />
                </Field>
                <Field label="Port">
                  <NumberInput required value={m.smtp_port} onValueChange={(smtp_port) => set({ smtp_port })} />
                </Field>
                <Field label="Security">
                  <Select value={m.smtp_security} onChange={(e) => set({ smtp_security: e.target.value as MailboxInput["smtp_security"] })}>
                    <option value="starttls">STARTTLS</option>
                    <option value="tls">TLS</option>
                    <option value="none">None</option>
                  </Select>
                </Field>
              </div>
            )}
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="Username">
                <Input value={m.smtp_username} onChange={(e) => set({ smtp_username: e.target.value.trim() })} autoComplete="off" />
              </Field>
              <Field label={preset === "custom" ? "Password" : "App password"}>
                <Input
                  type="password"
                  value={m.smtp_password}
                  onChange={(e) => set({ smtp_password: e.target.value })}
                  placeholder={mailbox ? "Leave empty to keep" : undefined}
                  autoComplete="new-password"
                />
              </Field>
            </div>
          </fieldset>

          <fieldset className="grid gap-4">
            {/* On the narrowest phones the checkbox drops under the heading, instead of both squeezing onto two lines each. */}
            <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
              <MicroLabel className="whitespace-nowrap">Reading replies (IMAP)</MicroLabel>
              <label className="flex items-center gap-2 text-xs whitespace-nowrap text-muted-foreground">
                <input type="checkbox" checked={separateImap} onChange={(e) => setSeparateImap(e.target.checked)} className="accent-black" />
                Different login than SMTP
              </label>
            </div>
            {preset === "custom" && (
              <div className="grid gap-4 sm:grid-cols-[1fr_6rem]">
                <Field label="Host">
                  <Input value={m.imap_host} onChange={(e) => set({ imap_host: e.target.value.trim() })} placeholder="imap.example.com" />
                </Field>
                <Field label="Port">
                  <NumberInput required value={m.imap_port} onValueChange={(imap_port) => set({ imap_port })} />
                </Field>
              </div>
            )}
            {separateImap && (
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Username">
                  <Input value={m.imap_username} onChange={(e) => set({ imap_username: e.target.value.trim() })} autoComplete="off" />
                </Field>
                <Field label="Password">
                  <Input
                    type="password"
                    value={m.imap_password}
                    onChange={(e) => set({ imap_password: e.target.value })}
                    placeholder={mailbox ? "Leave empty to keep" : undefined}
                    autoComplete="new-password"
                  />
                </Field>
              </div>
            )}
            {preset !== "custom" && (
              <p className="text-xs text-muted-foreground">
                {m.smtp_host}:{m.smtp_port} · {m.imap_host}:{m.imap_port}
              </p>
            )}
          </fieldset>
        </>
      )}

      <Field label="Daily limit" hint="Emails per day from this mailbox. Start around 30 for a new domain and raise it slowly.">
        <NumberInput required min={1} value={m.daily_limit} onValueChange={(daily_limit) => set({ daily_limit })} className="w-28 tabular-nums" />
      </Field>

      <div className="flex items-start justify-between gap-4">
        <div>
          <p className="text-sm font-medium text-foreground">Ramp up slowly</p>
          <p className="mt-1 text-xs text-muted-foreground">
            Send 10 emails on the first day and 4 more each day until the daily limit, so providers get used to a new mailbox before it sends
            at full volume.
          </p>
        </div>
        <Switch checked={m.ramp_up} onChange={(ramp_up) => set({ ramp_up })} label="Ramp up slowly" />
      </div>

      <div className="flex items-start justify-between gap-4">
        <div>
          <p className="text-sm font-medium text-foreground">Warm up</p>
          <p className="mt-1 text-xs text-pretty text-muted-foreground">
            Trade short emails with the other mailboxes warming up on this Splee. They read, answer and rescue each other&apos;s emails from
            spam, so providers trust a new mailbox before it sends campaigns. Warmup emails are archived once read.
          </p>
        </div>
        <Switch checked={m.warmup} onChange={(warmup) => set({ warmup })} label="Warm up" />
      </div>

      {m.warmup && (
        <>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Warmup emails a day" hint="Starts at 2 and adds 2 a day up to this.">
              <NumberInput
                required
                min={1}
                max={200}
                value={m.warmup_daily_cap}
                onValueChange={(warmup_daily_cap) => set({ warmup_daily_cap })}
                className="w-28 tabular-nums"
              />
            </Field>
            <Field label="Total emails a day" hint="Campaign and warmup emails together never go past this.">
              <NumberInput
                required
                min={1}
                value={m.total_daily_limit}
                onValueChange={(total_daily_limit) => set({ total_daily_limit })}
                className="w-28 tabular-nums"
              />
            </Field>
          </div>
          <div className="flex items-start justify-between gap-4">
            <div>
              <p className="text-sm font-medium text-foreground">Hold campaigns until warm</p>
              <p className="mt-1 text-xs text-pretty text-muted-foreground">
                No campaign emails in the first 14 days of warmup, or while its warmup emails land in spam.
              </p>
            </div>
            <Switch checked={m.hold_until_warm} onChange={(hold_until_warm) => set({ hold_until_warm })} label="Hold campaigns until warm" />
          </div>
        </>
      )}

      <Field
        label="Cost per 1,000 emails"
        hint="What your email provider charges per email, so your spend stats include it. Leave it at 0 for a flat-rate mailbox like Google Workspace."
      >
        <div className="relative w-28">
          <span className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-sm text-muted-foreground">$</span>
          {/* Stored per email in micro-dollars, so $1 per 1,000 emails is 1,000 micros per email. */}
          <NumberInput
            required
            min={0}
            step={0.01}
            value={m.cost_per_email_micros / 1000}
            onValueChange={(usd) => set({ cost_per_email_micros: Math.round(usd * 1000) })}
            className="pl-6 tabular-nums"
          />
        </div>
      </Field>

      <Field label="DKIM selector" hint="Only if your provider signs with a selector of its own. Check DNS tries it before the common ones.">
        <Input value={m.dkim_selector} onChange={(e) => set({ dkim_selector: e.target.value.trim() })} placeholder="s1" className="w-40" />
      </Field>

      <div className="flex justify-end">
        <Button type="submit" disabled={!valid} loading={saving}>
          {mailbox ? "Save" : "Connect mailbox"}
        </Button>
      </div>
    </form>
  );
}
