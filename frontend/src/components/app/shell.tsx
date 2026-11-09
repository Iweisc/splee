"use client";

import Link from "next/link";
import { useParams, usePathname, useRouter } from "next/navigation";
import { useEffect, useRef, useState } from "react";
import useSWR from "swr";
import { ArrowUpRight, ChevronsUpDown, Inbox, LogOut, Mail, Menu, Plus, X } from "lucide-react";
import { Toaster } from "sonner";
import { SupportChat } from "@/components/support-chat";
import { Logo } from "@/components/ui/logo";
import { Favicon, Spinner } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import { SOURCE_URL } from "@/lib/source";
import type { ProjectSummary } from "@/lib/types";
import { cn } from "@/lib/utils";
import { fetcher, useMe } from "@/hooks/use-me";
import { orgNav, projectNav } from "./nav";

const LAST_PROJECT_KEY = "splee:last-project";

export function AppShell({ children }: { children: React.ReactNode }) {
  const router = useRouter();
  const pathname = usePathname();
  const params = useParams<{ projectId?: string }>();
  const { me, isLoading } = useMe();
  const { data: projects } = useSWR<ProjectSummary[]>(me ? "/projects" : null, fetcher);
  // The phone menu remembers the page it was opened on, so following any link closes it without an effect.
  const [menuPath, setMenuPath] = useState<string | null>(null);
  const menuOpen = menuPath === pathname;
  const navRef = useRef<HTMLElement>(null);

  useEffect(() => {
    if (params.projectId) localStorage.setItem(LAST_PROJECT_KEY, params.projectId);
  }, [params.projectId]);

  // The nav scrolls on a laptop or phone, and the org pages sit at its bottom, so a fresh load of one left its
  // highlighted link out of view. This scrolls just enough to bring it clear of the faded edges, and not at all when
  // it's already in view, so clicking a link never moves the list under the pointer.
  useEffect(() => {
    const nav = navRef.current;
    const link = nav?.querySelector('[aria-current="page"]');
    if (!nav || !link) return;
    const n = nav.getBoundingClientRect();
    const l = link.getBoundingClientRect();
    if (l.bottom > n.bottom - 32) nav.scrollTop += l.bottom - n.bottom + 32;
    else if (l.top < n.top + 16) nav.scrollTop -= n.top + 16 - l.top;
  }, [pathname, menuOpen, me, projects]);

  useEffect(() => {
    if (me === null) router.replace(`/signin?next=${encodeURIComponent(pathname)}`);
  }, [me, pathname, router]);

  if (isLoading || !me) {
    return (
      <div className="flex min-h-screen items-center justify-center">
        <Spinner className="size-6" />
      </div>
    );
  }

  // Safe to read during render: we only get here on the client, after /auth/me resolved.
  const lastProject = localStorage.getItem(LAST_PROJECT_KEY);
  // Until the project list loads, the last project stands in, so the sidebar doesn't draw without its project links
  // and then push everything down when they arrive.
  const projectId =
    params.projectId ?? (projects ? (projects.find((p) => p.id === lastProject)?.id ?? projects[0]?.id ?? null) : lastProject);
  const project = projects?.find((p) => p.id === projectId);
  const pageTitle = titleFor(pathname, projectId, project);
  const projectInbox = projectId ? `/app/p/${projectId}/inbox` : null;

  const logout = async () => {
    await api("/auth/logout", { method: "POST" });
    router.replace("/signin");
  };

  return (
    <div className="flex min-h-screen flex-col lg:flex-row">
      <PageTitle title={pageTitle} />
      <header className="sticky top-0 z-30 flex h-14 items-center gap-3 border-b border-border bg-background px-4 lg:hidden">
        <button onClick={() => setMenuPath(pathname)} className="-ml-1.5 rounded-md p-1.5 hover:bg-accent" aria-label="Open menu">
          <Menu className="size-5" />
        </button>
        <Logo href="/app" className="text-lg" />
      </header>
      {menuOpen && <div className="fixed inset-0 z-40 bg-black/30 lg:hidden" onClick={() => setMenuPath(null)} />}

      {/* No transform for the slide-in: it would turn the support chat's fixed panel relative to this aside. */}
      <aside
        className={cn(
          "fixed inset-y-0 left-0 z-40 h-dvh w-60 shrink-0 flex-col border-r border-border bg-background lg:sticky lg:top-0 lg:z-auto lg:flex lg:h-screen",
          menuOpen ? "flex shadow-pop" : "hidden",
        )}
      >
        <div className="flex items-center justify-between px-4 pt-4 pb-3">
          <Logo href="/app" className="text-lg" />
          <button onClick={() => setMenuPath(null)} className="rounded-md p-1 text-muted-foreground hover:bg-accent lg:hidden" aria-label="Close menu">
            <X className="size-4" />
          </button>
        </div>

        <ProjectSwitcher projects={projects} current={project} />

        {/* The nav doesn't fit a laptop's height, so its edges fade: a cut-off item shows there's more to scroll to. Scrolled
            to either end, they only cover padding: the first group's mt-4 at the top and pb-8 at the bottom. */}
        <nav ref={navRef} className="flex-1 overflow-y-auto px-2 pb-8 [mask-image:linear-gradient(transparent,#000_1rem,#000_calc(100%-2rem),transparent)]">
          {projectId &&
            projectNav.map((group) => (
              <div key={group.title} className="mt-4">
                <p className="px-2 pb-1 font-mono text-[11px] tracking-wide text-muted-foreground uppercase">{group.title}</p>
                {group.items.map((item) => {
                  const href = item.href(projectId);
                  const active = href === `/app/p/${projectId}` ? pathname === href : pathname.startsWith(href);
                  const count = href === projectInbox ? <NeedsReplyCount projectId={projectId} /> : null;
                  return <NavLink key={item.label} href={href} icon={item.icon} label={item.label} active={active} count={count} />;
                })}
              </div>
            ))}

          <div className="mt-4">
            <p className="px-2 pb-1 font-mono text-[11px] tracking-wide text-muted-foreground uppercase">All projects</p>
            <NavLink href="/app/inbox" icon={Inbox} label="Inbox" active={pathname === "/app/inbox"} count={<NeedsReplyCount />} />
            <NavLink href="/app/emails" icon={Mail} label="Emails" active={pathname === "/app/emails"} />
          </div>

          <div className="mt-4">
            <p className="px-2 pb-1 font-mono text-[11px] tracking-wide text-muted-foreground uppercase">{me.org.name}</p>
            {orgNav.map((item) => (
              <NavLink key={item.href} href={item.href} icon={item.icon} label={item.label} active={pathname.startsWith(item.href)} />
            ))}
          </div>
        </nav>

        <div className="border-t border-border p-3">
          <SupportChat inSidebar />
          <a
            href={SOURCE_URL}
            target="_blank"
            rel="noreferrer"
            className="flex items-center justify-between rounded-lg px-2 py-1.5 text-sm text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            Source code <ArrowUpRight className="size-3.5" />
          </a>
          <div className="mt-1 flex items-center justify-between gap-2 px-2 py-1.5">
            <div className="min-w-0">
              <p className="truncate text-sm font-medium">{me.user.name || me.user.email}</p>
              <p className="truncate text-xs text-muted-foreground">{me.user.email}</p>
            </div>
            <button onClick={logout} className="rounded-md p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground" aria-label="Sign out">
              <LogOut className="size-4" />
            </button>
          </div>
        </div>
      </aside>

      <main className="min-w-0 flex-1">{children}</main>
      <Toaster position="bottom-right" />
    </div>
  );
}

/** "Leads · Acme" for project pages, "Team · Splee" for the rest. */
// Project pages that aren't in the sidebar.
const EXTRA_PROJECT_PAGES: Record<string, string> = { new: "New campaign", docs: "API docs" };
const EXTRA_ORG_PAGES: Record<string, string> = { "/app/inbox": "Inbox", "/app/emails": "Emails", "/app/onboarding": "New project" };

function titleFor(pathname: string, projectId: string | null, project?: ProjectSummary): string {
  if (projectId && pathname.startsWith(`/app/p/${projectId}`)) {
    const item = projectNav
      .flatMap((g) => g.items)
      .find((i) => {
        const href = i.href(projectId);
        return href === `/app/p/${projectId}` ? pathname === href : pathname.startsWith(href);
      });
    const label = item?.label ?? EXTRA_PROJECT_PAGES[pathname.split("/")[4] ?? ""] ?? "Overview";
    return `${label} · ${project?.name || project?.domain || "Splee"}`;
  }
  const label = orgNav.find((i) => pathname.startsWith(i.href))?.label ?? EXTRA_ORG_PAGES[pathname];
  return label ? `${label} · Splee` : "Splee";
}

/**
 * App pages are client components, so they can't export metadata. React 19 hoists this <title> into <head>.
 * Setting document.title in an effect doesn't stick: Next writes its own title after client navigations.
 */
function PageTitle({ title }: { title: string }) {
  return <title>{title}</title>;
}

function NavLink({
  href,
  icon: Icon,
  label,
  active,
  count,
}: {
  href: string;
  icon: React.ElementType;
  label: string;
  active: boolean;
  count?: React.ReactNode;
}) {
  return (
    <Link
      href={href}
      aria-current={active ? "page" : undefined}
      className={cn(
        "flex items-center gap-2.5 rounded-lg px-2 py-1.5 text-sm transition-colors",
        active ? "bg-card font-medium text-foreground shadow-card" : "text-foreground/75 hover:bg-accent hover:text-foreground",
      )}
    >
      <Icon className="size-4 shrink-0" />
      {label}
      {count}
    </Link>
  );
}

/** Threads in the inbox's "Needs reply" filter, so a new reply is visible from any page. Polled like the mailboxes. */
function NeedsReplyCount({ projectId }: { projectId?: string }) {
  const { data } = useSWR<{ total: number }>(`/inbox${qs({ project_id: projectId, filter: "needs_reply", per_page: 1 })}`, fetcher, {
    refreshInterval: 60_000,
  });
  if (!data?.total) return null;
  return (
    <span className="ml-auto rounded-full bg-foreground px-1.5 text-[11px] leading-[18px] font-medium text-background tabular-nums">
      {data.total > 99 ? "99+" : data.total}
      <span className="sr-only"> need a reply</span>
    </span>
  );
}

function ProjectSwitcher({ projects, current }: { projects?: ProjectSummary[]; current?: ProjectSummary }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="relative px-2">
      <button
        onClick={() => setOpen((o) => !o)}
        className="flex w-full items-center gap-2 rounded-lg border border-border bg-card px-2.5 py-2 text-left text-sm shadow-card hover:bg-accent"
      >
        {current?.domain ? <Favicon domain={current.domain} /> : <span className="size-4 rounded-sm bg-black/10" />}
        <span className="min-h-5 flex-1 truncate font-medium">{current ? current.name || current.domain : projects && "No project"}</span>
        <ChevronsUpDown className="size-3.5 text-muted-foreground" />
      </button>
      {open && (
        <div className="absolute inset-x-2 top-full z-20 mt-1 rounded-xl bg-card p-1 shadow-pop" onMouseLeave={() => setOpen(false)}>
          {projects?.map((p) => (
            <Link
              key={p.id}
              href={`/app/p/${p.id}`}
              onClick={() => setOpen(false)}
              className="flex items-center gap-2 rounded-lg px-2 py-1.5 text-sm hover:bg-accent"
            >
              {p.domain ? <Favicon domain={p.domain} /> : <span className="size-4 rounded-sm bg-black/10" />}
              <span className="truncate">{p.name || p.domain}</span>
            </Link>
          ))}
          <Link href="/app/onboarding" onClick={() => setOpen(false)} className="flex items-center gap-2 rounded-lg px-2 py-1.5 text-sm text-muted-foreground hover:bg-accent">
            <Plus className="size-4" /> New project
          </Link>
        </div>
      )}
    </div>
  );
}
