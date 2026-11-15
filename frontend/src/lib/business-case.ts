import type { BusinessCaseContent, CaseLine, CaseNumbers } from "@/lib/types";

/** A year is 48 working weeks. The backend does the same sum (business_cases.rs) for the chats. */
const TIMES: Record<CaseLine["per"], number> = { week: 48, month: 12, year: 1 };

export const annual = (per: CaseLine["per"], n: CaseNumbers) => n.quantity * n.unit_value * n.count * TIMES[per];

/** Each line's numbers: the buyer's where they gave them, else the agent's. */
export const numbersOf = (c: BusinessCaseContent, numbers: CaseNumbers[] | null): CaseNumbers[] =>
  c.lines.map((l, i) => numbers?.[i] ?? { quantity: l.quantity, unit_value: l.unit_value, count: l.count });

export const total = (c: BusinessCaseContent, numbers: CaseNumbers[]) => c.lines.reduce((sum, l, i) => sum + annual(l.per, numbers[i]), 0);

/** Months to earn back the first year's cost, or null without a cost or a gain. */
export const paybackMonths = (yearly: number, cost: number) => (cost > 0 && yearly > 0 ? cost / (yearly / 12) : null);
