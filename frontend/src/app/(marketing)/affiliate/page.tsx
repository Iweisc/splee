import type { Metadata } from "next";
import { ArrowRight } from "lucide-react";
import { LinkButton } from "@/components/ui/button";

export const metadata: Metadata = { title: "Affiliate program · Splee" };

// Mirrors billing::REFERRAL_BONUS_MICROS and billing::REFERRAL_QUALIFYING_SPEND_MICROS in the backend.
const BONUS = 30;
const QUALIFYING_SPEND = 10;

const steps = [
  { title: "Share your link", body: "Every workspace has a referral link on its Referral page." },
  { title: "They sign up", body: "The team you refer gets the usual $30 in free credits." },
  { title: `You get $${BONUS}`, body: `Once they've spent $${QUALIFYING_SPEND} on emails, $${BONUS} in credits lands in your balance.` },
];

export default function Affiliate() {
  return (
    <>
      <section className="mx-auto w-full max-w-[880px] px-5 pt-24 text-center">
        <h1 className="text-[clamp(2.5rem,7vw,3.75rem)] leading-none font-normal tracking-[-0.03em]">
          Refer a team, get <span className="text-brand-600">${BONUS}</span>
        </h1>
        <p className="mx-auto mt-6 max-w-[560px] text-lg text-muted-foreground">
          Know someone who sells to businesses? Send them your link. When their team spends ${QUALIFYING_SPEND} on emails, you get $
          {BONUS} in credits.
        </p>
        <LinkButton href="/app/referral" size="lg" className="mt-10 rounded-lg px-8">
          Get your referral link
          <ArrowRight className="size-4" />
        </LinkButton>
      </section>
      <section className="mx-auto grid w-full max-w-[1240px] grid-cols-1 gap-6 px-5 pt-24 pb-32 md:grid-cols-3">
        {steps.map((s, i) => (
          <div key={s.title} className="rounded-2xl bg-card p-6 shadow-plaque">
            <span className="flex size-7 items-center justify-center rounded-full bg-primary text-sm font-semibold text-primary-foreground">{i + 1}</span>
            <h2 className="mt-6 text-2xl font-medium tracking-tight">{s.title}</h2>
            <p className="mt-2 text-muted-foreground">{s.body}</p>
          </div>
        ))}
      </section>
    </>
  );
}
