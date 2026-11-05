import Link from "next/link";
import { cn } from "@/lib/utils";

export function LogoMark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 20 20" className={cn("size-5", className)} aria-hidden>
      <rect x="1" y="1" width="6" height="12" rx="1" fill="#a3a3a3" />
      <rect x="1" y="13" width="18" height="6" rx="1" fill="#0b0b0b" />
      <rect x="9" y="7" width="6" height="4" rx="1" fill="#10b981" />
    </svg>
  );
}

export function Logo({ href = "/", className }: { href?: string; className?: string }) {
  return (
    <Link href={href} className={cn("inline-flex items-center gap-1.5 text-[22px] font-medium tracking-tight", className)} aria-label="Splee">
      <LogoMark />
      <span>splee</span>
    </Link>
  );
}
