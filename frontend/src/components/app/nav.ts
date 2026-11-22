import {
  BarChart3,
  Bell,
  BookMarked,
  BookOpen,
  Bot,
  Building2,
  CalendarCheck,
  ChartLine,
  CalendarClock,
  CircleUser,
  ClipboardCheck,
  ClipboardList,
  DatabaseBackup,
  EyeOff,
  FileText,
  GraduationCap,
  Globe,
  Gauge,
  Inbox,
  KeyRound,
  Layers,
  Lightbulb,
  UserPlus,
  Mail,
  Megaphone,
  Phone,
  MessageSquareReply,
  MessageSquareText,
  MessagesSquare,
  Plug,
  Radio,
  Repeat,
  Rocket,
  Server,
  ShieldBan,
  ShieldCheck,
  Smartphone,
  Swords,
  Target,
  Upload,
  UserCheck,
  Users,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { qs } from "@/lib/api";

/** The count beside a nav link: an endpoint answering `{total}`, and what it counts, for screen readers. */
export interface NavCount {
  path: string;
  what: string;
}

export interface NavItem {
  label: string;
  href: (projectId: string) => string;
  icon: LucideIcon;
  count?: (projectId: string) => NavCount;
}

/** Inbox threads in "Needs reply" that are yours or nobody's, so a new reply shows from any page without counting a teammate's. */
export const needsReply = (projectId?: string): NavCount => ({
  path: `/inbox${qs({ project_id: projectId, filter: "needs_reply", owner: "mine_or_unassigned", per_page: 1 })}`,
  what: "need a reply",
});

/** The open calls the Calls page lists first, yours and nobody's. The page refreshes it as calls are logged or skipped. */
export const callsDue = (projectId: string): NavCount => ({ path: `/projects/${projectId}/calls/due`, what: "calls to make" });

// Explee's app routes (/app-auto-gtm/...) mapped to ours. Owners: D1 = campaign work pages, D2 = settings + org pages.
export const projectNav: { title: string; items: NavItem[] }[] = [
  {
    title: "Outreach",
    items: [
      { label: "Overview", href: (p) => `/app/p/${p}`, icon: BarChart3 }, // D1
      { label: "Analytics", href: (p) => `/app/p/${p}/analytics`, icon: ChartLine },
      { label: "Campaigns", href: (p) => `/app/p/${p}/segments`, icon: Layers }, // D1 (+ /segments/[segId], /new)
      { label: "Companies", href: (p) => `/app/p/${p}/companies`, icon: Building2 },
      { label: "Leads", href: (p) => `/app/p/${p}/leads`, icon: Users }, // D1
      { label: "Emails", href: (p) => `/app/p/${p}/emails`, icon: Mail }, // D1
      { label: "Inbox", href: (p) => `/app/p/${p}/inbox`, icon: Inbox, count: needsReply }, // D1
      { label: "LinkedIn", href: (p) => `/app/p/${p}/linkedin-sequence`, icon: UserPlus }, // D1
      { label: "Texts", href: (p) => `/app/p/${p}/texts`, icon: Smartphone },
      { label: "Calls", href: (p) => `/app/p/${p}/calls`, icon: Phone, count: callsDue },
      { label: "Coaching", href: (p) => `/app/p/${p}/coaching`, icon: GraduationCap },
      { label: "Playbook", href: (p) => `/app/p/${p}/playbook`, icon: BookMarked },
      { label: "Feedback", href: (p) => `/app/p/${p}/feedback`, icon: Lightbulb },
      { label: "Questionnaires", href: (p) => `/app/p/${p}/questionnaires`, icon: ClipboardCheck },
      { label: "Social", href: (p) => `/app/p/${p}/social`, icon: Megaphone },
      { label: "Import", href: (p) => `/app/p/${p}/import`, icon: Upload }, // D1
      { label: "Agent runs", href: (p) => `/app/p/${p}/runs`, icon: Bot },
    ],
  },
  {
    title: "Agent settings",
    items: [
      { label: "Autopilot", href: (p) => `/app/p/${p}/autopilot`, icon: Rocket },
      { label: "Plays", href: (p) => `/app/p/${p}/plays`, icon: Zap },
      { label: "Targeting", href: (p) => `/app/p/${p}/targeting`, icon: Target }, // D2
      { label: "Competitors", href: (p) => `/app/p/${p}/competitors`, icon: Swords },
      { label: "Daily limit", href: (p) => `/app/p/${p}/daily-limit`, icon: Gauge }, // D2
      { label: "Schedule", href: (p) => `/app/p/${p}/schedule`, icon: CalendarClock }, // D2
      { label: "Email templates", href: (p) => `/app/p/${p}/email-templates`, icon: FileText }, // D2
      { label: "Follow-ups", href: (p) => `/app/p/${p}/follow-ups`, icon: Repeat }, // D2
      { label: "Auto-replies", href: (p) => `/app/p/${p}/auto-replies`, icon: MessageSquareReply }, // D2
      { label: "Assignment", href: (p) => `/app/p/${p}/assignment`, icon: UserCheck },
      { label: "Saved replies", href: (p) => `/app/p/${p}/saved-replies`, icon: MessageSquareText },
      { label: "Materials", href: (p) => `/app/p/${p}/materials`, icon: BookOpen }, // D2
      { label: "Website chat", href: (p) => `/app/p/${p}/website-chat`, icon: MessagesSquare },
      { label: "Channels", href: (p) => `/app/p/${p}/channels`, icon: Radio }, // D2
      { label: "Integrations", href: (p) => `/app/p/${p}/integrations`, icon: Plug }, // D2
    ],
  },
];

/** `admin` items show to owners and admins only. */
export const orgNav: { label: string; href: string; icon: LucideIcon; admin?: boolean }[] = [
  { label: "Domains", href: "/app/sending-domains", icon: Globe },
  { label: "Mailboxes", href: "/app/domains", icon: Server }, // D2
  { label: "Deliverability", href: "/app/deliverability", icon: ShieldCheck },
  { label: "Booking pages", href: "/app/booking-pages", icon: CalendarCheck },
  { label: "Do-not-contact", href: "/app/dedup", icon: ShieldBan }, // D2
  { label: "Hidden companies", href: "/app/hidden-companies", icon: EyeOff },
  { label: "Team", href: "/app/organization", icon: Users }, // D2
  { label: "Notifications", href: "/app/notifications", icon: Bell }, // D2
  { label: "API keys", href: "/app/api-keys", icon: KeyRound }, // D2
  { label: "Data export", href: "/app/data-export", icon: DatabaseBackup },
  { label: "Audit log", href: "/app/audit", icon: ClipboardList, admin: true },
  { label: "Account", href: "/app/account", icon: CircleUser },
];

// Also routed: /app (redirect), /app/onboarding, /app/onboarding/[domain] (D1),
// /app/inbox, /app/emails, /app/pipeline (org-wide, D1), /app/p/[id]/docs (API docs, D2), /app/p/[id]/domains -> /app/domains (D2).
