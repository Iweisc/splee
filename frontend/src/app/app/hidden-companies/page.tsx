"use client";

import Link from "next/link";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { SettingsColumn } from "@/components/app/settings/layout";
import { DedupLists } from "@/components/app/work/dedup-lists";

export default function HiddenCompaniesPage() {
  return (
    <>
      <PageHeader
        title="Hidden companies"
        description={
          <>
            Add your CRM&apos;s accounts, current customers or a competitor&apos;s clients, and they stop showing up in company searches,
            downloads, saved searches and lookalikes. This only hides search results. Splee can still email them, so use{" "}
            <Link href="/app/dedup" className="underline underline-offset-2 hover:text-foreground">
              Do-not-contact
            </Link>{" "}
            to stop that.
          </>
        }
      />
      <PageBody>
        <SettingsColumn>
          <DedupLists />
        </SettingsColumn>
      </PageBody>
    </>
  );
}
