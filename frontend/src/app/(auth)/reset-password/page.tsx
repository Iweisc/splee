import type { Metadata } from "next";
import { ResetForm } from "./reset-form";

// The token is in this page's address until the form drops it, so it's never sent on as a Referer.
export const metadata: Metadata = { title: "Choose a new password · Splee", referrer: "no-referrer" };

export default async function ResetPasswordPage({ searchParams }: PageProps<"/reset-password">) {
  const { token } = await searchParams;
  return <ResetForm token={typeof token === "string" && token ? token : null} />;
}
