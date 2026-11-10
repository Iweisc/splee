import { Badge, type BadgeTone } from "@/components/ui/misc";
import type { DnsCheck } from "@/lib/types";

/** One record of a DNS check, grey before the first check. Hover shows the record found or what's missing. */
export function DnsBadge({ name, check }: { name: string; check?: DnsCheck }) {
  const tone: BadgeTone = !check ? "neutral" : check.ok ? "green" : "amber";
  return (
    <Badge tone={tone} title={check?.detail || check?.record || undefined} className="font-mono">
      {name} {!check ? "·" : check.ok ? "✓" : "✗"}
    </Badge>
  );
}
