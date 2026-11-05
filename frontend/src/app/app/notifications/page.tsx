"use client";

import useSWR from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LoadError, Section, SectionsSkeleton, SettingsColumn } from "@/components/app/settings/layout";
import { toastError } from "@/components/app/settings/use-action";
import { Switch } from "@/components/ui/form";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { NotificationPrefs } from "@/lib/types";

type Pref = Exclude<keyof NotificationPrefs, "user_id">;

const PREFS: { key: Pref; title: string; description: string }[] = [
  { key: "hot_lead_email", title: "Hot leads", description: "An email when a lead replies with interest." },
  { key: "daily_digest", title: "Daily digest", description: "A daily summary of sends, replies and spend." },
  { key: "weekly_report", title: "Weekly report", description: "A weekly summary of results per campaign." },
  { key: "low_balance", title: "Low balance", description: "An email when your balance is running low, before sending stops." },
];

export default function NotificationsPage() {
  const { me } = useMe();
  const { data, error, mutate } = useSWR<NotificationPrefs>("/notification-preferences", fetcher);

  // Toggles save immediately, there is nothing to batch on this page.
  const toggle = async (key: Pref, value: boolean) => {
    if (!data) return;
    const next = { ...data, [key]: value };
    try {
      await mutate(api<NotificationPrefs>("/notification-preferences", { method: "PUT", body: { ...next } }), {
        optimisticData: next,
        rollbackOnError: true,
        revalidate: false,
      });
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <>
      <PageHeader title="Notifications" description={me ? `Emails Splee sends to you at ${me.user.email}.` : undefined} />
      <PageBody>
        <SettingsColumn>
          {error ? (
            <LoadError error={error} what="notification settings" />
          ) : !data ? (
            <SectionsSkeleton count={4} />
          ) : (
            PREFS.map((p) => (
              <Section
                key={p.key}
                title={p.title}
                description={p.description}
                actions={<Switch checked={data[p.key]} onChange={(v) => toggle(p.key, v)} label={p.title} />}
              />
            ))
          )}
        </SettingsColumn>
      </PageBody>
    </>
  );
}
