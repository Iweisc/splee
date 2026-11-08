"use client";

import { useEffect, useRef } from "react";
import { Check } from "lucide-react";
import { Avatar } from "@/components/ui/misc";
import { cn } from "@/lib/utils";

// Every illustration uses the same fictional business so the six steps read as one story.
// The .example domains have no favicons, so the chips draw a letter instead of using <Favicon>.
const competitors = ["silkhouse.example", "everbloom.example", "faux-flora.example", "stemcraft.example", "petalpro.example", "evergreen-co.example"];

const segments = [
  { name: "Event designers", fit: 93 },
  { name: "Wedding planners", fit: 89 },
  { name: "Hotel banquet teams", fit: 84 },
  { name: "Restaurant groups", fit: 78 },
  { name: "Set decorators", fit: 71 },
];

const people = [
  { name: "Nora Lindqvist", title: "Owner", company: "Fern & Field Events", email: "nora@fernandfield.example" },
  { name: "Theo Marchetti", title: "Lead Designer", company: "Gilded Table Co.", email: "theo@gildedtable.example" },
  { name: "Amara Osei", title: "Founder", company: "Studio Bloomcraft", email: "amara@bloomcraft.example" },
  { name: "Jonah Reyes", title: "Event Producer", company: "Northlight Weddings", email: "jonah@northlight.example" },
];

const results = [
  { name: "Event designers", status: "Scaling", emails: "44" },
  { name: "Wedding planners", status: "Scaling", emails: "48" },
  { name: "Hotel banquet teams", status: "Working", emails: "53" },
  { name: "Restaurant groups", status: "Paused", emails: "160" },
  { name: "Set decorators", status: "Paused", emails: "146" },
];

const STICKY_TOP = 140;
const STICKY_STEP = 15;
const SCALE_STEP = 0.04;

export function Pipeline() {
  const wraps = useRef<(HTMLDivElement | null)[]>([]);

  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    let frame = 0;
    // A card shrinks a little for every later card that slides over it, so the stack reads as depth.
    const update = () => {
      frame = 0;
      const els = wraps.current;
      const depth = els.map(() => 0);
      els.forEach((el, j) => {
        if (!el || j === 0) return;
        const distance = el.getBoundingClientRect().top - (STICKY_TOP + STICKY_STEP * j);
        const covered = Math.min(1, Math.max(0, 1 - distance / el.offsetHeight));
        for (let i = 0; i < j; i++) depth[i]! += covered;
      });
      els.forEach((el, i) => {
        if (el) el.style.transform = `scale(${1 - SCALE_STEP * depth[i]!})`;
      });
    };
    const onScroll = () => {
      if (!frame) frame = requestAnimationFrame(update);
    };
    update();
    window.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("resize", onScroll);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("scroll", onScroll);
      window.removeEventListener("resize", onScroll);
    };
  }, []);

  const cards = [
    { title: "Learns what you sell", body: <SiteCard /> },
    { title: "Figures out who buys it", body: <FitTable /> },
    { title: "Finds those exact people", body: <PeopleGrid /> },
    { title: "Writes each a personal email", body: <EmailPreview /> },
    { title: "Handles replies and books meetings", body: <Chat />, fits: true },
    { title: "Learns what works and doubles down", body: <ResultsTable /> },
  ];

  return (
    <section className="mx-auto w-full max-w-[1240px] px-5 pt-40">
      <h2 className="sticky top-20 z-0 pb-5 text-center text-4xl font-normal">It runs the entire pipeline</h2>
      <div className="relative mt-6 pb-16">
        {cards.map((c, i) => (
          <div
            key={c.title}
            ref={(el) => {
              wraps.current[i] = el;
            }}
            className="mb-8 origin-top rounded-2xl transition-transform duration-100 ease-out last:mb-0 md:sticky"
            style={{ top: STICKY_TOP + STICKY_STEP * i, zIndex: i + 1 }}
          >
            <div className="relative w-full overflow-hidden rounded-2xl bg-card shadow-plaque">
              <div className="flex min-h-[260px] flex-col md:flex-row">
                <div className="flex max-w-[520px] flex-col p-4 md:flex-[55%] md:p-8">
                  <h3 className="text-[clamp(24px,4.5vw,36px)] leading-tight font-normal">{c.title}</h3>
                </div>
                {c.fits ? (
                  <div className="flex flex-1 items-center p-4 pt-0 md:p-8">{c.body}</div>
                ) : (
                  <div className="relative h-[300px] flex-1 overflow-hidden [mask-image:linear-gradient(#000_0_calc(100%-64px),transparent_100%)] md:h-auto">
                    <div className="absolute inset-x-4 top-0 md:inset-x-8 md:top-8">{c.body}</div>
                  </div>
                )}
              </div>
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

const panel = "overflow-hidden rounded-xl bg-accent";

function SiteCard() {
  return (
    <div className={cn(panel, "p-4")}>
      <p className="font-medium">petalworks.example</p>
      <p className="mt-2 text-muted-foreground">Wholesale silk flowers and greenery for event pros, shipped by the box from one warehouse</p>
      <div className="mt-4 border-t border-border pt-4">
        <p className="flex items-center gap-2 text-sm text-muted-foreground">
          <span className="size-1.5 animate-pulse rounded-full bg-brand-500" />
          Splee agent is studying competitors…
        </p>
        <div className="mt-3 flex flex-wrap gap-2">
          {competitors.map((d) => (
            <span key={d} className="inline-flex items-center gap-1.5 rounded-lg bg-card px-2.5 py-1.5 text-sm shadow-plaque">
              <span className="flex size-3.5 items-center justify-center rounded-full border border-border text-[9px] text-muted-foreground">
                {d[0]!.toUpperCase()}
              </span>
              {d}
            </span>
          ))}
        </div>
      </div>
    </div>
  );
}

function FitTable() {
  return (
    <div className="overflow-hidden rounded-xl shadow-plaque">
      <div className="flex justify-between bg-accent px-4 py-2.5 text-xs font-medium tracking-wide text-muted-foreground uppercase">
        <span>Clients</span>
        <span>Fit score</span>
      </div>
      {segments.map((s, i) => (
        <div key={s.name} className={cn("flex justify-between border-t border-border px-4 py-2", i > 2 && "text-muted-foreground")}>
          <span className={cn(i <= 2 && "font-medium")}>{s.name}</span>
          <span className="text-muted-foreground tabular-nums">{s.fit}%</span>
        </div>
      ))}
    </div>
  );
}

function PeopleGrid() {
  return (
    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
      {people.map((p) => (
        <div key={p.email} className={cn(panel, "p-4")}>
          <div className="flex items-center gap-2.5">
            <Avatar name={p.name} className="bg-card" />
            <div className="min-w-0">
              <p className="truncate font-medium">{p.name}</p>
              <p className="truncate text-sm text-muted-foreground">
                {p.title} · {p.company}
              </p>
            </div>
          </div>
          <p className="mt-3 truncate text-sm text-muted-foreground">{p.email}</p>
        </div>
      ))}
    </div>
  );
}

function EmailPreview() {
  return (
    <div className="overflow-hidden rounded-xl shadow-plaque">
      <div className="flex gap-3 border-b border-border px-4 py-3">
        <span className="text-muted-foreground">Subject</span>
        <span className="font-medium">Petalworks x Gilded Table Co.</span>
      </div>
      <div className="space-y-3 px-4 py-4 leading-relaxed text-muted-foreground">
        <p>Hi Theo,</p>
        <p>
          Saw <span className="rounded bg-brand-50 px-1 text-brand-800">Gilded Table builds full ceiling installs for hotel galas</span>.
          Fresh stems at that scale get expensive and wilt under stage lights. Petalworks ships silk by the box, so one order carries a
          whole season of events.
        </p>
        <p>Want a sample box to compare against your next fresh order?</p>
      </div>
    </div>
  );
}

function Chat() {
  return (
    <div className="flex w-full flex-col gap-3">
      <p className="self-start rounded-2xl bg-accent px-3 py-2.5">Do these hold up under stage lighting?</p>
      <p className="self-start rounded-2xl bg-accent px-3 py-2.5">Looks good. Can you do Tuesday 2pm?</p>
      <div className="self-end rounded-2xl bg-accent px-3 py-2.5">
        <p>Tuesday 2pm works, sending the invite now.</p>
        <p className="mt-2 inline-flex items-center gap-2 rounded-lg bg-card/70 px-2.5 py-1.5 font-medium">
          <Check className="size-4 text-brand-600" />
          Demo booked · Tue 14:00
        </p>
      </div>
    </div>
  );
}

function ResultsTable() {
  return (
    <div className="overflow-hidden rounded-xl shadow-plaque">
      <div className="grid grid-cols-[1.4fr_1fr_auto] bg-accent px-4 py-2.5 text-xs font-medium tracking-wide text-muted-foreground uppercase">
        <span>Campaign</span>
        <span>Status</span>
        <span>Emails/lead</span>
      </div>
      {results.map((r) => {
        const paused = r.status === "Paused";
        return (
          <div key={r.name} className={cn("grid grid-cols-[1.4fr_1fr_auto] border-t border-border px-4 py-2", paused && "text-muted-foreground")}>
            <span className={cn(!paused && "font-medium")}>{r.name}</span>
            <span className="flex items-center gap-2 text-muted-foreground">
              {r.status}
              {!paused && <span className="size-1.5 bg-brand-300" />}
            </span>
            <span className="text-right text-muted-foreground tabular-nums">{r.emails}</span>
          </div>
        );
      })}
    </div>
  );
}
