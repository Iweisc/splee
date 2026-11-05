import type { Metadata } from "next";
import { notFound } from "next/navigation";
import { ExplorerLanding } from "@/components/marketing/explorer-landing";
import { regions } from "@/content/regions";

export const dynamicParams = false;

export function generateStaticParams() {
  return regions.map((r) => ({ region: r.slug }));
}

export async function generateMetadata({ params }: PageProps<"/b2b-database/locations/[region]">): Promise<Metadata> {
  const { region } = await params;
  const match = regions.find((r) => r.slug === region);
  return { title: match ? `Companies in ${match.name} · Splee` : "Splee" };
}

export default async function RegionPage({ params }: PageProps<"/b2b-database/locations/[region]">) {
  const { region } = await params;
  const match = regions.find((r) => r.slug === region);
  if (!match) notFound();
  return (
    <ExplorerLanding
      title={`Companies in ${match.name}`}
      intro={`Search for businesses in ${match.name} by describing them in plain words. The agent researches them live on the web.`}
      country={match.name}
      activeRegion={match.slug}
    />
  );
}
