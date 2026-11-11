import type { Metadata } from "next";
import { SharedReportView } from "./shared-report";

export const metadata: Metadata = { title: "Outreach report", robots: { index: false } };

export default async function ReportPage({ params }: PageProps<"/report/[token]">) {
  const { token } = await params;
  return <SharedReportView token={token} />;
}
