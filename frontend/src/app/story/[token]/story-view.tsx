"use client";

import { useEffect, useRef, useState } from "react";
import useSWR from "swr";
import { Check, CircleAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Field, Input, Textarea } from "@/components/ui/form";
import { Favicon, Spinner } from "@/components/ui/misc";
import { OptionCards } from "@/components/app/settings/option-cards";
import { fetcher } from "@/hooks/use-me";
import { ApiError, api } from "@/lib/api";
import type { PublicStoryPage, StoryDraft } from "@/lib/types";
import { Frame } from "../../r/[token]/referral-page-view";

/** The story as it will be shared. Keep this in step with render() in backend/src/agent/stories.rs, which saves it. */
function shown(d: StoryDraft, company: string, named: boolean, name: string, job: string) {
  const hide = (s: string) => (named || !company.trim() ? s : s.replaceAll(company.trim(), d.anonymous_name));
  const anonymous = d.anonymous_name.charAt(0).toUpperCase() + d.anonymous_name.slice(1);
  const who = named
    ? [name, job, company]
        .map((s) => s.trim())
        .filter(Boolean)
        .join(", ")
    : job.trim()
      ? `${job.trim()} at ${d.anonymous_name}`
      : anonymous;
  return { title: named ? d.title : hide(d.anonymous_title), hide, who };
}

const SECTIONS = [
  ["challenge", "The challenge"],
  ["solution", "What they did"],
] as const;

/** The page a customer checks their case study on: the story as it would be shared, the quote and results to fix, whether
 * their name goes on it, and approving or saying no. `preview` is the team looking at it, which neither counts as a view
 * nor answers. */
export function StoryView({ token, preview }: { token: string; preview: boolean }) {
  const path = `/story-pages/${encodeURIComponent(token)}`;
  const { data: page, error, mutate } = useSWR<PublicStoryPage>(path, fetcher, { revalidateOnFocus: false });
  // Reported from the browser, once per load, so link checkers that never run scripts don't count.
  const reported = useRef(false);
  useEffect(() => {
    if (!page || reported.current || preview) return;
    reported.current = true;
    api(`${path}/view`, { method: "POST" }).catch(() => {});
  }, [page, path, preview]);

  if (error instanceof ApiError && error.status === 404) {
    return (
      <Frame>
        <div className="flex min-h-64 flex-col items-center justify-center gap-2 text-center text-balance">
          <CircleAlert className="size-6 text-danger" />
          <h1 className="text-xl font-medium">This page isn&apos;t here anymore</h1>
          <p className="text-sm text-muted-foreground">Ask whoever sent it for a new link.</p>
        </div>
      </Frame>
    );
  }
  if (!page) {
    return (
      <Frame>
        <div className="flex min-h-64 items-center justify-center">
          {error ? <p className="text-sm text-muted-foreground">{error.message}</p> : <Spinner />}
        </div>
      </Frame>
    );
  }
  return <Story page={page} path={path} preview={preview} onAnswered={() => mutate()} />;
}

function Story({ page, path, preview, onAnswered }: { page: PublicStoryPage; path: string; preview: boolean; onAnswered: () => unknown }) {
  const [form, setForm] = useState({
    name: page.full_name,
    title: page.job_title,
    named: page.named ?? true,
    quote: page.story.quote,
    results: page.story.results,
  });
  const [busy, setBusy] = useState<"approve" | "decline" | null>(null);
  const [failed, setFailed] = useState("");

  const approved = page.status === "approved";
  const company = page.company.trim() || page.full_name;
  const story = approved ? page.story : { ...page.story, quote: form.quote, results: form.results };
  const named = approved ? !!page.named : form.named;
  const s = shown(story, company, named, approved ? page.approved_name : form.name, approved ? page.approved_title : form.title);
  const first = page.first_name || page.full_name.split(" ")[0];
  const set = (patch: Partial<typeof form>) => setForm({ ...form, ...patch });

  const answer = async (what: "approve" | "decline") => {
    setBusy(what);
    setFailed("");
    try {
      await api(`${path}/${what}`, { method: "POST", body: what === "approve" ? form : undefined });
      await onAnswered();
    } catch (err) {
      setFailed(err instanceof Error ? err.message : "Something went wrong");
    } finally {
      setBusy(null);
    }
  };

  return (
    <Frame>
      <header className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm text-muted-foreground">
        <span className="flex items-center gap-2 font-medium text-foreground">
          {page.from_domain && <Favicon domain={page.from_domain} size={20} />}
          {page.from_name}
        </span>
        <span aria-hidden>·</span>
        <span className="flex items-center gap-2">
          {page.company_domain && <Favicon domain={page.company_domain} size={16} />}
          For {first}
        </span>
      </header>

      {approved ? (
        <p className="mt-8 flex items-start gap-2 rounded-2xl border border-brand-200 bg-brand-50 p-4 text-sm text-pretty text-brand-800">
          <Check className="mt-0.5 size-4 shrink-0" />
          Thanks, {first}. You approved it {named ? `with ${company}'s name on it` : "to share without your name"}, and {page.from_name} has it now.
        </p>
      ) : (
        <div className="mt-8">
          <h1 className="text-2xl leading-tight font-semibold tracking-tight text-balance sm:text-3xl">
            {first}, here&apos;s how we&apos;d tell your story
          </h1>
          <p className="mt-3 max-w-prose leading-relaxed text-pretty text-muted-foreground">
            Fix the quote, add your numbers and choose whether your name goes on it. Nothing is shared until you approve it.
            {page.status === "declined" && " You said no thanks, so nothing is shared. You can still approve it if you change your mind."}
          </p>
        </div>
      )}

      <article className="mt-8 rounded-2xl border border-border bg-card p-6 sm:p-8">
        <h2 className="text-2xl leading-tight font-semibold tracking-tight text-balance">{s.title}</h2>
        {story.summary && <p className="mt-3 text-lg leading-relaxed text-pretty text-muted-foreground">{s.hide(story.summary)}</p>}
        {SECTIONS.map(
          ([key, heading]) =>
            story[key] && (
              <section key={key} className="mt-6">
                <h3 className="text-sm font-semibold">{heading}</h3>
                <p className="mt-1 leading-relaxed text-pretty">{s.hide(story[key])}</p>
              </section>
            ),
        )}
        <section className="mt-6">
          <h3 className="text-sm font-semibold">The results</h3>
          {approved ? (
            <p className="mt-1 leading-relaxed text-pretty">{s.hide(story.results)}</p>
          ) : (
            <Textarea
              aria-label="The results"
              rows={4}
              maxLength={2000}
              value={form.results}
              onChange={(e) => set({ results: e.target.value })}
              placeholder="What changed for you, with numbers if you have them"
              className="mt-1.5 min-h-[calc(3lh+1.125rem)] resize-none text-base leading-relaxed field-sizing-content"
            />
          )}
        </section>
        <figure className="mt-8 border-l-2 border-brand-500 pl-4">
          {approved ? (
            <blockquote className="text-lg leading-relaxed text-pretty">&ldquo;{s.hide(story.quote)}&rdquo;</blockquote>
          ) : (
            <Textarea
              aria-label="Your quote"
              rows={3}
              maxLength={600}
              value={form.quote}
              onChange={(e) => set({ quote: e.target.value })}
              placeholder="In your own words"
              className="min-h-[calc(2lh+1.125rem)] resize-none text-lg leading-relaxed field-sizing-content"
            />
          )}
          <figcaption className="mt-2 text-sm text-muted-foreground">{s.who}</figcaption>
        </figure>
      </article>

      {!approved && !preview && (
        <section className="mt-6 flex flex-col gap-5 rounded-2xl border border-border bg-card p-6">
          <OptionCards
            name="named"
            value={form.named ? "named" : "anonymous"}
            onChange={(v) => set({ named: v === "named" })}
            className="@lg:grid-cols-2"
            options={[
              { value: "named", label: `Name ${company}`, description: "Your company and your name go on it." },
              { value: "anonymous", label: "Keep us anonymous", description: `It says ${story.anonymous_name} instead, and leaves your name out.` },
            ]}
          />
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Your name">
              <Input required value={form.name} onChange={(e) => set({ name: e.target.value })} autoComplete="name" maxLength={120} />
            </Field>
            <Field label="Your title (optional)">
              <Input value={form.title} onChange={(e) => set({ title: e.target.value })} autoComplete="organization-title" maxLength={120} />
            </Field>
          </div>
          {failed && <p className="text-sm text-danger">{failed}</p>}
          <div className="flex flex-wrap items-center justify-end gap-3">
            {page.status !== "declined" && (
              <Button type="button" variant="ghost" onClick={() => answer("decline")} loading={busy === "decline"} disabled={!!busy}>
                No thanks
              </Button>
            )}
            <Button type="button" onClick={() => answer("approve")} loading={busy === "approve"} disabled={!!busy || !form.name.trim()}>
              <Check className="size-4" /> Approve it
            </Button>
          </div>
        </section>
      )}
    </Frame>
  );
}
