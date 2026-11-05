export function LegalPage({ title, sections }: { title: string; sections: { heading: string; body: React.ReactNode }[] }) {
  return (
    <article className="mx-auto w-full max-w-[720px] px-5 pt-16 pb-32">
      <h1 className="text-4xl font-normal tracking-tight">{title}</h1>
      <p className="mt-4 text-muted-foreground">
        Splee is self-hosted software. The company or person running this instance is responsible for how it&apos;s used, and is who you
        should contact with any request.
      </p>
      <div className="mt-12 flex flex-col gap-10">
        {sections.map((s) => (
          <section key={s.heading}>
            <h2 className="mb-3 text-xl font-medium">{s.heading}</h2>
            <div className="flex flex-col gap-3 leading-relaxed text-muted-foreground">{s.body}</div>
          </section>
        ))}
      </div>
    </article>
  );
}
