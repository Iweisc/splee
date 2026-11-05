import type { Metadata } from "next";
import { InviteCard } from "./invite-card";

export const metadata: Metadata = { title: "Join your team · Splee" };

export default async function InvitePage({ params }: PageProps<"/invite/[token]">) {
  const { token } = await params;
  return <InviteCard token={token} />;
}
