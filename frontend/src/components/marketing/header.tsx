"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { Building2, ChevronDown, LayoutGrid, MapPin, Menu, Search, Send, X } from "lucide-react";
import { Logo } from "@/components/ui/logo";
import { useMe } from "@/hooks/use-me";
import { cn } from "@/lib/utils";

const products = [
  { href: "/", label: "Outreach Agent", description: "Finds clients and emails them for you", icon: Send },
  { href: "/b2b-database", label: "B2B Database", description: "Search companies anywhere, live", icon: Building2 },
  { href: "/gm-dataset", label: "Google Maps Dataset", description: "Local businesses by place and category", icon: MapPin },
  { href: "/tools/explorer", label: "Segments Explorer", description: "Describe a segment, get a company list", icon: Search },
];

const links = [
  { href: "/pricing", label: "Pricing" },
  { href: "/affiliate", label: "Affiliates" },
  { href: "/about", label: "About" },
];

const navLink = "px-4 py-3 text-base transition-colors duration-300 hover:text-foreground/70";

export function Header() {
  const { me } = useMe();
  const [productsOpen, setProductsOpen] = useState(false);
  const [mobileOpen, setMobileOpen] = useState(false);
  const productsRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!productsOpen) return;
    const close = (e: MouseEvent) => {
      if (!productsRef.current?.contains(e.target as Node)) setProductsOpen(false);
    };
    const escape = (e: KeyboardEvent) => e.key === "Escape" && setProductsOpen(false);
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", escape);
    };
  }, [productsOpen]);

  const account = me ? (
    <Link href="/app" className={cn(navLink, "whitespace-nowrap")}>
      Open app
    </Link>
  ) : (
    <Link href="/signin" className={cn(navLink, "whitespace-nowrap")}>
      Sign in
    </Link>
  );

  return (
    <header className="sticky top-0 z-40 bg-background">
      <div className="mx-auto flex h-20 w-full max-w-[1240px] items-center justify-between px-5">
        <Logo />
        <nav className="hidden items-center md:flex">
          <div ref={productsRef} className="relative">
            <button
              type="button"
              className={cn(navLink, "flex items-center gap-1")}
              aria-expanded={productsOpen}
              onClick={() => setProductsOpen((o) => !o)}
            >
              <LayoutGrid className="size-4" />
              Products
              <ChevronDown className={cn("size-4 transition-transform", productsOpen && "rotate-180")} />
            </button>
            {productsOpen && (
              <div className="absolute top-full left-1/2 w-80 -translate-x-1/2 pt-1">
                <div className="animate-fade-up rounded-2xl bg-card p-2 shadow-pop">
                  {products.map((p) => (
                    <Link
                      key={p.href}
                      href={p.href}
                      onClick={() => setProductsOpen(false)}
                      className="flex items-start gap-3 rounded-xl p-3 transition-colors hover:bg-accent"
                    >
                      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-accent">
                        <p.icon className="size-4" />
                      </span>
                      <span>
                        <span className="block text-sm font-medium">{p.label}</span>
                        <span className="block text-sm text-muted-foreground">{p.description}</span>
                      </span>
                    </Link>
                  ))}
                </div>
              </div>
            )}
          </div>
          {links.map((l) => (
            <Link key={l.href} href={l.href} className={navLink}>
              {l.label}
            </Link>
          ))}
          {account}
        </nav>
        <button type="button" className="-mr-2 p-2 md:hidden" aria-label="Menu" aria-expanded={mobileOpen} onClick={() => setMobileOpen((o) => !o)}>
          {mobileOpen ? <X className="size-6" /> : <Menu className="size-6" />}
        </button>
      </div>
      {mobileOpen && (
        <nav className="flex flex-col border-t border-border px-5 py-3 md:hidden" onClick={() => setMobileOpen(false)}>
          {[...products, ...links].map((l) => (
            <Link key={l.label} href={l.href} className="py-2.5 text-base">
              {l.label}
            </Link>
          ))}
          <Link href={me ? "/app" : "/signin"} className="py-2.5 text-base font-medium">
            {me ? "Open app" : "Sign in"}
          </Link>
        </nav>
      )}
    </header>
  );
}
