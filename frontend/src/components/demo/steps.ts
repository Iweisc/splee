import {
  Briefcase,
  Building2,
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

// `phase` values match explee.com's ?phase= query param so links look the same.
export const STEPS = [
  { n: 1, title: "Research your company", phase: "research_company" },
  { n: 2, title: "Explore competitors", phase: "research_competitors" },
  { n: 3, title: "Define campaigns", phase: "research_segments" },
  { n: 4, title: "Find potential customers", phase: "outreach_companies" },
  { n: 5, title: "Find decision makers", phase: "outreach_people" },
  { n: 6, title: "Write personal emails", phase: "outreach_emails" },
] as const;

export const STEP_COUNT = STEPS.length;

const segmentIcons: Record<string, LucideIcon> = {
  building: Building2,
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

export function segmentIcon(name: string): LucideIcon {
  return segmentIcons[name] ?? Building2;
}
