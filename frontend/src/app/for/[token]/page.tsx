import type { Metadata } from "next";
import { LeadPageView } from "./lead-page-view";

// Each page is written for one person, so it stays out of search engines and link previews show no more than its title.
export const metadata: Metadata = {
  title: "A page for you",
  robots: { index: false, follow: false },
};

export default async function ForPage({ params }: PageProps<"/for/[token]">) {
  const { token } = await params;
  return <LeadPageView token={token} />;
}
