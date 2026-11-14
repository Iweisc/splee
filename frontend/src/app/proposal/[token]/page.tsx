import type { Metadata } from "next";
import { ProposalView } from "./proposal-view";

// Each proposal is for one buyer, so it stays out of search engines and link previews show only the title.
export const metadata: Metadata = {
  title: "Proposal",
  robots: { index: false, follow: false },
};

export default async function ProposalPage({ params, searchParams }: PageProps<"/proposal/[token]">) {
  const { token } = await params;
  // The team opens it from the deal with ?preview=1, which isn't the buyer looking.
  const { preview } = await searchParams;
  return <ProposalView token={token} preview={preview === "1"} />;
}
