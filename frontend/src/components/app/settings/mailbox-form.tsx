"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Field, Input, NumberInput, Select, Switch } from "@/components/ui/form";
import { Tabs } from "@/components/ui/misc";
import type { Mailbox, MailboxInput } from "@/lib/types";
import { MicroLabel } from "@/components/app/work/states";

type Preset = "google" | "microsoft" | "zoho" | "custom";

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
  ...PRESETS.google,
  smtp_username: "",
  smtp_password: "",
  imap_username: "",
  imap_password: "",
  daily_limit: 30,
  cost_per_email_micros: 0,
  ramp_up: true,
};

function toInput(m: Mailbox): MailboxInput {
  const { email, from_name, smtp_host, smtp_port, smtp_security, smtp_username, imap_host, imap_port, imap_username, daily_limit, cost_per_email_micros, ramp_up } =
    m;
  return {
    email,
    from_name,
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

  const imapUser = separateImap ? m.imap_username : m.smtp_username;
  const imapPassword = separateImap ? m.imap_password : m.smtp_password;
  const valid =
    /^\S+@\S+\.\S+$/.test(m.email) &&
    m.smtp_host &&
    m.smtp_port > 0 &&
    m.smtp_username &&
    (mailbox || m.smtp_password) &&
    m.imap_host &&
    m.imap_port > 0 &&
    m.daily_limit > 0 &&
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
      <div>
        {/* The four tabs don't fit one row on phones, and scrolling hid Zoho and Custom past the edge.
            So phones get a 2x2 grid with shorter labels, and the note below still names the full product. */}
        <Tabs
          className="max-sm:grid max-sm:grid-cols-2"
          value={preset}
          onChange={pickPreset}
          options={[
            { value: "google", label: <>Google<span className="max-sm:hidden"> Workspace</span></> },
            { value: "microsoft", label: <>Microsoft<span className="max-sm:hidden"> 365</span></> },
            { value: "zoho", label: "Zoho" },
            { value: "custom", label: "Custom" },
          ]}
        />
        <p className="mt-2 text-xs text-muted-foreground">{PRESET_NOTES[preset]}</p>
      </div>

      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Email address">
          <Input
            type="email"
            value={m.email}
            onChange={(e) => {
              const email = e.target.value.trim();
              // Keep the username in step with the address while the user hasn't typed a different one.
              set({ email, ...(m.smtp_username === m.email ? { smtp_username: email } : {}) });
            }}
            placeholder="alex@try-acme.com"
            autoFocus
          />
        </Field>
        <Field label="From name">
          <Input value={m.from_name} onChange={(e) => set({ from_name: e.target.value })} placeholder="Alex Morgan" />
        </Field>
      </div>

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

      <div className="flex justify-end">
        <Button type="submit" disabled={!valid} loading={saving}>
          {mailbox ? "Save" : "Connect mailbox"}
        </Button>
      </div>
    </form>
  );
}
