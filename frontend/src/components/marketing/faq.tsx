import Link from "next/link";
import { ChevronDown } from "lucide-react";

const questions: { q: string; a: React.ReactNode }[] = [
  {
    q: "How does Splee work?",
    a: "Give it your website. A Claude agent researches your company, your competitors and your likely buyers on the web, proposes campaigns, finds matching companies and decision makers, and writes each person a personal email. Once you connect a mailbox and start a campaign, it sends on your schedule, follows up, reads replies and answers interested leads with your booking link.",
  },
  {
    q: "Will it work for my business?",
    a: (
      <>
        It&apos;s built for companies that sell to other businesses. The quickest way to check your niche is to{" "}
        <Link href="/" className="underline underline-offset-2">
          enter your website at the top of this page
        </Link>
        . The free demo shows which companies and people the agent would contact and what it would write to them. It can&apos;t
        promise that they&apos;ll buy.
      </>
    ),
  },
  {
    q: "How much does Splee cost?",
    a: "Nothing. Splee is free and open source under the AGPL-3.0, and you run it on your own machine or server. Research and writing use your own Claude account, and emails go out through your own mailboxes, so those are the only costs.",
  },
  {
    q: "Which countries and languages can I target?",
    a: "Any country the agent can research on the public web. You can include or exclude countries in each campaign's targeting and choose the language the emails are written in.",
  },
  {
    q: "Whose mailboxes send the emails? Will they land in spam?",
    a: "Your own. You connect mailboxes over SMTP and IMAP, and Splee checks their MX, SPF and DMARC records. Each mailbox has a daily sending limit and every email includes an unsubscribe link. Inbox placement depends on your domain's reputation, so nobody can guarantee it.",
  },
  {
    q: "Where do the leads come from?",
    a: "Live web research. For each campaign the agent searches the web with Claude, reads company sites and public pages, and picks the people who fit. Splee doesn't have its own contact database. If you already have a list, import it as a CSV after you sign up.",
  },
  {
    q: "What results can I expect?",
    a: "That depends on your offer, your market and your mailboxes, and we don't guarantee any results. A hot lead is a reply that moves the deal forward, like \"send me pricing\". Your dashboard shows how each campaign performs, and campaigns that need too many emails per hot lead are paused automatically.",
  },
];

export function Faq() {
  return (
    <section className="mx-auto w-full max-w-[760px] px-5 pt-40">
      <h2 className="mb-12 text-center text-4xl font-normal">Common questions</h2>
      <div>
        {questions.map((item) => (
          <details key={item.q} className="group border-b border-border last:border-b-0">
            <summary className="flex cursor-pointer list-none items-center justify-between gap-4 py-[18px] font-medium [&::-webkit-details-marker]:hidden">
              {item.q}
              <ChevronDown className="size-4 shrink-0 text-muted-foreground transition-transform group-open:rotate-180" />
            </summary>
            <p className="pb-5 leading-relaxed text-muted-foreground">{item.a}</p>
          </details>
        ))}
      </div>
    </section>
  );
}
