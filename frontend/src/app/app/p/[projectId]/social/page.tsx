"use client";

import { use, useState } from "react";
import { Check, ExternalLink, Megaphone, Send, SkipForward, Sparkles, Trash2, Undo2 } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { isActive, RunProgress, useProjectRuns } from "@/components/app/work/run-progress";
import { LoadError, MicroLabel, toastError } from "@/components/app/work/states";
import { Button, LinkButton } from "@/components/ui/button";
import { Field, Input, Select, Textarea } from "@/components/ui/form";
import { Badge, Card, EmptyState, Message, Skeleton, Tabs, type BadgeTone } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { ProjectSettings, SocialPost } from "@/lib/types";
import { cn, dateTime, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { useAgentEvents } from "@/hooks/use-agent-events";

type Platform = SocialPost["platform"];
type Tab = "draft" | "approved" | "posted" | "closed";

const PLATFORMS: Record<Platform, string> = { x: "X", reddit: "Reddit", bluesky: "Bluesky", mastodon: "Mastodon", linkedin: "LinkedIn" };
const TABS: { value: Tab; label: string; statuses: SocialPost["status"][] }[] = [
  { value: "draft", label: "Drafts", statuses: ["draft"] },
  { value: "approved", label: "Approved", statuses: ["approved", "posting"] },
  { value: "posted", label: "Posted", statuses: ["posted"] },
  { value: "closed", label: "Failed or skipped", statuses: ["failed", "skipped"] },
];
const EMPTY: Record<Tab, { title: string; text: string }> = {
  draft: { title: "No drafts", text: "Find threads to join, and the agent's replies and posts land here for you to check." },
  approved: { title: "Nothing waiting to post", text: "Approved replies and posts go out from your accounts every ten minutes, up to 15 a day each, or 5 on LinkedIn." },
  posted: { title: "Nothing posted yet", text: "Posts show up here with a link once they're live." },
  closed: { title: "Nothing failed or skipped", text: "Posts you skip, and ones a platform turns down, end up here." },
};

/** LinkedIn goes out through the Unipile connection for LinkedIn sending. */
const isConnected = (s: ProjectSettings, p: Platform) => (p === "linkedin" ? s.unipile_connected : s[`${p}_connected`]);
const dailyCap = (p: Platform) => (p === "linkedin" ? 5 : 15);

/** The most a reply or post can hold, like social::max_length on the backend. Reddit's limits are far above any reply, so it
 *  gets no counter. */
function maxLength(p: Platform, reply: boolean, mastodonChars: number): number | null {
  if (p === "x") return 280;
  if (p === "bluesky") return 300;
  if (p === "mastodon") return mastodonChars;
  if (p === "linkedin") return reply ? 1250 : 3000;
  return null;
}

/** Bluesky counts graphemes, so an emoji with a skin tone counts once. The others count characters. */
const graphemes = new Intl.Segmenter();
const length = (p: Platform, s: string) => (p === "bluesky" ? [...graphemes.segment(s)].length : [...s].length);

export default function SocialPage({ params }: PageProps<"/app/p/[projectId]/social">) {
  const { projectId } = use(params);
  const { data: settings, error: settingsError } = useSWR<ProjectSettings>(`/projects/${projectId}/settings`, fetcher);
  const { data, error, mutate } = useSWR<SocialPost[]>(`/projects/${projectId}/social`, fetcher);
  const { runs, mutate: refreshRuns } = useProjectRuns(projectId);
  const run = runs?.find((r) => r.kind === "social" && isActive(r)) ?? null;
  const [tab, setTab] = useState<Tab>("draft");
  const [starting, setStarting] = useState(false);

  // RunProgress doesn't show the run's final message, so the count of drafts comes as a toast.
  useAgentEvents(run ? `/api/runs/${run.id}/events` : null, (e) => {
    if (e.kind === "done") toast.success(e.message);
    if (e.kind === "error") toast.error(e.message || "The agent couldn't finish");
    if (e.kind === "done" || e.kind === "error") {
      setTab("draft");
      mutate();
    }
  });

  const connected = settings ? (Object.keys(PLATFORMS) as Platform[]).filter((p) => isConnected(settings, p)) : [];
  const start = async () => {
    setStarting(true);
    try {
      await api<{ run_id: string }>(`/projects/${projectId}/social/runs`, { method: "POST" });
      await refreshRuns();
    } catch (e) {
      toastError(e);
    } finally {
      setStarting(false);
    }
  };

  const shown = TABS.find((t) => t.value === tab)!;
  const posts = (data ?? []).filter((p) => shown.statuses.includes(p.status));
  const count = (t: (typeof TABS)[number]) => (data ?? []).filter((p) => t.statuses.includes(p.status)).length;

  return (
    <>
      <PageHeader
        title="Social"
        description="The agent finds recent threads on X, Reddit, Bluesky, Mastodon and LinkedIn where your buyers talk about the problem you solve, and drafts helpful replies and a few posts. Splee posts the ones you approve from your own accounts."
        actions={
          connected.length > 0 && (
            <Button onClick={start} loading={starting} disabled={!!run}>
              <Sparkles className="size-4" /> Find threads to join
            </Button>
          )
        }
      />
      <PageBody className="flex flex-col gap-5">
        {settingsError || error ? (
          <Card>
            <LoadError error={settingsError ?? error} retry={() => mutate()} />
          </Card>
        ) : !settings || !data ? (
          <div className="flex flex-col gap-3">
            {Array.from({ length: 3 }, (_, i) => (
              <Skeleton key={i} className="h-40 rounded-2xl" />
            ))}
          </div>
        ) : connected.length === 0 && data.length === 0 ? (
          <Card>
            <EmptyState
              icon={<Megaphone className="size-5" />}
              title="Connect an account first"
              action={
                <LinkButton href={`/app/p/${projectId}/integrations`} variant="outline" size="sm">
                  Go to integrations
                </LinkButton>
              }
            >
              Splee searches, drafts and posts as your own accounts. Connect X, Reddit, Bluesky or Mastodon, or LinkedIn sending, on the integrations page.
            </EmptyState>
          </Card>
        ) : (
          <>
            {run && <RunProgress key={run.id} run={run} />}
            {connected.length > 0 && <Composer projectId={projectId} platforms={connected} mastodonChars={settings.mastodon_max_chars} onAdded={() => mutate()} />}
            {/* Four tabs with counts don't fit a phone's width, so phones get a select instead. */}
            <div>
              <Tabs value={tab} onChange={setTab} options={TABS.map((t) => ({ value: t.value, label: `${t.label} (${count(t)})` }))} className="max-sm:hidden" />
              <Select value={tab} onChange={(e) => setTab(e.target.value as Tab)} aria-label="Show" className="sm:hidden">
                {TABS.map((t) => (
                  <option key={t.value} value={t.value}>
                    {t.label} ({count(t)})
                  </option>
                ))}
              </Select>
            </div>
            {posts.length === 0 ? (
              <Card>
                <EmptyState icon={<Megaphone className="size-5" />} title={EMPTY[tab].title}>
                  {EMPTY[tab].text}
                </EmptyState>
              </Card>
            ) : (
              <ul className="flex flex-col gap-3">
                {posts.map((p) => (
                  <PostCard key={p.id} post={p} mastodonChars={settings.mastodon_max_chars} onChange={() => mutate()} />
                ))}
              </ul>
            )}
          </>
        )}
      </PageBody>
    </>
  );
}

/** A post written by hand, which goes straight to Approved. */
function Composer({
  projectId,
  platforms,
  mastodonChars,
  onAdded,
}: {
  projectId: string;
  platforms: Platform[];
  mastodonChars: number;
  onAdded: () => Promise<unknown>;
}) {
  const [platform, setPlatform] = useState<Platform>(platforms[0]!);
  const [body, setBody] = useState("");
  const [title, setTitle] = useState("");
  const [subreddit, setSubreddit] = useState("");
  const [busy, setBusy] = useState(false);
  const on = platforms.includes(platform) ? platform : platforms[0]!;
  const reddit = on === "reddit";
  const max = maxLength(on, false, mastodonChars);
  const ready = !!body.trim() && (reddit ? !!title.trim() && !!subreddit.trim() : max === null || length(on, body) <= max);

  const add = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      await api(`/projects/${projectId}/social`, { method: "POST", body: { platform: on, body, title, subreddit, status: "approved" } });
      await onAdded();
      setBody("");
      setTitle("");
      toast.success(`Queued for ${PLATFORMS[on]}`);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card className="p-4 sm:p-5">
      <form onSubmit={add} className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="font-medium">Write a post</p>
          {/* Five platforms don't fit next to the heading on a phone, so phones get a select instead. */}
          {platforms.length > 1 && (
            <>
              <Tabs value={on} onChange={setPlatform} options={platforms.map((p) => ({ value: p, label: PLATFORMS[p] }))} className="max-sm:hidden" />
              <Select value={on} onChange={(e) => setPlatform(e.target.value as Platform)} aria-label="Platform" className="w-auto sm:hidden">
                {platforms.map((p) => (
                  <option key={p} value={p}>
                    {PLATFORMS[p]}
                  </option>
                ))}
              </Select>
            </>
          )}
        </div>
        {reddit && (
          <div className="flex flex-col gap-3 sm:flex-row">
            <Field label="Subreddit" className="sm:w-48">
              <Input placeholder="r/sales" autoComplete="off" value={subreddit} onChange={(e) => setSubreddit(e.target.value)} />
            </Field>
            <Field label="Title" className="flex-1">
              <Input autoComplete="off" maxLength={300} value={title} onChange={(e) => setTitle(e.target.value)} />
            </Field>
          </div>
        )}
        <Textarea
          aria-label="Post"
          rows={3}
          placeholder={reddit ? "What you want to share or ask" : on === "linkedin" ? "What you want to share" : "What's happening"}
          value={body}
          onChange={(e) => setBody(e.target.value)}
        />
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="text-xs text-pretty text-muted-foreground">
            {max !== null && (
              <>
                <CharCount n={length(on, body)} max={max} /> ·{" "}
              </>
            )}
            Goes out within ten minutes, up to {dailyCap(on)} a day.
          </p>
          <Button type="submit" size="sm" loading={busy} disabled={!ready} className="max-sm:ml-auto">
            <Send className="size-3.5" /> Queue post
          </Button>
        </div>
      </form>
    </Card>
  );
}

function CharCount({ n, max }: { n: number; max: number }) {
  return (
    <span className={cn("tabular-nums", n > max && "font-medium text-danger")}>
      {n}/{max}
    </span>
  );
}

const STATUS: Record<SocialPost["status"], { label: string; tone: BadgeTone }> = {
  draft: { label: "Draft", tone: "neutral" },
  approved: { label: "Approved", tone: "blue" },
  posting: { label: "Posting", tone: "amber" },
  posted: { label: "Posted", tone: "green" },
  failed: { label: "Failed", tone: "red" },
  skipped: { label: "Skipped", tone: "neutral" },
};

/** A draft stays editable until it's posted. Every button saves the edits along with the new status. */
function PostCard({ post: p, mastodonChars, onChange }: { post: SocialPost; mastodonChars: number; onChange: () => Promise<unknown> }) {
  const [body, setBody] = useState(p.body);
  const [title, setTitle] = useState(p.title);
  const [subreddit, setSubreddit] = useState(p.subreddit);
  const [saving, setSaving] = useState<string | null>(null);
  const reply = !!p.in_reply_to;
  const redditPost = p.platform === "reddit" && !reply;
  const posted = p.status === "posted";
  const dirty = body !== p.body || title !== p.title || subreddit !== p.subreddit;
  const max = maxLength(p.platform, reply, mastodonChars);
  const tooLong = max !== null && length(p.platform, body) > max;
  // LinkedIn calls a reply a comment.
  const kind = reply ? (p.platform === "linkedin" ? "comment" : "reply") : "post";

  const save = async (status?: SocialPost["status"]) => {
    setSaving(status ?? "save");
    try {
      const fresh = await api<SocialPost>(`/social/${p.id}`, { method: "PATCH", body: { body, title, subreddit, status } });
      // The backend trims the text and writes "r/sales" as "sales", which would otherwise leave the card looking unsaved.
      setBody(fresh.body);
      setTitle(fresh.title);
      setSubreddit(fresh.subreddit);
      await onChange();
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(null);
    }
  };
  const remove = async () => {
    if (!confirm("Delete this draft? The agent may draft a reply to this thread again.")) return;
    setSaving("delete");
    try {
      await api(`/social/${p.id}`, { method: "DELETE" });
      await onChange();
    } catch (e) {
      toastError(e);
      setSaving(null);
    }
  };

  return (
    <li>
      <Card className="flex flex-col gap-3 p-4 wrap-anywhere sm:p-5">
        <div className="flex flex-wrap items-center gap-2">
          <Badge tone="dark">{PLATFORMS[p.platform]}</Badge>
          <span className="text-sm font-medium capitalize">{kind}</span>
          {p.subreddit && <span className="text-sm text-muted-foreground">r/{p.subreddit}</span>}
          <span className="text-xs text-muted-foreground" title={dateTime(p.posted_at ?? p.created_at)}>
            {posted ? `Posted ${timeAgo(p.posted_at ?? p.created_at)}` : timeAgo(p.created_at)}
          </span>
          {p.status !== "draft" && <Badge tone={STATUS[p.status].tone} className="ml-auto">{STATUS[p.status].label}</Badge>}
        </div>

        {reply && (
          <div className="rounded-lg bg-muted/60 px-3 py-2.5">
            <a href={p.in_reply_to!} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 text-xs font-medium hover:underline">
              {p.platform === "linkedin" ? "Commenting on this post" : "Replying to this thread"} on {PLATFORMS[p.platform]} <ExternalLink className="size-3" />
            </a>
            {p.context && <p className="mt-1 line-clamp-4 text-sm whitespace-pre-wrap text-muted-foreground">{p.context}</p>}
          </div>
        )}
        {p.reason && (
          <p className="text-sm text-pretty text-muted-foreground">
            <span className="font-medium text-foreground">Why </span>
            {p.reason}
          </p>
        )}

        {posted ? (
          <>
            {p.title && <p className="font-medium">{p.title}</p>}
            <p className="text-sm whitespace-pre-wrap">{p.body}</p>
            {p.url && (
              <a href={p.url} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 self-start text-sm hover:underline">
                View on {PLATFORMS[p.platform]} <ExternalLink className="size-3.5" />
              </a>
            )}
          </>
        ) : (
          <>
            {redditPost && (
              <div className="flex flex-col gap-3 sm:flex-row">
                <Field label="Subreddit" className="sm:w-48">
                  <Input autoComplete="off" value={subreddit} onChange={(e) => setSubreddit(e.target.value)} />
                </Field>
                <Field label="Title" className="flex-1">
                  <Input autoComplete="off" maxLength={300} value={title} onChange={(e) => setTitle(e.target.value)} />
                </Field>
              </div>
            )}
            <div className="flex flex-col gap-1">
              <MicroLabel>Your {kind}</MicroLabel>
              {/* Grows with the text, so a long reply can be read without scrolling inside the box. */}
              <Textarea
                aria-label={`Your ${kind}`}
                rows={3}
                value={body}
                onChange={(e) => setBody(e.target.value)}
                className="field-sizing-content"
              />
              {max !== null && (
                <p className="text-right text-xs text-muted-foreground">
                  <CharCount n={length(p.platform, body)} max={max} />
                </p>
              )}
            </div>
          </>
        )}

        {p.status === "failed" && p.error && (
          <p className="rounded-lg bg-red-50 px-2.5 py-1.5 text-xs text-red-700">
            Splee couldn&apos;t post this: <Message text={p.error.replace(/\.+$/, "")} />.
          </p>
        )}

        {!posted && (
          <div className="flex flex-wrap items-center gap-1">
            {p.status === "draft" && (
              <Button size="sm" variant="ghost" onClick={remove} loading={saving === "delete"} disabled={!!saving} className="px-2">
                <Trash2 className="size-3.5" /> Delete
              </Button>
            )}
            {p.status !== "draft" && (
              <Button size="sm" variant="ghost" onClick={() => save("draft")} loading={saving === "draft"} disabled={!!saving} className="px-2">
                <Undo2 className="size-3.5" /> Back to drafts
              </Button>
            )}
            <span className="flex-1" />
            {/* Failed and skipped posts keep their edits through Approve again. */}
            {dirty && (p.status === "draft" || p.status === "approved") && (
              <Button size="sm" variant="ghost" onClick={() => save()} loading={saving === "save"} disabled={!!saving || (p.status === "approved" && tooLong)}>
                Save
              </Button>
            )}
            {p.status !== "skipped" && (
              <Button size="sm" variant="ghost" onClick={() => save("skipped")} loading={saving === "skipped"} disabled={!!saving} className="px-2">
                <SkipForward className="size-3.5" /> Skip
              </Button>
            )}
            {p.status !== "approved" && (
              <Button size="sm" variant="outline" onClick={() => save("approved")} loading={saving === "approved"} disabled={!!saving || tooLong}>
                <Check className="size-3.5" /> {p.status === "draft" ? "Approve" : "Approve again"}
              </Button>
            )}
          </div>
        )}
      </Card>
    </li>
  );
}
