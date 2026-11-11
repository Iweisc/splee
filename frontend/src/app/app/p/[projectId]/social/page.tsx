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
import { Badge, Card, EmptyState, Host, Message, Skeleton, Tabs, type BadgeTone } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { DiscourseForum, ProjectSettings, SocialPost } from "@/lib/types";
import { cn, dateTime, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { useAgentEvents } from "@/hooks/use-agent-events";

type Platform = SocialPost["platform"];
type Tab = "draft" | "approved" | "posted" | "closed";

const PLATFORMS: Record<Platform, string> = {
  x: "X",
  reddit: "Reddit",
  bluesky: "Bluesky",
  mastodon: "Mastodon",
  threads: "Threads",
  linkedin: "LinkedIn",
  youtube: "YouTube",
  stackexchange: "Stack Exchange",
  dev: "DEV",
  discourse: "Discourse",
  lemmy: "Lemmy",
  nostr: "Nostr",
  tumblr: "Tumblr",
  hashnode: "Hashnode",
};
const TABS: { value: Tab; label: string; statuses: SocialPost["status"][] }[] = [
  { value: "draft", label: "Drafts", statuses: ["draft"] },
  { value: "approved", label: "Approved", statuses: ["approved", "posting"] },
  { value: "posted", label: "Posted", statuses: ["posted"] },
  { value: "closed", label: "Failed or skipped", statuses: ["failed", "skipped"] },
];
const EMPTY: Record<Tab, { title: string; text: string }> = {
  draft: { title: "No drafts", text: "Find threads to join, and the agent's replies and posts land here for you to check." },
  approved: {
    title: "Nothing waiting to post",
    text: "Approved replies and posts go out from your accounts every ten minutes, up to 15 a day each. Lemmy and Tumblr take 10, LinkedIn, Stack Exchange and each Discourse forum 5, DEV 2 articles, and Hashnode 2 articles and 10 comments.",
  },
  posted: { title: "Nothing posted yet", text: "Posts show up here with a link once they're live." },
  closed: { title: "Nothing failed or skipped", text: "Posts you skip, and ones a platform turns down, end up here." },
};

/** LinkedIn goes out through the Unipile connection for LinkedIn sending, and Discourse through each forum the project joined. */
function isConnected(s: ProjectSettings, forums: DiscourseForum[], p: Platform) {
  if (p === "linkedin") return s.unipile_connected;
  if (p === "discourse") return forums.length > 0;
  return s[`${p}_connected`];
}
/** Original posts a day, like social::daily_cap on the backend. Discourse's is per forum. */
const dailyCap = (p: Platform) =>
  p === "linkedin" || p === "stackexchange" || p === "discourse" ? 5 : p === "lemmy" || p === "tumblr" ? 10 : p === "dev" || p === "hashnode" ? 2 : 15;
/** YouTube only takes comments and Stack Exchange answers, like social::takes_posts on the backend. */
const takesPosts = (p: Platform) => p !== "youtube" && p !== "stackexchange";
/** Stack Exchange and Discourse turn down short ones, like social::min_length on the backend. */
const minLength = (p: Platform) => (p === "stackexchange" ? 30 : p === "discourse" ? 20 : 1);
/** Like dev::TITLE_CHARS, hashnode::TITLE_CHARS, lemmy::TITLE_CHARS and discourse's title limits on the backend. */
const ARTICLE_TITLE_CHARS: Partial<Record<Platform, number>> = { dev: 128, hashnode: 250 };
/** Like dev::MAX_TAGS, hashnode::MAX_TAGS and tumblr::MAX_TAGS on the backend. */
const MAX_TAGS: Partial<Record<Platform, number>> = { dev: 4, hashnode: 5, tumblr: 10 };
const LEMMY_TITLE_CHARS = 200;
const TOPIC_TITLE_CHARS = [15, 255] as const;
/** Discourse forums only take new topics once a category is picked for them. */
const topicForums = (forums: DiscourseForum[]) => forums.filter((f) => f.category_id !== null);
const forumHost = (f: DiscourseForum) => f.url.replace(/^https:\/\//, "");
/** What a new post needs besides its text, like the backend's check. `community` is the subreddit or Lemmy community. */
function titleReady(p: Platform, title: string, community: string, forum: DiscourseForum | undefined) {
  const n = [...title.trim()].length;
  if (p === "reddit" || p === "lemmy") return n > 0 && n <= (p === "lemmy" ? LEMMY_TITLE_CHARS : 300) && !!community.trim();
  if (p === "dev" || p === "hashnode") return n > 0 && n <= ARTICLE_TITLE_CHARS[p]!;
  if (p === "discourse") return !!forum?.category_id && n >= TOPIC_TITLE_CHARS[0] && n <= TOPIC_TITLE_CHARS[1];
  return true;
}
/** Tags as typed, like "webdev, productivity". The backend keeps them the way each platform takes them, up to its limit. */
const tagList = (s: string) => s.split(",").map((t) => t.trim()).filter(Boolean);

/** The most a reply or post can hold, like social::max_length on the backend. Reddit's, YouTube's, Stack Exchange's, DEV's and
 *  Hashnode's limits are far above any reply, so they get no counter. */
function maxLength(p: Platform, reply: boolean, mastodonChars: number): number | null {
  if (p === "x") return 280;
  if (p === "bluesky") return 300;
  if (p === "mastodon") return mastodonChars;
  if (p === "threads") return 500;
  if (p === "nostr") return 5000;
  if (p === "tumblr") return 4096;
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
  const { data: forums, error: forumsError } = useSWR<DiscourseForum[]>(`/projects/${projectId}/discourse-forums`, fetcher);
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

  const connected = settings && forums ? (Object.keys(PLATFORMS) as Platform[]).filter((p) => isConnected(settings, forums, p)) : [];
  // Discourse takes topics only on forums with a category, and Hashnode articles only once a publication is picked.
  const posting = connected.filter(
    (p) => takesPosts(p) && (p !== "discourse" || topicForums(forums ?? []).length > 0) && (p !== "hashnode" || !!settings?.hashnode_publication_id),
  );
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
        description="The agent finds recent threads on X, Reddit, Bluesky, Mastodon, Threads, Lemmy, Nostr, Tumblr, Discourse forums and LinkedIn, articles on Hashnode, videos on YouTube and questions on Stack Exchange where your buyers talk about the problem you solve, and drafts helpful replies, a few posts and articles. Splee posts the ones you approve from your own accounts."
        actions={
          connected.length > 0 && (
            <Button onClick={start} loading={starting} disabled={!!run}>
              <Sparkles className="size-4" /> Find threads to join
            </Button>
          )
        }
      />
      <PageBody className="flex flex-col gap-5">
        {settingsError || error || forumsError ? (
          <Card>
            <LoadError error={settingsError ?? error ?? forumsError} retry={() => mutate()} />
          </Card>
        ) : !settings || !data || !forums ? (
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
              Splee searches, drafts and posts as your own accounts. Connect X, Reddit, Bluesky, Mastodon, Threads, YouTube, Stack Exchange, DEV,
              Lemmy, Nostr, Tumblr, Hashnode or a Discourse forum, or LinkedIn sending, on the integrations page.
            </EmptyState>
          </Card>
        ) : (
          <>
            {run && <RunProgress key={run.id} run={run} />}
            {posting.length > 0 && (
              <Composer
                projectId={projectId}
                platforms={posting}
                forums={topicForums(forums)}
                mastodonChars={settings.mastodon_max_chars}
                onAdded={() => mutate()}
              />
            )}
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
                  <PostCard
                    key={p.id}
                    post={p}
                    forum={forums.find((f) => f.id === p.forum_id)}
                    mastodonChars={settings.mastodon_max_chars}
                    onChange={() => mutate()}
                  />
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
  forums,
  mastodonChars,
  onAdded,
}: {
  projectId: string;
  platforms: Platform[];
  /** The Discourse forums with a category for new topics. */
  forums: DiscourseForum[];
  mastodonChars: number;
  onAdded: () => Promise<unknown>;
}) {
  const [platform, setPlatform] = useState<Platform>(platforms[0]!);
  const [body, setBody] = useState("");
  const [title, setTitle] = useState("");
  const [subreddit, setSubreddit] = useState("");
  const [community, setCommunity] = useState("");
  const [forumId, setForumId] = useState("");
  const [tags, setTags] = useState("");
  const [busy, setBusy] = useState(false);
  const on = platforms.includes(platform) ? platform : platforms[0]!;
  const reddit = on === "reddit";
  const dev = on === "dev" || on === "hashnode";
  const lemmy = on === "lemmy";
  const discourse = on === "discourse";
  const forum = forums.find((f) => f.id === forumId) ?? forums[0];
  const max = maxLength(on, false, mastodonChars);
  const ready =
    !!body.trim() &&
    [...body.trim()].length >= minLength(on) &&
    (max === null || length(on, body) <= max) &&
    titleReady(on, title, lemmy ? community : subreddit, forum);

  const add = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      const where = { subreddit: lemmy ? community : subreddit, forum_id: discourse ? forum?.id : undefined };
      await api(`/projects/${projectId}/social`, { method: "POST", body: { platform: on, body, title, ...where, tags: tagList(tags), status: "approved" } });
      await onAdded();
      setBody("");
      setTitle("");
      setTags("");
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
          <p className="font-medium">{dev ? "Write an article" : discourse ? "Start a topic" : "Write a post"}</p>
          {/* Several platforms don't fit next to the heading on a phone, so phones get a select instead. */}
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
        {lemmy && <CommunityFields community={community} title={title} setCommunity={setCommunity} setTitle={setTitle} />}
        {discourse && (
          <div className="flex flex-col gap-3 sm:flex-row">
            {forums.length > 1 && (
              <Field label="Forum" className="sm:w-56">
                <Select value={forum?.id} onChange={(e) => setForumId(e.target.value)}>
                  {forums.map((f) => (
                    <option key={f.id} value={f.id}>
                      {forumHost(f)}
                    </option>
                  ))}
                </Select>
              </Field>
            )}
            <TopicTitle title={title} setTitle={setTitle} />
          </div>
        )}
        {dev && <ArticleFields platform={on} title={title} tags={tags} setTitle={setTitle} setTags={setTags} />}
        {on === "tumblr" && <TagsField platform={on} tags={tags} setTags={setTags} />}
        <Textarea
          aria-label={dev ? "Article" : discourse ? "Topic" : "Post"}
          rows={dev ? 6 : 3}
          placeholder={
            reddit || lemmy || discourse
              ? "What you want to share or ask"
              : dev
                ? "Your article, in Markdown"
                : on === "linkedin"
                  ? "What you want to share"
                  : "What's happening"
          }
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
            {discourse && forum
              ? `Goes in ${forum.category_name}, up to ${dailyCap(on)} a day.`
              : `Goes out within ten minutes, up to ${dailyCap(on)} a day.`}
          </p>
          <Button type="submit" size="sm" loading={busy} disabled={!ready} className="max-sm:ml-auto">
            <Send className="size-3.5" /> {dev ? "Queue article" : discourse ? "Queue topic" : "Queue post"}
          </Button>
        </div>
      </form>
    </Card>
  );
}

/** A DEV or Hashnode article's title and tags. */
function ArticleFields({
  platform,
  title,
  tags,
  setTitle,
  setTags,
}: {
  platform: Platform;
  title: string;
  tags: string;
  setTitle: (s: string) => void;
  setTags: (s: string) => void;
}) {
  return (
    <div className="flex flex-col gap-3 sm:flex-row">
      <Field label="Title" className="flex-1">
        <Input autoComplete="off" maxLength={ARTICLE_TITLE_CHARS[platform]} value={title} onChange={(e) => setTitle(e.target.value)} />
      </Field>
      <TagsField platform={platform} tags={tags} setTags={setTags} className="sm:w-64" />
    </div>
  );
}

/** Tags for a DEV or Hashnode article or a Tumblr post, which is how people find posts on Tumblr. */
function TagsField({ platform, tags, setTags, className }: { platform: Platform; tags: string; setTags: (s: string) => void; className?: string }) {
  return (
    <Field label={`Up to ${MAX_TAGS[platform]} tags`} className={className}>
      <Input
        autoComplete="off"
        placeholder={platform === "tumblr" ? "small business, marketing" : platform === "hashnode" ? "devops, startup" : "webdev, productivity"}
        value={tags}
        onChange={(e) => setTags(e.target.value)}
      />
    </Field>
  );
}

/** A Lemmy post's community and title. */
function CommunityFields({
  community,
  title,
  setCommunity,
  setTitle,
}: {
  community: string;
  title: string;
  setCommunity: (s: string) => void;
  setTitle: (s: string) => void;
}) {
  return (
    <div className="flex flex-col gap-3 sm:flex-row">
      <Field label="Community" className="sm:w-56">
        <Input placeholder="sales@lemmy.world" autoComplete="off" value={community} onChange={(e) => setCommunity(e.target.value)} />
      </Field>
      <Field label="Title" className="flex-1">
        <Input autoComplete="off" maxLength={LEMMY_TITLE_CHARS} value={title} onChange={(e) => setTitle(e.target.value)} />
      </Field>
    </div>
  );
}

/** A Discourse topic's title, which the forum turns down under 15 characters. */
function TopicTitle({ title, setTitle }: { title: string; setTitle: (s: string) => void }) {
  return (
    <Field label="Title" className="flex-1">
      <Input autoComplete="off" minLength={TOPIC_TITLE_CHARS[0]} maxLength={TOPIC_TITLE_CHARS[1]} value={title} onChange={(e) => setTitle(e.target.value)} />
    </Field>
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

/** YouTube links to a comment with `lc`, and a reply to one goes in its thread. */
function replyingTo(p: SocialPost): string {
  if (p.platform === "linkedin") return "Commenting on this post";
  if (p.platform === "stackexchange") return "Answering this question";
  if (p.platform === "discourse") return "Replying to this topic";
  if (p.platform === "lemmy") return p.in_reply_to?.includes("/comment/") ? "Replying to this comment" : "Commenting on this post";
  if (p.platform === "nostr") return "Replying to this note";
  if (p.platform === "tumblr") return "Reblogging this post with your comment";
  if (p.platform === "hashnode") return "Commenting on this article";
  if (p.platform === "youtube") return p.in_reply_to?.includes("&lc=") ? "Replying to this comment" : "Commenting on this video";
  return "Replying to this thread";
}

/** A draft stays editable until it's posted. Every button saves the edits along with the new status. */
function PostCard({
  post: p,
  forum,
  mastodonChars,
  onChange,
}: {
  post: SocialPost;
  /** The Discourse forum it goes to, while the project still joins it. */
  forum: DiscourseForum | undefined;
  mastodonChars: number;
  onChange: () => Promise<unknown>;
}) {
  const [body, setBody] = useState(p.body);
  const [title, setTitle] = useState(p.title);
  const [subreddit, setSubreddit] = useState(p.subreddit);
  const [tags, setTags] = useState(p.tags.join(", "));
  const [saving, setSaving] = useState<string | null>(null);
  const reply = !!p.in_reply_to;
  const redditPost = p.platform === "reddit" && !reply;
  const lemmyPost = p.platform === "lemmy" && !reply;
  const topic = p.platform === "discourse" && !reply;
  const article = p.platform === "dev" || (p.platform === "hashnode" && !reply);
  const tumblrPost = p.platform === "tumblr" && !reply;
  const posted = p.status === "posted";
  const dirty = body !== p.body || title !== p.title || subreddit !== p.subreddit || tagList(tags).join(",") !== p.tags.join(",");
  const max = maxLength(p.platform, reply, mastodonChars);
  const n = length(p.platform, body.trim());
  // What the backend turns down for an approved post, so those buttons stay off until it fits.
  const tooShort = n < minLength(p.platform);
  const unfit =
    (max !== null && n > max) || tooShort || (!reply && !titleReady(p.platform, title, subreddit, forum)) || (p.platform === "discourse" && !forum);
  // LinkedIn, YouTube, Lemmy, Hashnode and Tumblr call a reply a comment, Stack Exchange an answer, DEV and Hashnode a post an
  // article, Discourse a topic, and Nostr a post a note.
  const kind = reply
    ? p.platform === "linkedin" || p.platform === "youtube" || p.platform === "lemmy" || p.platform === "hashnode" || p.platform === "tumblr"
      ? "comment"
      : p.platform === "stackexchange"
        ? "answer"
        : "reply"
    : article
      ? "article"
      : topic
        ? "topic"
        : p.platform === "nostr"
          ? "note"
          : "post";

  const save = async (status?: SocialPost["status"]) => {
    setSaving(status ?? "save");
    try {
      const fresh = await api<SocialPost>(`/social/${p.id}`, { method: "PATCH", body: { body, title, subreddit, tags: tagList(tags), status } });
      // The backend trims the text, writes "r/sales" as "sales" and "Web Dev" as "webdev", which would otherwise leave the card
      // looking unsaved.
      setBody(fresh.body);
      setTitle(fresh.title);
      setSubreddit(fresh.subreddit);
      setTags(fresh.tags.join(", "));
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
          {p.subreddit && <span className="text-sm text-muted-foreground">{p.platform === "lemmy" ? `!${p.subreddit}` : `r/${p.subreddit}`}</span>}
          {p.platform === "discourse" && (
            <span className="text-sm text-muted-foreground">
              {forum ? <Host host={topic ? `${forumHost(forum)} · ${forum.category_name}` : forumHost(forum)} /> : "Forum removed"}
            </span>
          )}
          <span className="text-xs text-muted-foreground" title={dateTime(p.posted_at ?? p.created_at)}>
            {posted ? `Posted ${timeAgo(p.posted_at ?? p.created_at)}` : timeAgo(p.created_at)}
          </span>
          {p.status !== "draft" && <Badge tone={STATUS[p.status].tone} className="ml-auto">{STATUS[p.status].label}</Badge>}
        </div>

        {reply && (
          <div className="rounded-lg bg-muted/60 px-3 py-2.5">
            <a href={p.in_reply_to!} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 text-xs font-medium hover:underline">
              {replyingTo(p)} on {PLATFORMS[p.platform]} <ExternalLink className="size-3" />
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
            {p.tags.length > 0 && <p className="text-xs text-muted-foreground">{p.tags.map((t) => `#${t}`).join(" ")}</p>}
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
            {lemmyPost && <CommunityFields community={subreddit} title={title} setCommunity={setSubreddit} setTitle={setTitle} />}
            {topic && <TopicTitle title={title} setTitle={setTitle} />}
            {article && <ArticleFields platform={p.platform} title={title} tags={tags} setTitle={setTitle} setTags={setTags} />}
            {tumblrPost && <TagsField platform={p.platform} tags={tags} setTags={setTags} />}
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
              {tooShort && n > 0 && <p className="text-right text-xs text-muted-foreground">Needs at least {minLength(p.platform)} characters</p>}
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
              <Button size="sm" variant="ghost" onClick={() => save()} loading={saving === "save"} disabled={!!saving || (p.status === "approved" && unfit)}>
                Save
              </Button>
            )}
            {p.status !== "skipped" && (
              <Button size="sm" variant="ghost" onClick={() => save("skipped")} loading={saving === "skipped"} disabled={!!saving} className="px-2">
                <SkipForward className="size-3.5" /> Skip
              </Button>
            )}
            {p.status !== "approved" && (
              <Button size="sm" variant="outline" onClick={() => save("approved")} loading={saving === "approved"} disabled={!!saving || unfit}>
                <Check className="size-3.5" /> {p.status === "draft" ? "Approve" : "Approve again"}
              </Button>
            )}
          </div>
        )}
      </Card>
    </li>
  );
}
