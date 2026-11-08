"use client";

import { useState } from "react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { Section, SettingsColumn } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";

export default function AccountPage() {
  const { me } = useMe();
  return (
    <>
      <PageHeader title="Account" description={me ? `Signed in as ${me.user.email}.` : undefined} />
      <PageBody>
        <SettingsColumn>
          <ChangePassword />
        </SettingsColumn>
      </PageBody>
    </>
  );
}

function ChangePassword() {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [repeat, setRepeat] = useState("");
  const { pending, run } = useAction();
  // There's no password reset by email, so a typo in the new password would lock you out. Hence the second field.
  // Held back until the second one is as long as the first, so it doesn't flash while you're still typing.
  const mismatch = repeat.length >= next.length && repeat !== next ? "The two new passwords don't match" : null;

  return (
    <Section title="Password" description="Changing it signs you out everywhere else, on other browsers and devices.">
      <form
        className="grid gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          void run(
            "password",
            async () => {
              await api("/auth/password", { body: { current_password: current, new_password: next } });
              setCurrent("");
              setNext("");
              setRepeat("");
            },
            "Password changed",
          );
        }}
      >
        <Field label="Current password">
          <Input type="password" autoComplete="current-password" required value={current} onChange={(e) => setCurrent(e.target.value)} />
        </Field>
        <Field label="New password" hint="At least 8 characters.">
          <Input type="password" autoComplete="new-password" required minLength={8} value={next} onChange={(e) => setNext(e.target.value)} />
        </Field>
        <Field label="New password again" error={mismatch}>
          <Input type="password" autoComplete="new-password" required value={repeat} onChange={(e) => setRepeat(e.target.value)} />
        </Field>
        <div>
          <Button type="submit" disabled={!current || next.length < 8 || repeat !== next} loading={pending === "password"}>
            Change password
          </Button>
        </div>
      </form>
    </Section>
  );
}
