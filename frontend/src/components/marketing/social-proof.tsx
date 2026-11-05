import { Avatar } from "@/components/ui/misc";
import { caseResults, testimonials, type Testimonial } from "@/content/social-proof";
import { LinkedinIcon } from "./linkedin-icon";

function Person({ name, avatarUrl, className }: { name: string; avatarUrl?: string; className?: string }) {
  if (!avatarUrl) return <Avatar name={name} className={className} />;
  // eslint-disable-next-line @next/next/no-img-element
  return <img src={avatarUrl} alt="" className={`${className} shrink-0 rounded-full object-cover`} />;
}

export function CaseResults() {
  if (caseResults.length === 0) return null;
  return (
    <section className="mx-auto w-full max-w-[1240px] px-5 pt-16">
      <h2 className="mb-12 text-center text-4xl font-normal">What our customers got out of it</h2>
      <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-4">
        {caseResults.map((c) => (
          <article key={c.name} className="flex flex-col gap-4 rounded-2xl bg-card p-[18px] shadow-plaque">
            <div className="flex items-center gap-2.5">
              <Person name={c.name} avatarUrl={c.avatarUrl} className="size-[43px]" />
              <div className="min-w-0">
                <p className="text-[15px] leading-tight font-semibold">{c.name}</p>
                <p className="text-[13px] text-muted-foreground">{c.company}</p>
              </div>
              {c.linkedinUrl && (
                <a href={c.linkedinUrl} target="_blank" rel="noreferrer" className="ml-auto opacity-50 transition-opacity hover:opacity-100" aria-label="LinkedIn">
                  <LinkedinIcon className="size-[18px]" />
                </a>
              )}
            </div>
            <blockquote className="flex-1 text-[15px] leading-relaxed">“{c.quote}”</blockquote>
            <div className="grid grid-cols-2 gap-2">
              <Metric value={String(c.hotLeads)} label="hot leads" />
              <Metric value={c.costPerLead} label="cost/lead" />
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}

function Metric({ value, label }: { value: string; label: string }) {
  return (
    <div className="flex min-h-[76px] flex-col justify-between rounded-lg bg-accent py-2.5 text-center">
      <span className="text-[22px] leading-tight font-semibold tracking-tight tabular-nums">{value}</span>
      <span className="text-[13px] text-muted-foreground">{label}</span>
    </div>
  );
}

export function Testimonials() {
  if (testimonials.length === 0) return null;
  const rows = [testimonials.filter((_, i) => i % 2 === 0), testimonials.filter((_, i) => i % 2 === 1)].filter((r) => r.length > 0);
  return (
    <section className="pt-40">
      <h2 className="mb-12 text-center text-4xl font-normal">What customers are saying</h2>
      <div className="flex flex-col gap-4">
        {rows.map((row, i) => (
          <MarqueeRow key={i} items={row} reverse={i === 1} />
        ))}
      </div>
    </section>
  );
}

function MarqueeRow({ items, reverse }: { items: Testimonial[]; reverse: boolean }) {
  // The list is rendered twice and shifted by -50%, so the loop has no visible seam.
  return (
    <div className="group overflow-hidden py-3.5 [mask-image:linear-gradient(90deg,transparent_0,#000_60px,#000_calc(100%-60px),transparent_100%)]">
      <div
        className={`flex w-max gap-4 group-hover:[animation-play-state:paused] motion-reduce:animate-none ${reverse ? "animate-marquee-reverse" : "animate-marquee"}`}
      >
        {[...items, ...items].map((t, i) => (
          <figure key={i} aria-hidden={i >= items.length} className="flex h-60 w-[440px] shrink-0 flex-col gap-3.5 rounded-2xl bg-card p-6 shadow-plaque">
            <blockquote className="flex-1 text-base leading-relaxed">“{t.quote}”</blockquote>
            <figcaption className="flex items-center gap-3">
              <Person name={t.name} avatarUrl={t.avatarUrl} className="size-[43px]" />
              <span>
                <span className="block font-medium">{t.name}</span>
                <span className="block text-muted-foreground">{t.company}</span>
              </span>
            </figcaption>
          </figure>
        ))}
      </div>
    </div>
  );
}
