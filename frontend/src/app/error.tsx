"use client";

import { useEffect } from "react";
import { Button, LinkButton } from "@/components/ui/button";
import { Logo } from "@/components/ui/logo";
import { Card } from "@/components/ui/misc";

// Next 16 passes `retry`, not the older `reset`.
export default function Error({ error, retry }: { error: unknown; retry: () => void }) {
  useEffect(() => {
    console.error(error);
  }, [error]);

  return (
    <main className="flex min-h-screen flex-col items-center justify-center gap-6 px-4">
      <Logo href="/" className="text-lg" />
      <Card className="w-full max-w-[400px] p-8 text-center">
        <h1 className="text-xl font-medium">Something went wrong</h1>
        <p className="mt-2 text-sm text-muted-foreground">This page hit an error. Trying again usually fixes it.</p>
        <div className="mt-6 flex justify-center gap-2">
          <LinkButton href="/" variant="outline">
            Home
          </LinkButton>
          <Button onClick={retry}>Try again</Button>
        </div>
      </Card>
    </main>
  );
}
