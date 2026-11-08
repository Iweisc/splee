import {
  BarChart3,
  Bell,
  BookOpen,
  Bot,
  CalendarClock,
  FileText,
  Gauge,
  Inbox,
  KeyRound,
  Layers,
  UserPlus,
  Mail,
  MessageSquareReply,
  Plug,
  Radio,
  Repeat,
  Server,
  ShieldBan,
  Swords,
  Target,
  Upload,
  Users,
  type LucideIcon,
} from "lucide-react";

export interface NavItem {
  label: string;
  href: (projectId: string) => string;
  icon: LucideIcon;
}

// Explee's app routes (/app-auto-gtm/...) mapped to ours. Owners: D1 = campaign work pages, D2 = settings + org pages.
export const projectNav: { title: string; items: NavItem[] }[] = [
  {
    title: "Outreach",
    items: [
      { label: "Overview", href: (p) => `/app/p/${p}`, icon: BarChart3 }, // D1
      { label: "Campaigns", href: (p) => `/app/p/${p}/segments`, icon: Layers }, // D1 (+ /segments/[segId], /new)
      { label: "Leads", href: (p) => `/app/p/${p}/leads`, icon: Users }, // D1
      { label: "Emails", href: (p) => `/app/p/${p}/emails`, icon: Mail }, // D1
      { label: "Inbox", href: (p) => `/app/p/${p}/inbox`, icon: Inbox }, // D1
      { label: "LinkedIn", href: (p) => `/app/p/${p}/linkedin-sequence`, icon: UserPlus }, // D1
      { label: "Import", href: (p) => `/app/p/${p}/import`, icon: Upload }, // D1
      { label: "Agent runs", href: (p) => `/app/p/${p}/runs`, icon: Bot },
    ],
  },
  {
    title: "Agent settings",
    items: [
      { label: "Targeting", href: (p) => `/app/p/${p}/targeting`, icon: Target }, // D2
      { label: "Competitors", href: (p) => `/app/p/${p}/competitors`, icon: Swords },
      { label: "Daily limit", href: (p) => `/app/p/${p}/daily-limit`, icon: Gauge }, // D2
      { label: "Schedule", href: (p) => `/app/p/${p}/schedule`, icon: CalendarClock }, // D2
      { label: "Email templates", href: (p) => `/app/p/${p}/email-templates`, icon: FileText }, // D2
      { label: "Follow-ups", href: (p) => `/app/p/${p}/follow-ups`, icon: Repeat }, // D2
      { label: "Auto-replies", href: (p) => `/app/p/${p}/auto-replies`, icon: MessageSquareReply }, // D2
      { label: "Materials", href: (p) => `/app/p/${p}/materials`, icon: BookOpen }, // D2
      { label: "Channels", href: (p) => `/app/p/${p}/channels`, icon: Radio }, // D2
      { label: "Integrations", href: (p) => `/app/p/${p}/integrations`, icon: Plug }, // D2
    ],
  },
];

export const orgNav: { label: string; href: string; icon: LucideIcon }[] = [
  { label: "Mailboxes", href: "/app/domains", icon: Server }, // D2
  { label: "Do-not-contact", href: "/app/dedup", icon: ShieldBan }, // D2
  { label: "Team", href: "/app/organization", icon: Users }, // D2
  { label: "Notifications", href: "/app/notifications", icon: Bell }, // D2
  { label: "API keys", href: "/app/api-keys", icon: KeyRound }, // D2
];

// Also routed: /app (redirect), /app/onboarding, /app/onboarding/[domain] (D1),
// /app/inbox, /app/emails (org-wide, D1), /app/p/[id]/docs (API docs, D2), /app/p/[id]/domains -> /app/domains (D2).
