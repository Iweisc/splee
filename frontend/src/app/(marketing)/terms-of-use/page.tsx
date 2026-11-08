import type { Metadata } from "next";
import { LegalPage } from "@/components/marketing/legal-page";

export const metadata: Metadata = { title: "Terms of use · Splee" };

export default function TermsOfUse() {
  return (
    <LegalPage
      title="Terms of use"
      sections={[
        {
          heading: "The service",
          body: (
            <p>
              Splee researches companies on the public web, drafts emails and sends them from mailboxes you connect. Results depend on your
              offer and your market, and no number of replies or meetings is guaranteed.
            </p>
          ),
        },
        {
          heading: "Your responsibilities",
          body: (
            <>
              <p>You&apos;re responsible for the emails sent from your mailboxes, including their content and who receives them.</p>
              <p>
                Don&apos;t use Splee for spam, deceptive messages, or contacting people who asked not to be contacted. Follow the
                anti-spam and data protection laws that apply to you and your recipients.
              </p>
            </>
          ),
        },
        {
          heading: "Ending use",
          body: (
            <p>
              You can pause all campaigns or delete your organization at any time from the app. The operator may suspend accounts that
              break these terms.
            </p>
          ),
        },
      ]}
    />
  );
}
