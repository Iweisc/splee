import { Footer } from "@/components/marketing/footer";
import { Header } from "@/components/marketing/header";
import { SupportChat } from "@/components/support-chat";

export default function MarketingLayout({ children }: LayoutProps<"/">) {
  return (
    <>
      <Header />
      <main className="flex-1">{children}</main>
      <Footer />
      <SupportChat />
    </>
  );
}
