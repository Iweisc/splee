"use client";

import { Languages } from "lucide-react";
import { useSWRConfig } from "swr";
import { Field, Select } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { useAction } from "@/components/app/settings/use-action";
import { api } from "@/lib/api";
import { type CampaignLanguage, LANGUAGES } from "@/lib/languages";
import type { SegmentWithStats } from "@/lib/types";
import { MicroLabel } from "./states";

const NOTES: Record<"en" | "lead" | "fixed", string> = {
  lead: "Each lead is written to in the language set on them, else their country's, else their company website's, else English. Countries with more than one language, like Switzerland, go by the website.",
  en: "Every email, follow-up, text and LinkedIn message is written in English.",
  fixed: "Every email, follow-up, text and LinkedIn message is written in this language, whatever the lead's country.",
};

/** The language the campaign writes in. Saved as soon as it's picked. Replies always answer in the language the lead wrote in. */
export function LanguageCard({ segment }: { segment: SegmentWithStats }) {
  const { mutate } = useSWRConfig();
  const { pending, run } = useAction();

  const save = (language: CampaignLanguage) =>
    run("language", () => mutate(`/segments/${segment.id}`, api<SegmentWithStats>(`/segments/${segment.id}`, { method: "PATCH", body: { language } }), { revalidate: false }));

  return (
    <Card className="flex flex-col gap-4 p-5">
      <div className="min-w-0">
        <MicroLabel className="flex items-center gap-1.5">
          <Languages className="size-3.5" /> Language
        </MicroLabel>
        <p className="mt-1 text-sm text-pretty text-muted-foreground">
          Applies to what&apos;s written from now on. Drafted replies answer in the language the lead wrote in.
        </p>
      </div>
      <Field label="Write in" hint={NOTES[segment.language === "en" || segment.language === "lead" ? segment.language : "fixed"]}>
        <Select value={segment.language} onChange={(e) => save(e.target.value as CampaignLanguage)} disabled={pending === "language"} className="w-full sm:w-72">
          <option value="lead">Each lead&apos;s language (default)</option>
          <option value="en">English</option>
          <optgroup label="One language for everyone">
            {LANGUAGES.filter((l) => l.code !== "en").map((l) => (
              <option key={l.code} value={l.code}>
                {l.name}
              </option>
            ))}
          </optgroup>
        </Select>
      </Field>
    </Card>
  );
}
