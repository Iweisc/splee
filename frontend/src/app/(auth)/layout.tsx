import { SupportChat } from "@/components/support-chat";
import { Logo } from "@/components/ui/logo";

export default function AuthLayout({ children }: LayoutProps<"/">) {
  return (
    <main className="flex flex-1 flex-col items-center justify-center gap-8 px-5 py-16">
      <Logo />
      {children}
      <SupportChat />
    </main>
  );
}
