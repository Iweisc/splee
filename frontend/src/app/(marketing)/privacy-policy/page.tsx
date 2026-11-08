import type { Metadata } from "next";
import { LegalPage } from "@/components/marketing/legal-page";

export const metadata: Metadata = { title: "Privacy Policy · Splee" };

export default function PrivacyPolicy() {
  return (
    <LegalPage
      title="Privacy Policy"
      sections={[
        {
          heading: "Account data",
          body: (
            <p>
              When you sign up we store your name, email address and a hashed password, plus the projects, campaigns and settings you
              create. Mailbox passwords are stored encrypted.
            </p>
          ),
        },
        {
          heading: "Lead data",
          body: (
            <p>
              The agent collects business contact details from public web sources: names, job titles, work email addresses, company
              information and public profile links. It&apos;s used only to research and contact those people on behalf of the workspace
              that found them.
            </p>
          ),
        },
        {
          heading: "Third parties",
          body: (
            <p>
              Research and writing run on Anthropic&apos;s Claude. Emails are sent through the
              mailboxes each user connects. Nothing is sold to anyone.
            </p>
          ),
        },
        {
          heading: "Your rights",
          body: (
            <p>
              You can ask the operator of this instance for a copy of your data, a correction or deletion. If you received an email,
              the unsubscribe link removes you from that workspace&apos;s outreach for good.
            </p>
          ),
        },
      ]}
    />
  );
}
