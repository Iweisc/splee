import { ArrowUpRight } from "lucide-react";
import { buttonClass } from "@/components/ui/button";
import { SOURCE_URL } from "@/lib/source";

const facts = [
  { title: "No credits or fees", body: "Splee costs nothing to run beyond your own Claude account and mailboxes." },
  { title: "Your data stays put", body: "Leads, emails and replies live in your own Postgres database." },
  { title: "AGPL-3.0", body: "Read the code, change it and run it however you like. Share your changes if you host it for others." },
];

export function OpenSource() {
  return (
    <section id="open-source" className="mx-auto w-full max-w-[1240px] scroll-mt-24 px-5 pt-40">
      <h2 className="mb-4 text-center text-4xl font-normal">Free and open source</h2>
      <p className="mx-auto mb-12 max-w-[560px] text-center text-lg text-muted-foreground">
        Self-host Splee on your laptop or a server. Every feature is included, with no paid plans.
      </p>
      <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
        {facts.map((f) => (
          <div key={f.title} className="rounded-2xl bg-card p-6 shadow-plaque">
            <h3 className="text-xl font-medium">{f.title}</h3>
            <p className="mt-2 text-muted-foreground">{f.body}</p>
          </div>
        ))}
      </div>
      <div className="mt-10 flex justify-center">
        <a href={SOURCE_URL} target="_blank" rel="noreferrer" className={buttonClass("outline", "lg", "rounded-lg")}>
          View the code on GitHub <ArrowUpRight className="size-4" />
        </a>
      </div>
    </section>
  );
}
