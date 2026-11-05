"use client";

import { useRouter } from "next/navigation";
import { useEffect } from "react";
import useSWR from "swr";
import { Spinner } from "@/components/ui/misc";
import { LoadError } from "@/components/app/work/states";
import type { ProjectSummary } from "@/lib/types";
import { fetcher } from "@/hooks/use-me";

export default function AppHome() {
  const router = useRouter();
  const { data: projects, error, mutate } = useSWR<ProjectSummary[]>("/projects", fetcher);

  useEffect(() => {
    if (!projects) return;
    if (projects.length === 0) {
      router.replace("/app/onboarding");
      return;
    }
    const last = localStorage.getItem("splee:last-project");
    const target = projects.find((p) => p.id === last) ?? projects[0]!;
    router.replace(`/app/p/${target.id}`);
  }, [projects, router]);

  if (error) return <LoadError error={error} retry={() => mutate()} className="min-h-[60vh]" />;
  return (
    <div className="flex min-h-[60vh] items-center justify-center">
      <Spinner className="size-6" />
    </div>
  );
}
