const things = [
  {
    title: "Every lead researched live",
    body: "The agent reads each company's site and public pages with Claude before it writes a word",
    tilt: "md:-rotate-1",
  },
  {
    title: "Sends from your own mailboxes",
    body: "Connect any SMTP mailbox. Splee checks its MX, SPF and DMARC records before you send",
    tilt: "",
  },
  {
    title: "Yours to run",
    body: "Run it yourself with your own Claude account and mailboxes. No credits, no per-email fees",
    tilt: "md:rotate-1",
  },
];

export function ThreeThings() {
  return (
    <section className="mx-auto w-full max-w-[1240px] px-5 pt-40">
      <h2 className="mb-12 text-center text-4xl font-normal">Three things nobody else has</h2>
      <div className="grid grid-cols-1 gap-6 md:grid-cols-3">
        {things.map((t) => (
          <div key={t.title} className={`flex min-h-[210px] flex-col justify-between gap-8 rounded-2xl bg-card p-6 shadow-plaque ${t.tilt}`}>
            <h3 className="text-[34px] leading-tight font-medium tracking-tight">{t.title}</h3>
            <p className="text-muted-foreground">{t.body}</p>
          </div>
        ))}
      </div>
    </section>
  );
}
