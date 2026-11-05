import type { Metadata } from "next";
import { ExplorerLanding } from "@/components/marketing/explorer-landing";

export const metadata: Metadata = { title: "B2B company search · Splee" };

export default function B2bDatabase() {
  return (
    <ExplorerLanding
      title="Find B2B companies anywhere"
      intro="Describe the companies you want to sell to. The agent searches the web live and returns a list with why each one fits."
    />
  );
}
