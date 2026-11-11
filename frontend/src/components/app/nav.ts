import {
  BarChart3,
  Bell,
  BookOpen,
  Bot,
  Building2,
  CalendarCheck,
  ChartLine,
  CalendarClock,
  CircleUser,
  FileText,
  Globe,
  Gauge,
  Inbox,
  KeyRound,
  Layers,
  UserPlus,
  Mail,
  Megaphone,
  MessageSquareReply,
  MessageSquareText,
  Plug,
  Radio,
  Repeat,
  Rocket,
  Server,
  ShieldBan,
  Smartphone,
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
      { label: "Analytics", href: (p) => `/app/p/${p}/analytics`, icon: ChartLine },
      { label: "Campaigns", href: (p) => `/app/p/${p}/segments`, icon: Layers }, // D1 (+ /segments/[segId], /new)
      { label: "Companies", href: (p) => `/app/p/${p}/companies`, icon: Building2 },
      { label: "Leads", href: (p) => `/app/p/${p}/leads`, icon: Users }, // D1
      { label: "Emails", href: (p) => `/app/p/${p}/emails`, icon: Mail }, // D1
      { label: "Inbox", href: (p) => `/app/p/${p}/inbox`, icon: Inbox }, // D1
      { label: "LinkedIn", href: (p) => `/app/p/${p}/linkedin-sequence`, icon: UserPlus }, // D1
      { label: "Texts", href: (p) => `/app/p/${p}/texts`, icon: Smartphone },
      { label: "Social", href: (p) => `/app/p/${p}/social`, icon: Megaphone },
      { label: "Import", href: (p) => `/app/p/${p}/import`, icon: Upload }, // D1
      { label: "Agent runs", href: (p) => `/app/p/${p}/runs`, icon: Bot },
    ],
  },
  {
    title: "Agent settings",
    items: [
      { label: "Autopilot", href: (p) => `/app/p/${p}/autopilot`, icon: Rocket },
      { label: "Targeting", href: (p) => `/app/p/${p}/targeting`, icon: Target }, // D2
      { label: "Competitors", href: (p) => `/app/p/${p}/competitors`, icon: Swords },
      { label: "Daily limit", href: (p) => `/app/p/${p}/daily-limit`, icon: Gauge }, // D2
      { label: "Schedule", href: (p) => `/app/p/${p}/schedule`, icon: CalendarClock }, // D2
      { label: "Email templates", href: (p) => `/app/p/${p}/email-templates`, icon: FileText }, // D2
      { label: "Follow-ups", href: (p) => `/app/p/${p}/follow-ups`, icon: Repeat }, // D2
      { label: "Auto-replies", href: (p) => `/app/p/${p}/auto-replies`, icon: MessageSquareReply }, // D2
      { label: "Saved replies", href: (p) => `/app/p/${p}/saved-replies`, icon: MessageSquareText },
      { label: "Materials", href: (p) => `/app/p/${p}/materials`, icon: BookOpen }, // D2
      { label: "Channels", href: (p) => `/app/p/${p}/channels`, icon: Radio }, // D2
      { label: "Integrations", href: (p) => `/app/p/${p}/integrations`, icon: Plug }, // D2
    ],
  },
];

export const orgNav: { label: string; href: string; icon: LucideIcon }[] = [
  { label: "Domains", href: "/app/sending-domains", icon: Globe },
  { label: "Mailboxes", href: "/app/domains", icon: Server }, // D2
  { label: "Booking pages", href: "/app/booking-pages", icon: CalendarCheck },
  { label: "Do-not-contact", href: "/app/dedup", icon: ShieldBan }, // D2
  { label: "Team", href: "/app/organization", icon: Users }, // D2
  { label: "Notifications", href: "/app/notifications", icon: Bell }, // D2
  { label: "API keys", href: "/app/api-keys", icon: KeyRound }, // D2
  { label: "Account", href: "/app/account", icon: CircleUser },
];

// Also routed: /app (redirect), /app/onboarding, /app/onboarding/[domain] (D1),
// /app/inbox, /app/emails, /app/pipeline (org-wide, D1), /app/p/[id]/docs (API docs, D2), /app/p/[id]/domains -> /app/domains (D2).
