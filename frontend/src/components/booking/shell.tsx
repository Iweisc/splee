import { CircleAlert, Clock, MapPin, Video } from "lucide-react";
import { Card } from "@/components/ui/misc";
import { LogoMark } from "@/components/ui/logo";
import type { PublicBookingPage } from "@/lib/types";

/** The frame of the public booking pages: who the call is with on the left (on top on phones), the step on the right. */
export function BookingShell({ page, children }: { page?: PublicBookingPage; children: React.ReactNode }) {
  return (
    <main className="flex flex-1 flex-col items-center bg-muted px-3 py-6 sm:justify-center sm:px-5 sm:py-12">
      <Card className="grid w-full max-w-[860px] overflow-hidden md:grid-cols-[280px_minmax(0,1fr)]">
        {page && (
          <aside className="border-b border-border p-6 md:border-r md:border-b-0">
            <p className="text-sm text-muted-foreground">{page.host}</p>
            <h1 className="mt-1 text-2xl font-semibold text-balance">{page.name}</h1>
            <ul className="mt-4 flex flex-col gap-2 text-sm text-muted-foreground">
              <li className="flex items-center gap-2">
                <Clock className="size-4 shrink-0" />
                {page.duration_minutes} minutes
              </li>
              {page.location && (
                <li className="flex items-start gap-2">
                  {page.location === "Video call" ? <Video className="mt-0.5 size-4 shrink-0" /> : <MapPin className="mt-0.5 size-4 shrink-0" />}
                  <span className="min-w-0 break-words">{page.location}</span>
                </li>
              )}
            </ul>
          </aside>
        )}
        <section className="min-w-0 p-6">{children}</section>
      </Card>
      <p className="mt-6 flex items-center gap-1.5 text-xs text-muted-foreground">
        <LogoMark className="size-3.5" /> Booking by Splee
      </p>
    </main>
  );
}

/** A slug or token that matches nothing. */
export function Missing() {
  return (
    <BookingShell>
      <div className="flex min-h-64 flex-col items-center justify-center gap-2 text-center text-balance">
        <CircleAlert className="size-6 text-danger" />
        <h1 className="text-xl font-medium">This booking link doesn&apos;t work</h1>
        <p className="text-sm text-muted-foreground">It may have changed. Ask the person who sent it for a new one.</p>
      </div>
    </BookingShell>
  );
}
