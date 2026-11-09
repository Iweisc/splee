"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import { ArrowRight, Mail, UserPlus } from "lucide-react";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { Switch } from "@/components/ui/form";
import { Badge } from "@/components/ui/misc";

export default function ChannelsPage() {
  const { projectId } = useParams<{ projectId: string }>();
  return (
    <ProjectSettingsPage title="Channels" description="Where the agent reaches your leads.">
      {(s, update) => (
        <>
          <Section
            title={
              <span className="flex items-center gap-2">
                <Mail className="size-4" /> Email <Badge tone="green">Required</Badge>
              </span>
            }
            description="Personal cold emails and follow-ups, sent from your own mailboxes and paid per sent email."
            actions={<Switch checked disabled onChange={() => {}} label="Email is always on" />}
          >
            <Link href="/app/domains" className="inline-flex items-center gap-1 text-sm font-medium hover:underline">
              Manage mailboxes <ArrowRight className="size-3.5" />
            </Link>
          </Section>

          <Section
            title={
              <span className="flex items-center gap-2">
                <UserPlus className="size-4" /> LinkedIn
              </span>
            }
            description="When a lead with a LinkedIn profile gets their first email, the agent writes them a connection note and two follow-up messages. You send these from your own profile. Splee never logs into LinkedIn."
            actions={
              <Switch
                checked={s.channels.linkedin}
                onChange={(linkedin) => update({ channels: { ...s.channels, linkedin } })}
                label="LinkedIn sequences"
              />
            }
          >
            {s.channels.linkedin && (
              <Link href={`/app/p/${projectId}/linkedin-sequence`} className="inline-flex items-center gap-1 text-sm font-medium hover:underline">
                Open LinkedIn sequences <ArrowRight className="size-3.5" />
              </Link>
            )}
          </Section>
        </>
      )}
    </ProjectSettingsPage>
  );
}
