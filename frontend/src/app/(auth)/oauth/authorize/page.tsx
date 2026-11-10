import type { Metadata } from "next";
import { Consent } from "./consent";

export const metadata: Metadata = { title: "Connect an app · Splee" };

/** The OAuth authorization endpoint that ChatGPT, Claude.ai and other MCP clients send people to. See routes/oauth.rs. */
export default async function AuthorizePage({ searchParams }: PageProps<"/oauth/authorize">) {
  const sp = await searchParams;
  const params = Object.fromEntries(Object.entries(sp).filter((e): e is [string, string] => typeof e[1] === "string"));
  return <Consent params={params} />;
}
