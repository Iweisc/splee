import { Faq } from "@/components/marketing/faq";
import { FinalCta } from "@/components/marketing/final-cta";
import { Hero } from "@/components/marketing/hero";
import { Pipeline } from "@/components/marketing/pipeline";
import { PricingCard } from "@/components/marketing/pricing-card";
import { CaseResults, Testimonials } from "@/components/marketing/social-proof";
import { ThreeThings } from "@/components/marketing/three-things";

export default function Home() {
  return (
    <>
      <Hero />
      <CaseResults />
      <Testimonials />
      <Pipeline />
      <ThreeThings />
      <section id="pricing" className="scroll-mt-24 px-5 pt-40">
        <h2 className="mb-12 text-center text-4xl font-normal">Pay as you go with no subscription</h2>
        <PricingCard />
      </section>
      <Faq />
      <FinalCta />
    </>
  );
}
