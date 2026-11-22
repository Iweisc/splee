import type { Metadata } from "next";
import { CaseView } from "./case-view";

// Each case is one buyer's, so it stays out of search engines.
export const metadata: Metadata = {
  title: "Business case",
  robots: { index: false, follow: false },
};

export default async function CasePage({ params, searchParams }: PageProps<"/case/[token]">) {
  const { token } = await params;
  const { preview } = await searchParams;
  return <CaseView token={token} preview={preview !== undefined} />;
}
