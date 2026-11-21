import type { Metadata, Viewport } from "next";
import { GeistSans } from "geist/font/sans";
import { GeistMono } from "geist/font/mono";
import "./globals.css";

export const metadata: Metadata = {
  title: "Splee — 24/7 AI agent that finds clients while you sleep",
  description:
    "Our AI agents research the market, sharpen your ICP, find high-intent prospects, write personalized emails, and book demos.",
  // The manifest is app/manifest.ts. iOS reads this and app/apple-icon.png instead when Splee is added to the home screen.
  appleWebApp: { capable: true, title: "Splee", statusBarStyle: "default" },
};

export const viewport: Viewport = { themeColor: "#f6f5f3" };

export default function RootLayout({ children }: LayoutProps<"/">) {
  return (
    <html lang="en" className={`${GeistSans.variable} ${GeistMono.variable} h-full antialiased`}>
      <body className="min-h-full flex flex-col">{children}</body>
    </html>
  );
}
