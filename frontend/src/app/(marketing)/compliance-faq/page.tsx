import type { Metadata } from "next";
import { LegalPage } from "@/components/marketing/legal-page";

export const metadata: Metadata = { title: "Compliance FAQ · Splee" };

export default function ComplianceFaq() {
  return (
    <LegalPage
      title="Compliance FAQ"
      sections={[
        {
          heading: "Where does contact data come from?",
          body: (
            <p>
              From public web sources. The agent searches the web and reads company websites and public pages to find businesses and the
              people who work there. Users can also import their own lists. Splee doesn&apos;t buy or resell contact databases.
            </p>
          ),
        },
        {
          heading: "What is the legal basis under GDPR?",
          body: (
            <p>
              Cold B2B outreach usually relies on legitimate interest: a business contacting a professional about something relevant to
              their role. The operator of this instance and each sending user are responsible for checking that this applies to their
              outreach, keeping messages relevant, and honouring objections straight away.
            </p>
          ),
        },
        {
          heading: "How do people opt out?",
          body: (
            <p>
              Every email has an unsubscribe link and a List-Unsubscribe header. The link opens a page with a single Unsubscribe button, and
              mail apps that read the header unsubscribe in one click. Either one stops all further emails from that workspace and adds the
              address to its suppression list. Replies asking to stop are detected and suppressed the same way.
            </p>
          ),
        },
        {
          heading: "Who sends the emails?",
          body: (
            <p>
              Users send from their own mailboxes, under their own name and domain. Splee checks the mailbox&apos;s DNS records and enforces
              a daily sending limit for each mailbox, but the sender is responsible for the content and for complying with local rules.
            </p>
          ),
        },
        {
          heading: "How do I get my data removed?",
          body: <p>Use the unsubscribe link in any email you received, or contact the operator of this Splee instance and ask for deletion.</p>,
        },
      ]}
    />
  );
}
