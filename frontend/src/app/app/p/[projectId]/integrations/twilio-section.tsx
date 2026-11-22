"use client";

import { useState } from "react";
import Link from "next/link";
import { Smartphone } from "lucide-react";
import { toast } from "sonner";
import useSWR from "swr";
import { CopyField } from "@/components/app/settings/copy-button";
import { Section } from "@/components/app/settings/layout";
import { toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input, Select, Switch, Textarea } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { ProjectSettings, TwilioStatus } from "@/lib/types";
import { LastFailure, submit } from "./shared";

const TEMPLATE_FIELDS = {
  first_name: "First name",
  last_name: "Last name",
  full_name: "Full name",
  company_name: "Company",
  title: "Job title",
  city: "City",
  sender_name: "Your sender name",
} as const;

/** The organization's Twilio account, shared by every project. The API key's secret and the auth token are write-only, and
 *  connecting checks both with Twilio first. */
export function TwilioSection({ settings }: { settings: ProjectSettings }) {
  const { data, mutate } = useSWR<TwilioStatus>("/organization/twilio", fetcher);
  const empty = { account_sid: "", api_key_sid: "", api_secret: "", auth_token: "", sms_from: "", whatsapp_from: "" };
  const [form, setForm] = useState(empty);
  const set = (key: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [key]: e.target.value.trim() });
  const [busy, setBusy] = useState(false);
  const call = async (path: string, method: "PUT" | "DELETE", body: Record<string, unknown> | undefined, done: string) => {
    setBusy(true);
    try {
      await mutate(await api<TwilioStatus>(path, { method, body }), { revalidate: false });
      toast.success(done);
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      setBusy(false);
    }
  };
  const connect = async () => {
    if (await call("/organization/twilio", "PUT", form, "Connected to Twilio")) setForm(empty);
  };
  const disconnect = () => {
    if (confirm("Disconnect Twilio for every project? Approved texts will wait until you connect it again.")) call("/organization/twilio", "DELETE", undefined, "Twilio disconnected");
  };
  const c = data?.connection;
  const steps = settings.text_steps.length;
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Smartphone className="size-4" /> Texts and calls
        </span>
      }
      description="Sends the SMS and WhatsApp steps of your sequences and your replies to texts, and places your team's calls, from your own Twilio account. Every project in your organization shares it."
      actions={
        c ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {!data ? null : c ? (
        <div className="flex flex-col gap-4">
          <p className="text-sm text-pretty">
            Texting through {c.account_name || "your Twilio account"}
            {c.sms_from && <> with SMS from {c.sms_from}</>}
            {c.whatsapp_from && <>{c.sms_from ? " and" : " with"} WhatsApp from {c.whatsapp_from}</>}. Due texts go out in your sending hours, in the
            lead&apos;s own time zone when their number shows it, up to {settings.daily_text_limit} a day.{" "}
            {steps ? `This project has ${steps === 1 ? "1 text step" : `${steps} text steps`} on the ` : "Add text steps on the "}
            <Link href={`/app/p/${settings.project_id}/follow-ups`} className="font-medium underline underline-offset-2">
              Follow-ups page
            </Link>
            .
          </p>
          {/* Not a Field: its <label> wrapper would turn a click on the label text into a copy. */}
          <div className="flex flex-col gap-1.5">
            <span className="text-sm font-medium">Incoming messages webhook</span>
            <CopyField value={data.inbound_url} />
            <p className="text-xs text-pretty text-muted-foreground">
              In Twilio, paste this as the &quot;A message comes in&quot; webhook, with HTTP POST, on your number, Messaging Service or WhatsApp
              sender. Replies then land in the inbox, and STOP takes the number off your lists.
            </p>
          </div>
          {c.whatsapp_from && <WhatsappTemplate connection={c} onSaved={(fresh) => mutate(fresh, { revalidate: false })} />}
          <CallingSettings status={data} onSaved={(fresh) => mutate(fresh, { revalidate: false })} />
        </div>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            In the Twilio console, copy the Account SID and auth token from Account info, and create an API key under API keys &amp; tokens. Splee
            sends with the key and checks that replies come from Twilio with the auth token.
          </p>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Account SID">
              <Input placeholder="AC…" autoComplete="off" value={form.account_sid} onChange={set("account_sid")} />
            </Field>
            <Field label="Auth token">
              <Input type="password" autoComplete="off" value={form.auth_token} onChange={set("auth_token")} />
            </Field>
            <Field label="API key SID">
              <Input placeholder="SK…" autoComplete="off" value={form.api_key_sid} onChange={set("api_key_sid")} />
            </Field>
            <Field label="API key secret">
              <Input type="password" autoComplete="off" value={form.api_secret} onChange={set("api_secret")} />
            </Field>
            <Field label="SMS sender" hint="A number like +14155550100, or a Messaging Service SID.">
              <Input placeholder="+14155550100 or MG…" autoComplete="off" value={form.sms_from} onChange={set("sms_from")} />
            </Field>
            <Field label="WhatsApp sender" hint="Optional. A number approved for WhatsApp in Twilio.">
              <Input placeholder="+14155550101" autoComplete="off" value={form.whatsapp_from} onChange={set("whatsapp_from")} />
            </Field>
          </div>
          <div className="flex justify-end">
            <Button
              type="submit"
              loading={busy}
              disabled={!form.account_sid || !form.auth_token || !form.api_key_sid || !form.api_secret || (!form.sms_from && !form.whatsapp_from)}
            >
              Connect
            </Button>
          </div>
        </form>
      )}
      <LastFailure error={c?.error ?? null} at={c?.failed_at ?? null} what="send" />
    </Section>
  );
}

/** Calls from the browser go through a TwiML App, which saving points at Splee. Calls are recorded, and transcribed when it's on. */
function CallingSettings({ status, onSaved }: { status: TwilioStatus; onSaved: (fresh: TwilioStatus) => void }) {
  const c = status.connection!;
  const local = c.local_numbers.length > 0;
  const [form, setForm] = useState({
    twiml_app_sid: c.twiml_app_sid,
    voice_from: c.voice_from,
    transcribe: c.call_transcription,
    local_presence: local,
    missed_call_text: c.missed_call_text,
  });
  const [busy, setBusy] = useState(false);
  // With local presence on, saving reads the numbers from Twilio again, so it's never a no-op.
  const changed =
    form.twiml_app_sid !== c.twiml_app_sid ||
    form.voice_from !== c.voice_from ||
    form.transcribe !== c.call_transcription ||
    form.local_presence !== local ||
    form.missed_call_text.trim() !== c.missed_call_text ||
    form.local_presence;
  const save = async (body: typeof form) => {
    setBusy(true);
    try {
      onSaved(await api<TwilioStatus>("/organization/twilio/calling", { method: "PUT", body }));
      toast.success(body.twiml_app_sid ? "Calling is on" : "Calling is off");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  const fallback = c.sms_from.startsWith("+") ? c.sms_from : "";
  return (
    <form onSubmit={submit(() => save(form))} className="flex flex-col gap-3 rounded-lg border border-border p-3">
      <div>
        <p className="text-sm font-medium">Calls</p>
        <p className="mt-0.5 text-xs text-pretty text-muted-foreground">
          Lets your team call leads from the Calls page. Create a TwiML App in Twilio under Voice, TwiML Apps, and paste its SID here. Saving
          points its Voice URL at Splee. Calls are recorded, so tell leads at the start where the law asks you to.
        </p>
        {c.twiml_app_sid && (
          <p className="mt-1.5 text-xs text-pretty text-muted-foreground">
            To take calls from leads, set &quot;A call comes in&quot; on each of your numbers to this TwiML App. They ring in Splee for the lead&apos;s
            owner, or else the whole team, and one nobody answers takes a message and adds a call back.
          </p>
        )}
      </div>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="TwiML App SID">
          <Input placeholder="AP…" autoComplete="off" value={form.twiml_app_sid} onChange={(e) => setForm({ ...form, twiml_app_sid: e.target.value.trim() })} />
        </Field>
        <Field label="Calls come from" hint={fallback ? `Leave empty to use ${fallback}.` : "A Twilio number that can make calls."}>
          <Input placeholder={fallback || "+14155550100"} autoComplete="off" value={form.voice_from} onChange={(e) => setForm({ ...form, voice_from: e.target.value.trim() })} />
        </Field>
      </div>
      <span className="flex items-start gap-2.5 text-sm">
        <Switch checked={form.local_presence} onChange={(local_presence) => setForm({ ...form, local_presence })} label="Call from a local number" />
        <span className="text-pretty">
          Call each lead from your Twilio number in their area code, or else their country
          {local && (
            <span className="mt-0.5 block text-xs text-muted-foreground">
              From {c.local_numbers.length === 1 ? "your one number" : `your ${c.local_numbers.length} numbers`}. Save again after you add
              one in Twilio.
            </span>
          )}
        </span>
      </span>
      {c.twiml_app_sid && (
        <Field
          label="Text leads you miss"
          hint={
            c.sms_from
              ? "Goes from your SMS sender to a lead who called and got nobody, once a day at most. Leave it empty to send none."
              : "Add an SMS sender to text back leads who called and got nobody."
          }
        >
          <Textarea
            maxLength={240}
            disabled={!c.sms_from}
            className="min-h-10 field-sizing-content"
            placeholder="Sorry we missed your call. We'll call you back soon, or reply here with a good time."
            value={form.missed_call_text}
            onChange={(e) => setForm({ ...form, missed_call_text: e.target.value })}
          />
        </Field>
      )}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <span className="flex items-center gap-2.5 text-sm">
          <Switch checked={form.transcribe} onChange={(transcribe) => setForm({ ...form, transcribe })} label="Transcribe calls" />
          Transcribe calls, and have the agent summarize them
        </span>
        <span className="flex gap-2">
          {c.twiml_app_sid && (
            <Button
              type="button"
              variant="ghost"
              loading={busy}
              onClick={() =>
                save({ twiml_app_sid: "", voice_from: form.voice_from, transcribe: false, local_presence: false, missed_call_text: c.missed_call_text })
              }
            >
              Turn off
            </Button>
          )}
          <Button type="submit" variant="outline" loading={busy} disabled={!changed || !form.twiml_app_sid}>
            Save
          </Button>
        </span>
      </div>
    </form>
  );
}

/** WhatsApp only takes an approved template until the lead writes back, so WhatsApp steps send this one. */
function WhatsappTemplate({ connection: c, onSaved }: { connection: NonNullable<TwilioStatus["connection"]>; onSaved: (fresh: TwilioStatus) => void }) {
  const [sid, setSid] = useState(c.whatsapp_template_sid);
  const [busy, setBusy] = useState(false);
  const save = async (template_sid: string, variables: Record<string, string>) => {
    setBusy(true);
    try {
      onSaved(await api<TwilioStatus>("/organization/twilio/template", { method: "PUT", body: { template_sid, variables } }));
      toast.success(template_sid ? "Template saved" : "Template removed");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  const vars = Object.entries(c.whatsapp_template_vars).sort(([a], [b]) => Number(a) - Number(b));
  return (
    <div className="flex flex-col gap-3 rounded-lg border border-border p-3">
      <form onSubmit={submit(() => save(sid, sid === c.whatsapp_template_sid ? c.whatsapp_template_vars : {}))}>
        <Field label="WhatsApp template" hint="The Content SID of a template WhatsApp approved, from Messaging → Content Template Builder.">
          <div className="flex flex-col gap-2 sm:flex-row">
            <Input placeholder="HX…" autoComplete="off" value={sid} onChange={(e) => setSid(e.target.value.trim())} />
            <Button type="submit" variant="outline" loading={busy} disabled={sid === c.whatsapp_template_sid}>
              Save
            </Button>
          </div>
        </Field>
      </form>
      {c.whatsapp_template_sid ? (
        <>
          <p className="rounded-lg bg-muted px-3 py-2 text-sm whitespace-pre-wrap">{c.whatsapp_template_body}</p>
          {vars.length > 0 && (
            <div className="grid gap-3 sm:grid-cols-2">
              {vars.map(([name, field]) => (
                <Field key={name} label={`{{${name}}} is the lead's`}>
                  <Select value={field} onChange={(e) => save(c.whatsapp_template_sid, { ...c.whatsapp_template_vars, [name]: e.target.value })} disabled={busy}>
                    {Object.entries(TEMPLATE_FIELDS).map(([value, label]) => (
                      <option key={value} value={value}>
                        {label}
                      </option>
                    ))}
                  </Select>
                </Field>
              ))}
            </div>
          )}
        </>
      ) : (
        <p className="text-xs text-pretty text-muted-foreground">WhatsApp steps wait until you pick one. Replies inside 24 hours of their last message can be any text.</p>
      )}
    </div>
  );
}
