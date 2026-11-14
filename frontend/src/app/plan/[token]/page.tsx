import type { Metadata } from "next";
import { PlanView } from "./plan-view";

// Each plan is between one buyer and one team, so it stays out of search engines and link previews show only the title.
export const metadata: Metadata = {
  title: "Our plan",
  robots: { index: false, follow: false },
};

export default async function PlanPage({ params, searchParams }: PageProps<"/plan/[token]">) {
  const { token } = await params;
  // The team opens it from the deal with ?preview=1, which isn't the buyer looking.
  const { preview } = await searchParams;
  return <PlanView token={token} preview={preview === "1"} />;
}
