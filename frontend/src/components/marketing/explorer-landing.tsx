import Link from "next/link";
import { regions } from "@/content/regions";
import { cn } from "@/lib/utils";
import { Explorer } from "./explorer";

const facts = [
  { title: "Searched live", body: "The agent searches the web with Claude for every query, so results reflect what's online today." },
  { title: "A reason for each", body: "Every company comes with a short note on why it matches what you asked for." },
  { title: "Yours to export", body: "Download the list as CSV, or sign up and let the agent find the people and write to them." },
];

export function ExplorerLanding({
  title,
  intro,
  country,
  placeholder,
  activeRegion,
}: {
  title: React.ReactNode;
  intro: string;
  country?: string;
  placeholder?: string;
  activeRegion?: string;
}) {
  return (
    <div className="mx-auto w-full max-w-[1240px] px-5 pt-20 pb-32">
      <div className="mx-auto max-w-[760px] text-center">
        <h1 className="text-[clamp(2.25rem,6vw,3.25rem)] leading-tight font-normal tracking-[-0.03em]">{title}</h1>
        <p className="mt-5 text-lg text-muted-foreground">{intro}</p>
      </div>
      <div className="mx-auto mt-12 max-w-[960px]">
        <Explorer key={country} defaultCountry={country} placeholder={placeholder} />
      </div>
      <div className="mt-24 grid grid-cols-1 gap-6 md:grid-cols-3">
        {facts.map((f) => (
          <div key={f.title} className="rounded-2xl bg-card p-6 shadow-plaque">
            <h2 className="text-xl font-medium">{f.title}</h2>
            <p className="mt-2 text-muted-foreground">{f.body}</p>
          </div>
        ))}
      </div>
      <div className="mt-24">
        <h2 className="mb-6 text-center text-2xl font-normal">Search by region</h2>
        <div className="flex flex-wrap justify-center gap-2">
          {regions.map((r) => (
            <Link
              key={r.slug}
              href={`/b2b-database/locations/${r.slug}`}
              className={cn(
                "rounded-full px-4 py-1.5 text-sm transition-colors",
                r.slug === activeRegion ? "bg-primary text-primary-foreground" : "bg-accent hover:bg-black/[0.08]",
              )}
            >
              {r.name}
            </Link>
          ))}
        </div>
      </div>
    </div>
  );
}
