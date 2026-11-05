import { LinkButton } from "@/components/ui/button";
import { Logo } from "@/components/ui/logo";
import { Card } from "@/components/ui/misc";

export default function NotFound() {
  return (
    <main className="flex min-h-screen flex-col items-center justify-center gap-6 px-4">
      <Logo href="/" className="text-lg" />
      <Card className="w-full max-w-[400px] p-8 text-center">
        <h1 className="text-xl font-medium">Page not found</h1>
        <p className="mt-2 text-sm text-muted-foreground">The link may be old or mistyped.</p>
        <div className="mt-6 flex justify-center gap-2">
          <LinkButton href="/" variant="outline">
            Home
          </LinkButton>
          <LinkButton href="/app">Open the app</LinkButton>
        </div>
      </Card>
    </main>
  );
}
