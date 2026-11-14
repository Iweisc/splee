import type { ProposalItem } from "./types";

export const PERIODS: Record<ProposalItem["period"], { label: string; suffix: string }> = {
  one_time: { label: "One-time", suffix: "" },
  month: { label: "Monthly", suffix: "/mo" },
  year: { label: "Yearly", suffix: "/yr" },
};

const sum = (items: ProposalItem[], period?: ProposalItem["period"]) =>
  items.filter((i) => !period || i.period === period).reduce((n, i) => n + i.quantity * i.unit_price, 0);

/** Keep in sync with first_year in backend/src/proposals.rs: monthly items 12 times, the rest once. */
export function firstYear(items: ProposalItem[]): number {
  return sum(items) + sum(items, "month") * 11;
}

export function monthly(items: ProposalItem[]): number {
  return sum(items, "month");
}
