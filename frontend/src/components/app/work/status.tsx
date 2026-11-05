import {
  Briefcase,
  Building,
  Camera,
  Code,
  Dumbbell,
  Factory,
  GraduationCap,
  Heart,
  Hotel,
  House,
  Landmark,
  Palette,
  Plane,
  Scissors,
  ShoppingCart,
  Stethoscope,
  Store,
  Truck,
  Users,
  Utensils,
  type LucideIcon,
} from "lucide-react";
import { Badge, type BadgeTone } from "@/components/ui/misc";
import type { Classification, EmailStatus, LeadStatus, ProjectStatus, SegmentStatus } from "@/lib/types";
import { cn } from "@/lib/utils";

export const SEGMENT_ICONS: Record<string, LucideIcon> = {
  building: Building,
  store: Store,
  hotel: Hotel,
  heart: Heart,
  palette: Palette,
  home: House,
  scissors: Scissors,
  briefcase: Briefcase,
  "graduation-cap": GraduationCap,
  stethoscope: Stethoscope,
  factory: Factory,
  truck: Truck,
  utensils: Utensils,
  code: Code,
  "shopping-cart": ShoppingCart,
  users: Users,
  landmark: Landmark,
  plane: Plane,
  dumbbell: Dumbbell,
  camera: Camera,
};

export function SegmentIcon({ name, className }: { name: string; className?: string }) {
  const Icon = SEGMENT_ICONS[name] ?? Building;
  return (
    <span className={cn("inline-flex size-7 shrink-0 items-center justify-center rounded-lg border border-border bg-card", className)}>
      <Icon className="size-3.5 text-foreground/70" />
    </span>
  );
}

const segmentStatus: Record<SegmentStatus, { label: string; dot: string }> = {
  scaling: { label: "Scaling", dot: "bg-brand-500" },
  working: { label: "Working", dot: "bg-brand-500" },
  paused: { label: "Paused", dot: "bg-black/20" },
  draft: { label: "Draft", dot: "bg-amber-400" },
};

export function SegmentStatusDot({ status }: { status: SegmentStatus }) {
  const s = segmentStatus[status];
  return (
    <span className={cn("inline-flex items-center gap-1.5 text-sm", status === "paused" ? "text-muted-foreground" : "text-foreground")}>
      <span className={cn("size-1.5 rounded-full", s.dot)} />
      {s.label}
    </span>
  );
}

export const LEAD_STATUS: Record<LeadStatus, { label: string; tone: BadgeTone }> = {
  new: { label: "New", tone: "neutral" },
  queued: { label: "Queued", tone: "amber" },
  contacted: { label: "Contacted", tone: "neutral" },
  replied: { label: "Replied", tone: "blue" },
  hot: { label: "Hot", tone: "green" },
  meeting_booked: { label: "Meeting booked", tone: "dark" },
  not_interested: { label: "Not interested", tone: "neutral" },
  bounced: { label: "Bounced", tone: "red" },
  unsubscribed: { label: "Unsubscribed", tone: "red" },
  do_not_contact: { label: "Do not contact", tone: "red" },
};

export function LeadStatusBadge({ status }: { status: LeadStatus }) {
  const s = LEAD_STATUS[status];
  return <Badge tone={s.tone}>{s.label}</Badge>;
}

const emailStatus: Record<EmailStatus, BadgeTone> = {
  draft: "amber",
  approved: "blue",
  scheduled: "blue",
  sending: "blue",
  sent: "green",
  failed: "red",
  cancelled: "neutral",
  received: "neutral",
};

export function EmailStatusBadge({ status }: { status: EmailStatus }) {
  return <Badge tone={emailStatus[status]} className="capitalize">{status}</Badge>;
}

const classification: Record<Classification, { label: string; tone: BadgeTone }> = {
  hot: { label: "Hot", tone: "green" },
  question: { label: "Question", tone: "blue" },
  not_interested: { label: "Not interested", tone: "neutral" },
  ooo: { label: "Out of office", tone: "amber" },
  unsubscribe: { label: "Unsubscribe", tone: "red" },
  bounce: { label: "Bounce", tone: "red" },
  other: { label: "Other", tone: "neutral" },
};

export function ClassificationBadge({ value }: { value: Classification }) {
  const c = classification[value];
  return <Badge tone={c.tone}>{c.label}</Badge>;
}

const projectStatus: Record<ProjectStatus, { label: string; tone: BadgeTone }> = {
  researching: { label: "Researching", tone: "amber" },
  ready: { label: "Ready", tone: "neutral" },
  active: { label: "Sending", tone: "green" },
  paused: { label: "Paused", tone: "neutral" },
  failed: { label: "Research failed", tone: "red" },
};

export function ProjectStatusBadge({ status }: { status: ProjectStatus }) {
  const s = projectStatus[status];
  return <Badge tone={s.tone}>{s.label}</Badge>;
}

export function FitScore({ value }: { value: number }) {
  const pct = Math.max(0, Math.min(100, value));
  return (
    <span className="inline-flex items-center gap-2">
      <span className="h-1 w-12 overflow-hidden rounded-full bg-black/10">
        <span className="block h-full rounded-full bg-brand-500" style={{ width: `${pct}%` }} />
      </span>
      <span className="text-xs text-muted-foreground tabular-nums">{pct}</span>
    </span>
  );
}
