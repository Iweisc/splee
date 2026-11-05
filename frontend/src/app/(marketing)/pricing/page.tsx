import type { Metadata } from "next";
import { PricingTabs } from "./pricing-tabs";

export const metadata: Metadata = { title: "Pricing · Splee" };

export default function Pricing() {
  return (
    <div className="mx-auto w-full max-w-[1240px] px-5 pt-8 pb-32">
      <h1 className="mb-6 text-3xl font-bold">Pricing</h1>
      <PricingTabs />
    </div>
  );
}
