"use client";

import { useState } from "react";
import { UserPlus } from "lucide-react";
import { Section } from "@/components/app/settings/layout";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import type { ProjectSettings } from "@/lib/types";
import { LastFailure, useConnection, submit } from "./shared";

/** The token is write-only, like a CRM token. Connecting checks it with Unipile and finds the LinkedIn account on it. */
export function UnipileSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [dsn, setDsn] = useState("");
  const [token, setToken] = useState("");
  const { busy, call } = useConnection(projectId, "unipile");
  const connect = async () => {
    if (await call({ dsn, token }, "Connected to Unipile")) setToken("");
  };
  const disconnect = () => {
    if (confirm("Disconnect Unipile? Due LinkedIn steps will wait for you to send them, and approved LinkedIn posts will wait too.")) call(undefined, "Unipile disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <UserPlus className="size-4" /> LinkedIn sending
        </span>
      }
      description="Sends each due LinkedIn step, and the LinkedIn posts and comments you approve on the Social page, from your own account through Unipile. It also finds decision makers at your target companies."
      actions={
        settings.unipile_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.unipile_connected ? (
        <p className="text-sm text-pretty">
          Sending as {settings.unipile_account_name || "your LinkedIn account"}. Each due step goes out in your sending hours, up to 25 a day, and
          messages wait until they accept the connect note. A reply on LinkedIn stops their sequence and emails. Approved posts and comments go
          out one at a time every ten minutes, up to 5 a day. Finding people searches LinkedIn for decision makers, up to 100 search pages a day.
        </p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4 sm:flex-row">
          <Field label="DSN" className="sm:w-56" hint="On your Unipile dashboard.">
            <Input placeholder="api8.unipile.com:13851" autoComplete="off" value={dsn} onChange={(e) => setDsn(e.target.value.trim())} />
          </Field>
          <Field label="Access token" className="flex-1" hint="Connect your LinkedIn account in Unipile, then generate a token under Access Tokens.">
            <div className="flex gap-2">
              <Input aria-label="Access token" type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!dsn || !token}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={settings.unipile_error} at={settings.unipile_failed_at} what="send" />
    </Section>
  );
}
