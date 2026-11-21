import type { Metadata } from "next";
import { safeNext } from "../safe-next";
import { SigninForm } from "./signin-form";

export const metadata: Metadata = { title: "Sign in · Splee" };

export default async function SigninPage({ searchParams }: PageProps<"/signin">) {
  const { next, sso_error } = await searchParams;
  return <SigninForm next={safeNext(next)} ssoError={typeof sso_error === "string" ? sso_error : null} />;
}
