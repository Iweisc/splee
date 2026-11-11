import type { Metadata } from "next";
import { Logo } from "@/components/ui/logo";
import { ReportUnsubscribeCard } from "./unsubscribe-card";

export const metadata: Metadata = { title: "Unsubscribe from a report · Splee", robots: { index: false } };

export default async function ReportUnsubscribePage({ params }: PageProps<"/report-unsubscribe/[token]">) {
  const { token } = await params;
  return (
    <main className="flex flex-1 flex-col items-center justify-center gap-8 px-5 py-16">
      <Logo />
      <ReportUnsubscribeCard token={token} />
    </main>
  );
}
