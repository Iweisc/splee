"use client";

import { useRouter } from "next/navigation";
import { useState } from "react";
import { useSWRConfig } from "swr";
import { ArrowRight } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input, Textarea } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { Project } from "@/lib/types";
import { isDomain, normalizeDomain } from "@/lib/utils";

export default function OnboardingPage() {
  const router = useRouter();
  const { mutate } = useSWRConfig();
  const [noWebsite, setNoWebsite] = useState(false);
  const [website, setWebsite] = useState("");
  const [description, setDescription] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const invalid = noWebsite ? description.trim().length < 20 : !isDomain(website);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (invalid) return;
    setSubmitting(true);
    try {
      const project = await api<Project>("/projects", {
        body: noWebsite ? { description: description.trim() } : { domain: normalizeDomain(website) },
      });
      // The run page looks the project up by domain in this list, so it has to include the new one first.
      await mutate("/projects");
      router.push(`/app/onboarding/${project.domain ?? `p-${project.id}`}`);
    } catch (err) {
      toastError(err);
      setSubmitting(false);
    }
  };

  return (
    <>
      <PageHeader title="New project" description="Splee researches your company, finds your buyers and drafts the first emails. It takes a few minutes." />
      <PageBody>
        <Card className="max-w-xl p-6">
          <form onSubmit={submit} className="flex flex-col gap-4">
            {noWebsite ? (
              <Field label="What do you sell, and to whom?" hint="A few sentences is enough. At least 20 characters.">
                <Textarea
                  autoFocus
                  value={description}
                  onChange={(e) => setDescription(e.target.value)}
                  placeholder="We make bookkeeping software for independent dental clinics in the US…"
                />
              </Field>
            ) : (
              <Field label="Your website" error={website && !isDomain(website) ? "Enter a domain like acme.com" : null}>
                <Input autoFocus value={website} onChange={(e) => setWebsite(e.target.value)} placeholder="acme.com" />
              </Field>
            )}
            {/* Where the two don't fit side by side, on the narrowest phones, Start research takes the full row on top. */}
            <div className="flex flex-wrap-reverse items-center justify-between gap-3">
              <button
                type="button"
                onClick={() => setNoWebsite((v) => !v)}
                className="text-sm whitespace-nowrap text-muted-foreground underline-offset-4 hover:underline"
              >
                {noWebsite ? "I have a website" : "I don't have a website"}
              </button>
              <Button type="submit" loading={submitting} disabled={invalid} className="max-sm:grow">
                Start research <ArrowRight className="size-4" />
              </Button>
            </div>
          </form>
        </Card>
      </PageBody>
    </>
  );
}
