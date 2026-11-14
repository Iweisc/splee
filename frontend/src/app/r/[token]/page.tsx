import type { Metadata } from "next";
import { ReferralPageView } from "./referral-page-view";

// Each page is one customer's, so it stays out of search engines.
export const metadata: Metadata = {
  title: "An introduction",
  robots: { index: false, follow: false },
};

export default async function ReferralPage({ params }: PageProps<"/r/[token]">) {
  const { token } = await params;
  return <ReferralPageView token={token} />;
}
