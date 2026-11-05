import type { Metadata } from "next";
import { AppShell } from "@/components/app/shell";

// No title from Next here: AppShell renders a <title> per page, and a Next title would win after every client navigation.
export const metadata: Metadata = { title: null };

export default function AppLayout({ children }: LayoutProps<"/app">) {
  return <AppShell>{children}</AppShell>;
}
