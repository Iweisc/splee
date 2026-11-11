"use client";

import Link from "next/link";
import { use, useState } from "react";
import useSWR from "swr";
import { toast } from "sonner";
import { MessageCircle, X } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { ChipInput } from "@/components/app/settings/chip-input";
import { CodeBlock } from "@/components/app/settings/code-block";
import { LoadError, Section, SectionsSkeleton, SettingsColumn } from "@/components/app/settings/layout";
import { SaveBar } from "@/components/app/settings/save-bar";
import { toastError } from "@/components/app/settings/use-action";
import { Field, Input, NumberInput, Switch, Textarea } from "@/components/ui/form";
import { Tabs } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { useOrigin } from "@/hooks/use-origin";
import { api } from "@/lib/api";
import type { ChatSettings, ChatSettingsPage, VisitorSite } from "@/lib/types";
import { cn, money, num } from "@/lib/utils";

const FAQ_MAX = 8000;
const HEX = /^#[0-9a-fA-F]{6}$/;

type Draft = ChatSettings & { domains: string[] };

export default function WebsiteChatPage({ params }: PageProps<"/app/p/[projectId]/website-chat">) {
  const { projectId } = use(params);
  const origin = useOrigin();
  const key = `/projects/${projectId}/chat`;
  const { data, error, mutate } = useSWR<ChatSettingsPage>(key, fetcher);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [saving, setSaving] = useState(false);
  const saved: Draft | undefined = data && { ...data.settings, domains: data.site.domains };
  const s = draft ?? saved;
  const dirty = !!draft && JSON.stringify(draft) !== JSON.stringify(saved);
  const update = (patch: Partial<Draft>) => s && setDraft({ ...s, ...patch });

  const save = async () => {
    if (!draft || !data) return;
    setSaving(true);
    try {
      const { domains, ...settings } = draft;
      // The domains belong to the visitors snippet too, so they're saved through its route.
      if (JSON.stringify(domains) !== JSON.stringify(data.site.domains)) {
        await api<VisitorSite>(`/projects/${projectId}/visitors/site`, { method: "PUT", body: { domains } });
      }
      await mutate(await api<ChatSettingsPage>(key, { method: "PUT", body: settings }), { revalidate: false });
      setDraft(null);
      toast.success("Settings saved");
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };

  const problem = !s
    ? null
    : !HEX.test(s.accent)
      ? "Pick the colour as a hex code, like #2563eb"
      : !s.greeting.trim()
        ? "Write a greeting"
        : s.faq.length > FAQ_MAX
          ? `Keep the FAQ under ${num(FAQ_MAX)} characters`
          : s.domains.length === 0
            ? "Add the domain your site is on"
            : null;

  return (
    <>
      <PageHeader
        title="Website chat"
        description="A chat on your site that answers visitors from your product description, materials and FAQ, and turns interested ones into leads."
      />
      <PageBody>
        <SettingsColumn>
          {error ? (
            <LoadError error={error} what="the chat settings" retry={() => mutate()} />
          ) : !s || !data ? (
            <SectionsSkeleton count={3} />
          ) : (
            <>
              <Section
                title="Chat on your site"
                description={s.enabled ? "Visitors on your domains see the chat button." : "Off. The script draws nothing until you turn it on."}
                actions={<Switch checked={s.enabled} onChange={(enabled) => update({ enabled })} label="Chat on your site" />}
              >
                <div className="flex flex-col gap-4">
                  <CodeBlock code={`<script async src="${origin}/api/c/${data.site.site_key}.js"></script>`} label="Add to every page" wrap />
                  <Field
                    label="Your site's domains"
                    hint={<span className="text-pretty">The chat only answers on these and their subdomains. The visitors snippet uses the same list.</span>}
                    group
                  >
                    <ChipInput value={s.domains} onChange={(domains) => update({ domains })} placeholder="example.com" />
                  </Field>
                </div>
              </Section>

              <Section title="Appearance" description="How the chat looks on your site, and where it shows.">
                <div className="grid gap-6 md:grid-cols-[minmax(0,1fr)_280px]">
                  <div className="flex min-w-0 flex-col gap-4">
                    <Field label="Accent colour">
                      <div className="flex items-center gap-2">
                        <input
                          type="color"
                          value={HEX.test(s.accent) ? s.accent : "#111111"}
                          onChange={(e) => update({ accent: e.target.value })}
                          className="h-10 w-12 shrink-0 cursor-pointer rounded-lg border border-border-strong bg-card p-1"
                          aria-label="Pick a colour"
                        />
                        <Input value={s.accent} onChange={(e) => update({ accent: e.target.value.trim() })} className="font-mono" maxLength={7} />
                      </div>
                    </Field>
                    <Field label="Button position" group>
                      <Tabs
                        value={s.position}
                        onChange={(position) => update({ position })}
                        options={[
                          { value: "right", label: "Bottom right" },
                          { value: "left", label: "Bottom left" },
                        ]}
                        className="self-start"
                      />
                    </Field>
                    <Field label="Greeting" hint="The first message visitors see when they open the chat.">
                      <Input value={s.greeting} onChange={(e) => update({ greeting: e.target.value })} maxLength={300} />
                    </Field>
                    <Field label="Pages it shows on" hint={<span className="text-pretty">Each page counts with the pages under it. Leave it empty for every page.</span>} group>
                      <ChipInput value={s.paths} onChange={(paths) => update({ paths })} placeholder="/pricing, /docs" />
                    </Field>
                  </div>
                  <Preview settings={s} title={data.title} />
                </div>
              </Section>

              <Section
                title="FAQ"
                description={
                  <>
                    The chat answers only from your product description, your <Link href={`/app/p/${projectId}/materials`} className="underline">materials</Link>{" "}
                    and these answers. When none of them cover a question, it says so and asks for an email.
                  </>
                }
              >
                <Field label="Questions and answers" hint={`${num(s.faq.length)} of ${num(FAQ_MAX)} characters`}>
                  <Textarea
                    value={s.faq}
                    onChange={(e) => update({ faq: e.target.value })}
                    rows={8}
                    placeholder={"Q: Is there a free trial?\nA: Yes, 14 days, no card needed."}
                  />
                </Field>
              </Section>

              <Section title="Limits" description="Each answer is one call to a small Claude model. Past either limit, the chat asks visitors for their email instead.">
                <div className="grid gap-4 sm:grid-cols-2">
                  <Field label="Answers per day" hint={`${num(data.usage.today_messages)} today, counted in UTC`}>
                    <NumberInput min={1} max={5000} step={1} value={s.daily_messages} onValueChange={(n) => update({ daily_messages: Math.round(n) })} />
                  </Field>
                  <Field label="Spend per month" hint={`${money(data.usage.month_spend_micros)} so far this month`}>
                    <div className="relative">
                      <span className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-sm text-muted-foreground">$</span>
                      <NumberInput
                        min={0}
                        max={1000}
                        step={1}
                        value={s.monthly_budget_micros / 1_000_000}
                        onValueChange={(n) => update({ monthly_budget_micros: Math.round(n * 100) * 10_000 })}
                        className="pl-7 tabular-nums"
                      />
                    </div>
                  </Field>
                </div>
              </Section>
            </>
          )}
          <SaveBar form={{ dirty, saving, save, reset: () => setDraft(null) }} problem={problem} />
        </SettingsColumn>
      </PageBody>
    </>
  );
}

/** The open chat as visitors see it, drawn here rather than by the real script, which would need one of your domains. */
function Preview({ settings, title }: { settings: Draft; title: string }) {
  const accent = HEX.test(settings.accent) ? settings.accent : "#111111";
  const left = settings.position === "left";
  return (
    <div aria-label="Preview" className="flex flex-col gap-1.5">
      <span className="text-sm font-medium">Preview</span>
      <div className="flex flex-col gap-3 rounded-2xl bg-black/[0.04] p-3">
        <div className="flex flex-col overflow-hidden rounded-xl bg-white shadow-pop">
          <div className="flex items-center gap-2 px-3 py-2.5 text-sm font-semibold text-white" style={{ background: accent }}>
            <span className="flex-1 truncate">{title || "Your company"}</span>
            <X className="size-4" aria-hidden />
          </div>
          <div className="flex flex-col gap-2 bg-[#f7f7f8] p-3 text-[13px]">
            <p className="max-w-[85%] self-start rounded-xl rounded-bl-sm border border-black/10 bg-white px-2.5 py-1.5 text-pretty">{settings.greeting}</p>
            <p className="max-w-[85%] self-end rounded-xl rounded-br-sm px-2.5 py-1.5 text-white" style={{ background: accent }}>
              Do you have a free trial?
            </p>
          </div>
          <div className="flex gap-2 border-t border-black/5 p-2">
            <span className="flex-1 rounded-lg border border-black/10 px-2 py-1.5 text-xs text-muted-foreground">Type your question</span>
            <span className="rounded-lg px-2.5 py-1.5 text-xs font-semibold text-white" style={{ background: accent }}>
              Send
            </span>
          </div>
        </div>
        <span
          className={cn("grid size-11 place-items-center rounded-full text-white shadow-pop", left ? "self-start" : "self-end")}
          style={{ background: accent }}
        >
          <MessageCircle className="size-5" aria-hidden />
        </span>
      </div>
    </div>
  );
}
