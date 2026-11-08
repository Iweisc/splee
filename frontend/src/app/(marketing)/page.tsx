import { Faq } from "@/components/marketing/faq";
import { FinalCta } from "@/components/marketing/final-cta";
import { Hero } from "@/components/marketing/hero";
import { OpenSource } from "@/components/marketing/open-source";
import { Pipeline } from "@/components/marketing/pipeline";
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
      <OpenSource />
      <Faq />
      <FinalCta />
    </>
  );
}
