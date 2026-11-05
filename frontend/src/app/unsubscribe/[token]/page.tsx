import type { Metadata } from "next";
import { Logo } from "@/components/ui/logo";
import { UnsubscribeCard } from "./unsubscribe-card";

export const metadata: Metadata = { title: "Unsubscribe · Splee", robots: { index: false } };

export default async function UnsubscribePage({ params }: PageProps<"/unsubscribe/[token]">) {
  const { token } = await params;
  return (
    <main className="flex flex-1 flex-col items-center justify-center gap-8 px-5 py-16">
      <Logo />
      <UnsubscribeCard token={token} />
    </main>
  );
}
