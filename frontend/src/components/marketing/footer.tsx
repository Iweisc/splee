import Link from "next/link";
import { regions } from "@/content/regions";
import { SOURCE_URL } from "@/lib/source";

const tools = [
  { href: "/b2b-database", label: "B2B company search" },
  { href: "/gm-dataset", label: "Google Maps search" },
  { href: "/tools/explorer", label: "Segments Explorer" },
];

const resources = [
  { href: "/about", label: "About" },
  { href: "/pricing", label: "Pricing" },
  { href: "/affiliate", label: "Affiliate program" },
  { href: "/compliance-faq", label: "Compliance FAQ" },
];

const regionLinks = regions.map((r) => ({ href: `/b2b-database/locations/${r.slug}`, label: `Companies in ${r.name}` }));

function Column({ title, links }: { title?: string; links: { href: string; label: string }[] }) {
  return (
    <div>
      <p className="mb-4 h-6 font-medium text-muted-foreground">{title}</p>
      <ul className="flex flex-col gap-2">
        {links.map((l) => (
          <li key={l.href}>
            <Link href={l.href} className="text-muted-foreground transition-colors hover:text-foreground">
              {l.label}
            </Link>
          </li>
        ))}
      </ul>
    </div>
  );
}

export function Footer() {
  return (
    <footer className="border-t border-border">
      <div className="mx-auto w-full max-w-[1240px] px-5 pt-[88px] pb-12 md:px-9">
        <div className="grid grid-cols-1 gap-10 sm:grid-cols-2 lg:grid-cols-4 lg:gap-0">
          <Column title="Products and GTM Tools" links={tools} />
          <Column title="B2B Databases" links={regionLinks.slice(0, 5)} />
          <Column links={regionLinks.slice(5)} />
          <Column title="Resources" links={resources} />
        </div>
        <div className="mt-20 flex flex-col justify-between gap-3 text-muted-foreground sm:flex-row">
          <p>© {new Date().getFullYear()} Splee, a self-hosted outbound agent</p>
          <p>
            <a href={SOURCE_URL} target="_blank" rel="noreferrer" className="hover:text-foreground">
              Source code
            </a>
            <span className="mx-2">|</span>
            <Link href="/terms-of-use" className="hover:text-foreground">
              Terms of use
            </Link>
            <span className="mx-2">|</span>
            <Link href="/privacy-policy" className="hover:text-foreground">
              Privacy Policy
            </Link>
          </p>
        </div>
      </div>
    </footer>
  );
}
