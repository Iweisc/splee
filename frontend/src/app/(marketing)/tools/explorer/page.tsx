import type { Metadata } from "next";
import { ExplorerLanding } from "@/components/marketing/explorer-landing";

export const metadata: Metadata = { title: "Segments Explorer · Splee" };

export default function SegmentsExplorer() {
  return (
    <ExplorerLanding
      title="Segments Explorer"
      intro="Describe a segment in plain words and get a list of matching companies, researched live, with a reason for each."
    />
  );
}
