"use client";

import { useParams } from "next/navigation";
import { useState } from "react";
import useSWR from "swr";
import { MessageSquareText, Plus, Trash2 } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LoadError, Section, SectionsSkeleton, SettingsColumn } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Field, Input, Textarea } from "@/components/ui/form";
import { Card, EmptyState } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { SavedReply } from "@/lib/types";

const EMPTY = { title: "", body: "" };

export default function SavedRepliesPage() {
  const { projectId } = useParams<{ projectId: string }>();
  const key = `/projects/${projectId}/saved-replies`;
  const { data, error, mutate } = useSWR<SavedReply[]>(key, fetcher);
  const { pending, run } = useAction();
  const [draft, setDraft] = useState(EMPTY);
  const ready = draft.title.trim() && draft.body.trim();

  const remove = (r: SavedReply) =>
    run(r.id, async () => {
      await api(`/saved-replies/${r.id}`, { method: "DELETE" });
      await mutate();
    }, `Deleted ${r.title}`);

  const add = async () => {
    const ok = await run("add", async () => {
      await api(key, { body: draft });
      await mutate();
    }, "Reply saved");
    if (ok) setDraft(EMPTY);
  };

  return (
    <>
      <PageHeader
        title="Saved replies"
        description="Answers you give often, ready to drop into any reply from the inbox."
      />
      <PageBody>
        <SettingsColumn>
          {error ? (
            <LoadError error={error} what="saved replies" retry={() => mutate()} />
          ) : !data ? (
            <SectionsSkeleton />
          ) : data.length === 0 ? (
            <Card>
              <EmptyState icon={<MessageSquareText className="size-6" />} title="No saved replies yet">
                Add your pricing, a meeting link or a polite no below.
              </EmptyState>
            </Card>
          ) : (
            <Card className="divide-y divide-border">
              {data.map((r) => (
                <div key={r.id} className="flex items-start gap-4 px-5 py-4">
                  <div className="min-w-0 flex-1">
                    <p className="truncate font-medium">{r.title}</p>
                    <p className="mt-0.5 line-clamp-3 text-sm whitespace-pre-wrap text-muted-foreground">{r.body}</p>
                  </div>
                  <Button variant="ghost" size="icon" onClick={() => remove(r)} loading={pending === r.id} aria-label={`Delete ${r.title}`}>
                    <Trash2 className="size-4" />
                  </Button>
                </div>
              ))}
            </Card>
          )}

          {data && (
            <Section title="Add a reply" description="Write {first_name} or {company} and the inbox fills in the lead's.">
              <form
                className="grid gap-4"
                onSubmit={(e) => {
                  e.preventDefault();
                  if (ready) add();
                }}
              >
                <Field label="Name">
                  <Input value={draft.title} maxLength={80} onChange={(e) => setDraft({ ...draft, title: e.target.value })} placeholder="Pricing" />
                </Field>
                <Field label="Reply">
                  <Textarea
                    value={draft.body}
                    maxLength={5000}
                    onChange={(e) => setDraft({ ...draft, body: e.target.value })}
                    placeholder={"Hi {first_name},\n\nPlans start at $99 a month for the whole team at {company}."}
                    rows={5}
                  />
                </Field>
                <div className="flex justify-end">
                  <Button type="submit" disabled={!ready} loading={pending === "add"}>
                    <Plus className="size-4" /> Save reply
                  </Button>
                </div>
              </form>
            </Section>
          )}
        </SettingsColumn>
      </PageBody>
    </>
  );
}
