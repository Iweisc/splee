import type { Metadata } from "next";
import { DemoPage } from "@/components/demo/demo-page";

export const metadata: Metadata = { title: "Your free outreach demo · Splee" };

export default async function ExplorePage({ params, searchParams }: PageProps<"/auto-gtm/company/[slug]/explore">) {
  const { slug } = await params;
  const { token } = await searchParams;
  return <DemoPage slug={decodeURIComponent(slug)} urlToken={typeof token === "string" ? token : null} />;
}
