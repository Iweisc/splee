import type { Metadata } from "next";
import { StoryView } from "./story-view";

// Each page is one customer's story before they approve it, so it stays out of search engines.
export const metadata: Metadata = {
  title: "Your story",
  robots: { index: false, follow: false },
};

export default async function StoryPage({ params, searchParams }: PageProps<"/story/[token]">) {
  const { token } = await params;
  const { preview } = await searchParams;
  return <StoryView token={token} preview={preview !== undefined} />;
}
