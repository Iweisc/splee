import type { Metadata } from "next";
import { Globe, Mail, Wallet } from "lucide-react";
import { FinalCta } from "@/components/marketing/final-cta";

export const metadata: Metadata = { title: "About · Splee" };

const principles = [
  {
    icon: Globe,
    title: "Research happens live",
    body: "Every company and person is researched on the public web with Claude when you run a campaign. There's no stale database behind it.",
  },
  {
    icon: Mail,
    title: "You own the sending",
    body: "Emails go out from your mailboxes, under your name, within the daily budget you set. You can pause everything at any time.",
  },
  {
    icon: Wallet,
    title: "You pay for what's sent",
    body: "$0.04 per email and nothing else. Research and writing aren't billed, and there's no subscription to cancel.",
  },
];

export default function About() {
  return (
    <>
      <section className="mx-auto w-full max-w-[880px] px-5 pt-24 text-center">
        <h1 className="text-[clamp(2.5rem,7vw,3.75rem)] leading-none font-normal tracking-[-0.03em]">AI that works, not talks</h1>
        <p className="mx-auto mt-8 max-w-[640px] text-lg text-muted-foreground">
          Splee does the outbound work instead of asking you questions. Give it your website and it finds the companies likely to buy,
          the people to talk to, and writes to each of them. It&apos;s software you run yourself, not an agency, and it doesn&apos;t come
          with a sales team or a contact database.
        </p>
      </section>
      <section className="mx-auto w-full max-w-[1240px] px-5 pt-32">
        <h2 className="mb-12 text-center text-4xl font-normal">How it works</h2>
        <div className="grid grid-cols-1 gap-6 md:grid-cols-3">
          {principles.map((p) => (
            <div key={p.title} className="rounded-2xl bg-card p-6 shadow-plaque">
              <span className="flex size-10 items-center justify-center rounded-xl bg-accent">
                <p.icon className="size-5" />
              </span>
              <h3 className="mt-6 text-2xl font-medium tracking-tight">{p.title}</h3>
              <p className="mt-3 text-muted-foreground">{p.body}</p>
            </div>
          ))}
        </div>
      </section>
      <FinalCta />
    </>
  );
}
