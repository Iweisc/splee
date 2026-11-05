import type { Metadata } from "next";
import { ExplorerLanding } from "@/components/marketing/explorer-landing";

export const metadata: Metadata = { title: "Google Maps search · Splee" };

export default function GmDataset() {
  return (
    <ExplorerLanding
      title="Local businesses, found on demand"
      intro="Clinics, restaurants, salons, workshops: describe the kind of place and where it is. The agent looks them up on the web and returns a list you can export."
      placeholder="e.g. family-run dental clinics in Lyon"
    />
  );
}
