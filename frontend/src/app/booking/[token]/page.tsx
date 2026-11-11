import type { Metadata } from "next";
import { ManageBooking } from "./manage-booking";

export const metadata: Metadata = { title: "Your call", robots: { index: false } };

export default async function ManageBookingPage({ params, searchParams }: PageProps<"/booking/[token]">) {
  const [{ token }, query] = await Promise.all([params, searchParams]);
  return <ManageBooking token={token} cancel={query.cancel === "1"} />;
}
