"use client";

import { useRouter } from "next/navigation";
import { use, useState } from "react";
import { Sparkles } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { EMPTY_SEGMENT, SegmentFields, type SegmentDraft } from "@/components/app/work/segment-form";
import { toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Textarea } from "@/components/ui/form";
import { Card, Tabs } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { Segment } from "@/lib/types";

export default function NewCampaignPage({ params }: PageProps<"/app/p/[projectId]/new">) {
  const { projectId } = use(params);
  const router = useRouter();
  const [mode, setMode] = useState<"prompt" | "manual">("prompt");
  const [prompt, setPrompt] = useState("");
  const [draft, setDraft] = useState<SegmentDraft>(EMPTY_SEGMENT);
  const [saving, setSaving] = useState(false);

  const canSubmit = mode === "prompt" ? prompt.trim().length >= 10 : draft.name.trim().length > 0;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!canSubmit) return;
    setSaving(true);
    try {
      const segment = await api<Segment>(`/projects/${projectId}/segments`, {
        body: mode === "prompt" ? { prompt: prompt.trim() } : { ...draft, name: draft.name.trim() },
      });
      router.push(`/app/p/${projectId}/segments/${segment.id}`);
    } catch (err) {
      toastError(err);
      setSaving(false);
    }
  };

  return (
    <>
      <PageHeader title="New campaign" description="A campaign is one buyer segment. Describe it and the agent fills in targeting, pain points and angle." />
      <PageBody>
        <Card className="max-w-3xl p-6">
          <Tabs
            value={mode}
            onChange={setMode}
            options={[
              { value: "prompt", label: "Describe it" },
              { value: "manual", label: "Fill in fields" },
            ]}
            className="mb-5"
          />
          <form onSubmit={submit} className="flex flex-col gap-5">
            {mode === "prompt" ? (
              <Field label="Who do you want to reach?" hint="Plain words. Who they are, where, how big, and why they'd buy.">
                <Textarea
                  autoFocus
                  value={prompt}
                  onChange={(e) => setPrompt(e.target.value)}
                  className="min-h-32"
                  placeholder="Independent wedding planners in the US and UK who decorate venues with flowers and care about looks in photos."
                />
              </Field>
            ) : (
              <SegmentFields value={draft} onChange={setDraft} />
            )}
            <div className="flex justify-end gap-2">
              <Button type="button" variant="ghost" onClick={() => router.back()}>
                Cancel
              </Button>
              <Button type="submit" loading={saving} disabled={!canSubmit}>
                {mode === "prompt" && <Sparkles className="size-4" />}
                Create campaign
              </Button>
            </div>
          </form>
        </Card>
      </PageBody>
    </>
  );
}
