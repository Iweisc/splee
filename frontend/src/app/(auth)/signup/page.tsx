import type { Metadata } from "next";
import { safeNext } from "../safe-next";
import { SignupForm } from "./signup-form";

export const metadata: Metadata = { title: "Create your account · Splee" };

const one = (v: string | string[] | undefined) => (typeof v === "string" && v ? v : null);

export default async function SignupPage({ searchParams }: PageProps<"/signup">) {
  const sp = await searchParams;
  const claim = one(sp.claim);
  const token = one(sp.token);
  return (
    <SignupForm
      next={safeNext(sp.next)}
      claim={claim && token ? { project_id: claim, token } : null}
      referralCode={one(sp.ref)}
      inviteToken={one(sp.invite)}
      inviteEmail={one(sp.email)}
    />
  );
}
